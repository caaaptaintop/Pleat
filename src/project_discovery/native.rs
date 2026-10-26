//! Bounded reads of native project registrations, never conversation bodies.
use std::{fs::File, io::Read, path::Path};

use anyhow::{Result, anyhow};
use serde_json::Value;

use super::{AgentKind, ProjectEntry, path_identity, project_name};

const MAX_INDEX_BYTES: u64 = 16 * 1024 * 1024;

pub(super) fn json_index(path: &Path) -> Result<Value> {
    let file = File::open(path).map_err(|_| anyhow!("Agent 项目索引暂不可读"))?;
    let mut bytes = Vec::new();
    file.take(MAX_INDEX_BYTES + 1).read_to_end(&mut bytes)
        .map_err(|_| anyhow!("Agent 项目索引暂不可读"))?;
    if bytes.len() as u64 > MAX_INDEX_BYTES {
        return Err(anyhow!("Agent 项目索引超过读取上限"));
    }
    serde_json::from_slice(&bytes).map_err(|_| anyhow!("Agent 项目索引格式无效"))
}

pub(super) fn project(path: &str, agent: AgentKind) -> Option<ProjectEntry> {
    let path = Path::new(path);
    if !path.is_absolute() || !path.is_dir() { return None; }
    let path = path_identity(path);
    Some(ProjectEntry { name: project_name(&path), path, agents: vec![agent],
        proma_workspaces: Vec::new(), native_workspaces: Vec::new(), archive_states: Vec::new(), archived: false, hidden: false, retained: false })
}

// Official Claude directory documentation defines `projects` as per-project
// state keyed by its path. Other keys may contain credentials and are ignored.
fn claude_projects(index: &Path) -> Result<Vec<ProjectEntry>> {
    let value = json_index(index)?;
    let Some(projects) = value.get("projects") else { return Ok(Vec::new()); };
    let projects = projects.as_object().ok_or_else(|| anyhow!("Claude Code 项目索引格式无效"))?;
    Ok(projects.keys().filter_map(|path| project(path, AgentKind::ClaudeCode)).collect())
}

pub(super) fn refresh(home: &Path, previous: &[ProjectEntry]) -> Vec<ProjectEntry> {
    let mut entries = merge_workbuddy_projects(super::workspaces::workbuddy_projects(home), previous);
    // JsonlLocalAgentStore is caller-configured; never infer its location.
    let sdk = std::env::var_os("KITTER_CURSOR_SDK_STORE")
        .map(|store| super::cursor_sdk::projects_at(Path::new(&store)));
    entries.extend(merge_cursor_catalogs(super::vscode::cursor_projects(home), sdk, previous));
    let claude = std::env::var_os("CLAUDE_CONFIG_DIR").map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.to_path_buf()).join(".claude.json");
    let mut catalogs = vec![(AgentKind::ClaudeCode, claude_projects(&claude)),
        (AgentKind::Grok, super::grok::grok_projects(home)),
        (AgentKind::Droid, super::droid::droid_projects(home)),
        (AgentKind::Trae, super::editor_native::trae_projects(home)),
        (AgentKind::Antigravity, super::editor_native::antigravity_projects(home)),
        (AgentKind::Copilot, super::copilot::copilot_projects(home))];
    catalogs.push((AgentKind::DimAgent, super::dimagent::projects(home)));
    catalogs.push((AgentKind::Amp, super::amp::projects(home)));
    catalogs.extend(super::cli::catalogs(home));
    catalogs.extend(super::runtime_agents::catalogs(home));
    for (agent, result) in catalogs {
        // An already observed Hermes projects.db disappearing is a source
        // failure, not an empty or newly archived native project catalog.
        let result = if agent == AgentKind::Hermes && crate::agents::hermes_home(home).ok().is_some_and(|profile| {
            let source = profile.join("projects.db");
            (!source.is_file() && previous.iter().flat_map(|p| &p.native_workspaces)
                .any(|w| w.agent == AgentKind::Hermes && w.path == path_identity(&source)))
                || (!source.exists() && !profile.join("state.db").is_file() && previous.iter()
                    .any(|p| p.agents.contains(&AgentKind::Hermes) && !p.native_workspaces.iter().any(|w| w.agent == AgentKind::Hermes)))
        }) { Err(anyhow!("Hermes 原生项目来源不可读，状态未知")) } else { result };
        match result {
            Ok(projects) => entries.extend(projects.into_iter().map(|entry| retain_known_archive(entry, previous, agent))),
            Err(_) => entries.extend(retain_failed_source(previous, agent)),
        }
    }
    entries
}

fn merge_cursor_catalogs(ide: Result<Vec<ProjectEntry>>, sdk: Option<Result<Vec<ProjectEntry>>>, previous: &[ProjectEntry]) -> Vec<ProjectEntry> {
    let ide_unavailable = ide.is_err();
    let mut current = Vec::new();
    let mut unavailable = false;
    for result in std::iter::once(ide).chain(sdk) {
        match result { Ok(rows) => current.extend(rows), Err(_) => unavailable = true }
    }
    // Apply previous state once, after merging current observations. Otherwise
    // an unknown/failed SDK source could reintroduce yesterday's active state
    // alongside an explicit IDE archive observation from this refresh.
    let empty = std::collections::BTreeSet::new();
    let mut merged = super::all_projects(&[], &current, &empty, &empty).into_iter()
        .map(|entry| retain_known_archive(entry, previous, AgentKind::Cursor)).collect::<Vec<_>>();
    if ide_unavailable {
        // SDK cwd metadata cannot prove that an unreadable IDE workspace was
        // removed. Keep only Cursor's prior workspace relations for that cwd.
        for entry in &mut merged {
            if let Some(old) = previous.iter().find(|old| old.path == entry.path && old.agents.contains(&AgentKind::Cursor)) {
                for workspace in old.native_workspaces.iter().filter(|workspace| workspace.agent == AgentKind::Cursor) {
                    if !entry.native_workspaces.iter().any(|current| current.id() == workspace.id()) {
                        entry.native_workspaces.push(workspace.clone());
                    }
                }
            }
        }
    }
    if unavailable {
        let seen = merged.iter().map(|entry| path_identity(&entry.path)).collect::<std::collections::BTreeSet<_>>();
        merged.extend(retain_failed_source(previous, AgentKind::Cursor).into_iter()
            .filter(|entry| !seen.contains(&path_identity(&entry.path))));
    }
    merged
}

fn merge_workbuddy_projects(result: Result<super::workspaces::WorkBuddyDiscovery>, previous: &[ProjectEntry]) -> Vec<ProjectEntry> {
    use super::workspaces::{WorkBuddyCompleteness, WorkBuddyDiscovery};
    match result {
        Err(_) => retain_failed_source(previous, AgentKind::WorkBuddy),
        Ok(WorkBuddyDiscovery { projects, completeness: WorkBuddyCompleteness::CatalogOnlySessionScopeUnknown
            | WorkBuddyCompleteness::CatalogOnlySessionsUnreadable }) => {
            // Even an empty catalog is not an empty session-derived source.
            // Keep only WorkBuddy's prior ownership, metadata and archive state;
            // a fresh catalog observation cannot revive another Agent or prove
            // an old session/project active, deleted, hidden or archived.
            let mut entries = retain_failed_source(previous, AgentKind::WorkBuddy);
            let mut seen = entries.iter().map(|entry| path_identity(&entry.path))
                .collect::<std::collections::BTreeSet<_>>();
            entries.extend(projects.into_iter().filter(|entry| seen.insert(path_identity(&entry.path))));
            entries
        }
        Ok(WorkBuddyDiscovery { projects, completeness: WorkBuddyCompleteness::Complete }) => projects.into_iter()
            .map(|entry| retain_known_archive(entry, previous, AgentKind::WorkBuddy)).collect(),
    }
}

fn retain_known_archive(mut entry: ProjectEntry, previous: &[ProjectEntry], agent: AgentKind) -> ProjectEntry {
    let current = entry.archive_states.iter().find(|state| state.agent == agent).and_then(|state| state.archived);
    if current.is_none() {
        let known = previous.iter().find(|old| old.path == entry.path && old.agents.contains(&agent))
            .and_then(|old| old.archive_states.iter().find(|state| state.agent == agent).and_then(|state| state.archived)
                .or_else(|| (old.archive_states.is_empty() && old.archived).then_some(true)));
        if let Some(archived) = known { entry.record_archive(agent, archived); }
    }
    for workspace in &mut entry.native_workspaces {
        if workspace.archived.is_none() {
            workspace.archived = previous.iter().flat_map(|old| &old.native_workspaces)
                .find(|old| old.id() == workspace.id()).and_then(|old| old.archived);
        }
    }
    entry
}

fn retain_failed_source(previous: &[ProjectEntry], agent: AgentKind) -> Vec<ProjectEntry> {
    previous.iter().filter(|p| p.agents.contains(&agent)).cloned().map(|mut entry| {
        // A merged row's other Agents must not be resurrected when only this
        // source failed. Preserve known archival state; failure cannot infer it.
        entry.retain_source(agent);
        entry
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::workspaces::{self, WorkBuddyCompleteness, WorkBuddyDiscovery};

    fn workbuddy_catalog(root: &Path, paths: &[&Path]) -> Result<WorkBuddyDiscovery> {
        let connection = rusqlite::Connection::open(root.join("workbuddy.db")).unwrap();
        connection.execute_batch("CREATE TABLE workspaces(path TEXT PRIMARY KEY, last_opened_at INTEGER NOT NULL);").unwrap();
        for path in paths {
            connection.execute("INSERT INTO workspaces VALUES(?1,1)", [path.to_str().unwrap()]).unwrap();
        }
        drop(connection);
        workspaces::workbuddy_projects_at(root, None)
    }

    #[test]
    fn workbuddy_partial_refresh_adds_catalog_and_preserves_only_its_own_previous_source() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let registered = root.join("registered"); let session = root.join("previous-session");
        std::fs::create_dir(&registered).unwrap(); std::fs::create_dir(&session).unwrap();
        let mut old = project(session.to_str().unwrap(), AgentKind::WorkBuddy).unwrap();
        old.agents.extend([AgentKind::Copilot, AgentKind::Proma]);
        old.proma_workspaces.push(root.join("other-workspace"));
        old.record_archive(AgentKind::WorkBuddy, true);
        old.record_archive(AgentKind::Copilot, false);
        old.record_archive(AgentKind::Proma, false);
        let own = super::super::NativeWorkspace {
            agent: AgentKind::WorkBuddy, source_id: Some("fixture-workbuddy".into()), display_name: None,
            archived: Some(true), path: root.join("workbuddy.db"),
            project_roots: std::sync::Arc::new(vec![session.clone()]),
        };
        old.native_workspaces.push(own.clone());
        old.native_workspaces.push(super::super::NativeWorkspace { agent: AgentKind::Copilot, ..own.clone() });
        let mut other_only = project(registered.to_str().unwrap(), AgentKind::Copilot).unwrap();
        other_only.record_archive(AgentKind::Copilot, true);
        let previous = vec![old, other_only];
        let result = workbuddy_catalog(&root, &[&registered]);
        assert_eq!(result.as_ref().unwrap().completeness, WorkBuddyCompleteness::CatalogOnlySessionScopeUnknown);
        let entries = merge_workbuddy_projects(result, &previous);
        assert_eq!(entries.len(), 2);
        let retained = entries.iter().find(|entry| entry.path == session).unwrap();
        assert_eq!(retained.agents, [AgentKind::WorkBuddy]);
        assert!(retained.archived);
        assert!(retained.proma_workspaces.is_empty());
        assert_eq!(retained.native_workspaces, [own]);
        assert_eq!(retained.archive_states, [super::super::NativeArchiveState {
            agent: AgentKind::WorkBuddy, archived: Some(true),
        }]);
        let added = entries.iter().find(|entry| entry.path == registered).unwrap();
        assert_eq!(added.agents, [AgentKind::WorkBuddy]);
        assert!(!added.archived && added.archive_states.is_empty());
        assert!(added.native_workspaces.is_empty());
    }

    #[test]
    fn workbuddy_partial_empty_catalog_preserves_previous_but_complete_empty_can_remove_it() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let previous = vec![project(root.to_str().unwrap(), AgentKind::WorkBuddy).unwrap()];
        let discovery = workbuddy_catalog(&root, &[]).unwrap();
        assert!(discovery.projects.is_empty());
        assert_eq!(discovery.completeness, WorkBuddyCompleteness::CatalogOnlySessionScopeUnknown);
        let retained = merge_workbuddy_projects(Ok(discovery), &previous);
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].agents, [AgentKind::WorkBuddy]);
        assert!(!retained[0].archived);
        assert!(retained[0].archive_states.iter().all(|state| state.archived.is_none()));
        let unreadable_sessions = merge_workbuddy_projects(Ok(WorkBuddyDiscovery {
            projects: Vec::new(), completeness: WorkBuddyCompleteness::CatalogOnlySessionsUnreadable,
        }), &previous);
        assert_eq!(unreadable_sessions.len(), 1);
        assert_eq!(unreadable_sessions[0].agents, [AgentKind::WorkBuddy]);
        assert!(!unreadable_sessions[0].archived);
        assert!(merge_workbuddy_projects(Ok(WorkBuddyDiscovery {
            projects: Vec::new(), completeness: WorkBuddyCompleteness::Complete,
        }), &previous).is_empty());
    }

    #[test]
    fn workbuddy_partial_catalog_match_preserves_prior_visibility_and_archive_state() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let mut old = project(root.to_str().unwrap(), AgentKind::WorkBuddy).unwrap();
        old.agents.push(AgentKind::Copilot);
        old.record_archive(AgentKind::WorkBuddy, true); old.record_archive(AgentKind::Copilot, false);
        old.hidden = true; old.retained = true;
        let entries = merge_workbuddy_projects(workbuddy_catalog(&root, &[&root]), &[old]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].agents, [AgentKind::WorkBuddy]);
        assert!(entries[0].archived && entries[0].hidden && entries[0].retained);
        let merged = super::super::all_projects(&[], &entries, &std::collections::BTreeSet::new(), &std::collections::BTreeSet::new());
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].agents, [AgentKind::WorkBuddy]);
        assert!(merged[0].archived, "catalog presence is not a native active/archive observation");
    }

    #[test]
    fn workbuddy_failed_catalog_preserves_previous_without_restoring_other_agents() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let mut old = project(root.to_str().unwrap(), AgentKind::WorkBuddy).unwrap();
        old.agents.push(AgentKind::Copilot);
        old.record_archive(AgentKind::WorkBuddy, true); old.record_archive(AgentKind::Copilot, false);
        for corrupt in [false, true] {
            if corrupt { std::fs::write(root.join("workbuddy.db"), "fixture-corrupt-private-body").unwrap(); }
            let result = workspaces::workbuddy_projects_at(&root, None);
            assert!(result.is_err());
            let entries = merge_workbuddy_projects(result, &[old.clone()]);
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].agents, [AgentKind::WorkBuddy]);
            assert!(entries[0].archived);
        }
    }

    #[test]
    fn unknown_refresh_keeps_its_own_known_archive_but_explicit_active_wins() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap();
        let unknown = project(path.to_str().unwrap(), AgentKind::Cursor).unwrap();
        let mut previous = unknown.clone(); previous.record_archive(AgentKind::Cursor, true);
        previous.agents.push(AgentKind::Proma); previous.record_archive(AgentKind::Proma, false);
        let retained = retain_known_archive(unknown.clone(), &[previous.clone()], AgentKind::Cursor);
        assert!(retained.archived);
        assert_eq!(retained.agents, [AgentKind::Cursor]);
        let mut active = unknown.clone(); active.record_archive(AgentKind::Cursor, false);
        assert!(!retain_known_archive(active, &[previous.clone()], AgentKind::Cursor).archived);
        let other = project(path.to_str().unwrap(), AgentKind::Trae).unwrap();
        assert!(!retain_known_archive(other, &[previous], AgentKind::Trae).archived);
    }

    #[test]
    fn cursor_current_ide_archive_wins_over_unknown_or_failed_sdk() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap();
        let unknown = project(path.to_str().unwrap(), AgentKind::Cursor).unwrap();
        let mut old = unknown.clone(); old.record_archive(AgentKind::Cursor, false);
        let mut current = unknown.clone(); current.record_archive(AgentKind::Cursor, true);
        for sdk in [Ok(vec![unknown.clone()]), Err(anyhow!("fixture unreadable"))] {
            let rows = merge_cursor_catalogs(Ok(vec![current.clone()]), Some(sdk), &[old.clone()]);
            assert_eq!(rows.len(), 1);
            assert!(rows[0].archived);
            assert_eq!(rows[0].archive_states[0].archived, Some(true));
        }
        let mut active = unknown.clone(); active.record_archive(AgentKind::Cursor, false);
        let rows = merge_cursor_catalogs(Ok(vec![active]), Some(Err(anyhow!("fixture unreadable"))), &[current]);
        assert!(!rows[0].archived);
    }

    #[test]
    fn cursor_failed_source_retains_only_unobserved_cursor_ownership() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap();
        let mut old = project(path.to_str().unwrap(), AgentKind::Cursor).unwrap();
        old.agents.push(AgentKind::Proma);
        old.record_archive(AgentKind::Cursor, true); old.record_archive(AgentKind::Proma, false);
        let rows = merge_cursor_catalogs(Ok(Vec::new()), Some(Err(anyhow!("fixture unreadable"))), &[old]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].agents, [AgentKind::Cursor]);
        assert!(rows[0].archived);
    }

    #[test]
    fn cursor_sdk_only_unknown_does_not_infer_project_archive() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap();
        let unknown = project(path.to_str().unwrap(), AgentKind::Cursor).unwrap();
        let rows = merge_cursor_catalogs(Err(anyhow!("fixture IDE missing")), Some(Ok(vec![unknown])), &[]);
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].archived);
        assert!(rows[0].archive_states.iter().all(|state| state.archived.is_none()));
    }

    #[test]
    fn cursor_ide_failure_keeps_workspace_membership_without_reviving_other_agent() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap();
        let unknown = project(path.to_str().unwrap(), AgentKind::Cursor).unwrap();
        let mut old = unknown.clone(); old.agents.push(AgentKind::Proma);
        for agent in [AgentKind::Cursor, AgentKind::Proma] {
            old.native_workspaces.push(super::super::NativeWorkspace { agent, source_id: None,
                display_name: None, archived: Some(true), path: path.join(format!("{}.code-workspace", agent.id())),
                project_roots: std::sync::Arc::new(vec![path.clone()]) });
        }
        let rows = merge_cursor_catalogs(Err(anyhow!("fixture IDE missing")), Some(Ok(vec![unknown.clone()])), &[old.clone()]);
        assert_eq!(rows[0].agents, [AgentKind::Cursor]);
        assert_eq!(rows[0].native_workspaces, [old.native_workspaces[0].clone()]);
        // A readable IDE observation can remove its old workspace relation.
        let rows = merge_cursor_catalogs(Ok(vec![unknown]), Some(Err(anyhow!("fixture SDK missing"))), &[old]);
        assert!(rows[0].native_workspaces.is_empty());
    }

    #[test]
    fn native_project_keys_ignore_unrelated_values_and_retain_on_failed_refresh() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let cwd = root.join("real-project");
        std::fs::create_dir(&cwd).unwrap();
        let index = root.join(".claude.json");
        std::fs::write(&index, serde_json::to_vec(&serde_json::json!({
            "projects": {cwd.to_string_lossy(): {"lastSessionId":"not-a-path"},
                "relative":{}, root.join("missing").to_string_lossy():{}},
            "unrelated": {"cwd":root}, "secret":"must-never-be-reported"
        })).unwrap()).unwrap();
        let mut previous = refresh(&root, &[]);
        assert_eq!(previous.len(), 1);
        assert_eq!(previous[0].path, cwd);
        previous[0].agents.push(AgentKind::Codex);
        previous[0].archived = true;
        let hermes_only = ProjectEntry { agents: vec![AgentKind::Hermes], ..previous[0].clone() };
        let retained = retain_failed_source(&[hermes_only], AgentKind::Hermes);
        assert_eq!(retained[0].agents, [AgentKind::Hermes]);
        assert!(retained[0].archived, "failed refresh must not revive a Hermes-archived project");
        assert!(retained[0].proma_workspaces.is_empty());
        std::fs::write(&index, "must-never-be-reported").unwrap();
        assert!(!claude_projects(&index).unwrap_err().to_string().contains("must-never-be-reported"));
        let retained = refresh(&root, &previous);
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].agents, [AgentKind::ClaudeCode]);
        assert!(retained[0].archived);
        std::fs::write(&index, "{\"projects\":{}}").unwrap();
        assert!(refresh(&root, &previous).is_empty());
    }

    #[test]
    fn native_index_read_limit_is_enforced() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("oversized.json");
        File::create(&path).unwrap().set_len(MAX_INDEX_BYTES + 1).unwrap();
        assert!(json_index(&path).unwrap_err().to_string().contains("读取上限"));
    }
}
