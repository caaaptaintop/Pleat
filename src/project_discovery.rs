//! Read project catalogs without scanning conversations or changing Agent data.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Result, anyhow};
use serde::Serialize;
use serde_json::Value;

use crate::effective_skills::AgentKind;

mod native;
mod workspaces;
mod cli;
mod runtime_agents;
mod sqlite_vfs;
mod metadata_worker;
mod grok;
mod droid;
mod copilot;
mod vscode;
mod cursor_sdk;
mod codex_projects;
mod codex_config;
mod codex_sources;
#[cfg(windows)]
mod codex_windows;
mod editor_native;
mod dimagent;
mod amp;
pub(crate) use runtime_agents::openclaw_config_at;

/// Internal metadata subprocess entry point used by the CLI and desktop.
#[doc(hidden)]
pub fn run_metadata_worker() -> Option<i32> { metadata_worker::entry() }

/// Native workspace identity and its member project roots stay separate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NativeWorkspace {
    pub agent: AgentKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    pub path: PathBuf,
    // Membership is represented by the containing ProjectEntry rows in JSON.
    // Share this internally to avoid quadratic copies for large workspaces.
    #[serde(skip_serializing)]
    pub project_roots: Arc<Vec<PathBuf>>,
}

impl NativeWorkspace {
    pub fn id(&self) -> String {
        if let Some(id) = &self.source_id {
            use sha2::{Digest, Sha256};
            let mut key = Sha256::new();
            key.update(path_identity(&self.path).as_os_str().as_encoded_bytes());
            key.update([0]); key.update(id.as_bytes());
            format!("native:{}:{:x}", self.agent.id(), key.finalize())
        } else { format!("native:{}:{}", self.agent.id(), path_identity(&self.path).display()) }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NativeArchiveState {
    pub agent: AgentKind,
    pub archived: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProjectEntry {
    pub path: PathBuf,
    pub name: String,
    /// Agents associated by a native catalog or explicit project installation.
    pub agents: Vec<AgentKind>,
    /// Native Skill roots remain separate from the Finder display identity.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proma_workspaces: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub native_workspaces: Vec<NativeWorkspace>,
    /// None means the source has no verified native archive observation.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub archive_states: Vec<NativeArchiveState>,
    /// True only when every known catalog entry explicitly reports archival.
    pub archived: bool,
    pub hidden: bool,
    pub retained: bool,
}

impl ProjectEntry {
    pub fn is_active(&self) -> bool {
        !self.hidden && (!self.archived || self.retained)
    }

    fn retain_source(&mut self, agent: AgentKind) {
        self.normalize_archive_states();
        self.agents = vec![agent];
        if agent != AgentKind::Proma { self.proma_workspaces.clear(); }
        self.native_workspaces.retain(|workspace| workspace.agent == agent);
        if !self.archive_states.is_empty() {
            self.archive_states.retain(|state| state.agent == agent);
            self.archived = !self.archive_states.is_empty() && self.archive_states.iter().all(|state| state.archived == Some(true));
        }
    }

    fn normalize_archive_states(&mut self) {
        let legacy_archived = self.archive_states.is_empty() && self.archived;
        for agent in &self.agents {
            if !self.archive_states.iter().any(|state| state.agent == *agent) {
                self.archive_states.push(NativeArchiveState { agent: *agent, archived: legacy_archived.then_some(true) });
            }
        }
        self.archived = !self.archive_states.is_empty() && self.archive_states.iter().all(|state| state.archived == Some(true));
    }

    pub(crate) fn record_archive(&mut self, agent: AgentKind, archived: bool) {
        self.normalize_archive_states();
        if let Some(state) = self.archive_states.iter_mut().find(|state| state.agent == agent) {
            state.archived = Some(state.archived.unwrap_or(true) && archived);
        } else { self.archive_states.push(NativeArchiveState { agent, archived: Some(archived) }); }
        self.archived = self.archive_states.iter().all(|state| state.archived == Some(true));
    }

    pub(crate) fn merge_archival(&mut self, other: &Self) {
        self.normalize_archive_states();
        for agent in &other.agents {
            let state = other.archive_states.iter().find(|state| state.agent == *agent)
                .map(|state| state.archived).unwrap_or_else(|| (other.archive_states.is_empty() && other.archived).then_some(true));
            if let Some(archived) = state { self.record_archive(*agent, archived); }
            else if !self.archive_states.iter().any(|state| state.agent == *agent) {
                self.archive_states.push(NativeArchiveState { agent: *agent, archived: None });
            }
        }
        self.archived = !self.archive_states.is_empty() && self.archive_states.iter().all(|state| state.archived == Some(true));
    }

    /// Project-local directories this Agent can read. Proma uses its native
    /// workspaces rather than the business directory shown in the project list.
    pub fn skill_directories(&self, agent: crate::agents::AgentIconInfo) -> Vec<PathBuf> {
        use crate::{InstallTarget, agents::installation_root};

        if agent.global_only {
            return Vec::new();
        }
        let mut roots = Vec::new();
        for target in agent.install_targets {
            if *target == InstallTarget::Proma {
                for workspace in &self.proma_workspaces {
                    roots.push(workspace.join("skills"));
                }
            } else {
                roots.extend(installation_root(&self.path, *target));
            }
        }
        if agent.shared_agents {
            roots.extend(installation_root(&self.path, InstallTarget::Universal));
        }
        let mut seen = BTreeSet::new();
        roots.retain(|root| seen.insert(path_identity(root)));
        roots
    }
}

pub fn path_identity(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

pub fn codex_home() -> Option<PathBuf> { codex_home_selection().home }

struct CodexHomeSelection { home: Option<PathBuf>, inspector: bool }

fn codex_home_selection() -> CodexHomeSelection {
    codex_catalog_home_from(std::env::var_os("KITTER_CODEX_PROJECTS_HOME"),
        std::env::var("CODEX_HOME").ok(), dirs::home_dir())
}

fn codex_catalog_home_from(
    inspector: Option<std::ffi::OsString>, native: Option<String>, os_home: Option<PathBuf>,
) -> CodexHomeSelection {
    codex_catalog_home_resolving(inspector, native, os_home, |path| {
        if !path.metadata()?.is_dir() {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
        }
        path.canonicalize()
    })
}

fn codex_catalog_home_resolving(
    inspector: Option<std::ffi::OsString>, native: Option<String>, os_home: Option<PathBuf>,
    resolve_native: impl FnOnce(&Path) -> std::io::Result<PathBuf>,
) -> CodexHomeSelection {
    if let Some(inspector) = inspector {
        let path = PathBuf::from(inspector);
        return CodexHomeSelection { home: path.is_absolute().then_some(path), inspector: true };
    }
    let home = if let Some(native) = native.filter(|value| !value.is_empty()) {
        let path = PathBuf::from(native);
        // A failed native observation stays unknown. Later metadata cannot
        // reconstruct a successful canonicalization or use the inspector cwd.
        if path.is_absolute() { resolve_native(&path).ok().filter(|path| path.is_absolute()) }
        else { None }
    } else { os_home.map(|home| home.join(".codex")).filter(|home| home.is_absolute()) };
    CodexHomeSelection { home, inspector: false }
}

pub fn discover_proma_projects(root: &Path) -> Result<Vec<ProjectEntry>> {
    let mut projects = crate::proma::workspaces(root)?
        .into_iter()
        .map(|workspace| ProjectEntry {
            path: workspace.project_root_path.unwrap_or_else(|| workspace.path.clone()),
            name: workspace.name,
            agents: vec![AgentKind::Proma],
            proma_workspaces: vec![workspace.path],
            native_workspaces: Vec::new(),
            archive_states: vec![NativeArchiveState { agent: AgentKind::Proma, archived: Some(workspace.archived) }],
            archived: workspace.archived,
            hidden: false,
            retained: false,
        })
        .collect::<Vec<_>>();
    let mut counts = BTreeMap::new();
    for project in &projects {
        *counts.entry(project.path.clone()).or_insert(0) += 1;
    }
    for project in &mut projects {
        if counts[&project.path] > 1 {
            project.name = project_name(&project.path);
        }
    }
    Ok(projects)
}

pub fn discover_agent_projects() -> Result<Vec<ProjectEntry>> {
    refresh_agent_projects(&[])
}

pub fn refresh_agent_projects(previous: &[ProjectEntry]) -> Result<Vec<ProjectEntry>> {
    let selection = codex_home_selection();
    let locations = codex_sources::runtime_locations(selection.home.as_deref(), selection.inspector);
    let catalogs = merge_catalogs(Some(discover_codex_locations_for(selection.home.as_deref(), &locations, previous)),
        crate::proma::home().as_deref(), previous);
    let home = dirs::home_dir().unwrap_or_default();
    let mut entries = native::refresh(&home, previous);
    match catalogs {
        Ok(projects) => entries.extend(projects),
        Err(error) if entries.is_empty() => return Err(error),
        Err(_) => {},
    }
    Ok(all_projects(&[], &entries, &BTreeSet::new(), &BTreeSet::new()))
}

fn discover_catalogs(codex: Option<&Path>, proma: Option<&Path>, previous: &[ProjectEntry]) -> Result<Vec<ProjectEntry>> {
    merge_catalogs(codex.map(|root| discover_codex_catalogs_at(root, root, previous)), proma, previous)
}

fn merge_catalogs(codex: Option<Result<Vec<ProjectEntry>>>, proma: Option<&Path>, previous: &[ProjectEntry]) -> Result<Vec<ProjectEntry>> {
    let mut entries = Vec::new();
    let mut failure = None;
    for (agent, catalog, present) in [
        // Codex retains each missing/unreadable catalog inside its combined reader.
        (AgentKind::Codex, codex, true),
        (AgentKind::Proma, proma.map(discover_proma_projects), proma.is_some_and(|root| root.join("agent-workspaces.json").is_file())),
    ] {
        let Some(catalog) = catalog else { continue; };
        let catalog = if !present && previous.iter().any(|entry| entry.agents.contains(&agent)) {
            Err(anyhow!("Agent 项目记录暂不可用"))
        } else { catalog };
        match catalog {
            Ok(projects) => entries.extend(projects),
            Err(error) => {
                failure = Some(error);
                entries.extend(previous.iter().filter(|entry| entry.agents.contains(&agent)).cloned().map(|mut entry| {
                    entry.retain_source(agent);
                    entry
                }));
            }
        }
    }
    if entries.is_empty() && let Some(error) = failure {
        return Err(error);
    }
    // Retained native state stays authoritative for its own Agent. An unrelated
    // catalog failure must not erase a known archive flag or reactivate a row.
    let mut merged = all_projects(&[], &entries, &BTreeSet::new(), &BTreeSet::new());
    for entry in &mut merged {
        for agent in local_installation_agents(&entry.path, Some(entry)) {
            if !entry.agents.contains(&agent) { entry.agents.push(agent); }
        }
    }
    Ok(merged)
}

/// Inspect one explicit catalog scope without inheriting process config/env.
pub fn discover_codex_projects(home: &Path) -> Result<Vec<ProjectEntry>> {
    discover_codex_locations_at(home, &codex_sources::explicit_locations(home), &[])
}

fn discover_codex_locations_at(
    home: &Path, locations: &codex_config::KnownCatalogLocations, previous: &[ProjectEntry],
) -> Result<Vec<ProjectEntry>> {
    // Origin metadata stays separate from active native configuration. Only a
    // successful fixed DB projection makes a candidate a discovered source.
    discover_codex_locations_for(Some(home), locations, previous)
}

fn discover_codex_locations_for(
    home: Option<&Path>, locations: &codex_config::KnownCatalogLocations, previous: &[ProjectEntry],
) -> Result<Vec<ProjectEntry>> {
    let roots = locations.candidates.iter().map(|candidate| candidate.root.clone()).collect::<Vec<_>>();
    discover_codex_catalog_sources_at(home, &roots, !locations.diagnostics.is_empty(), previous)
}

fn catalog_source_present(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}

fn discover_codex_catalogs_at(
    home: &Path, sqlite_home: &Path, previous: &[ProjectEntry],
) -> Result<Vec<ProjectEntry>> {
    discover_codex_catalog_sources_at(Some(home), &[sqlite_home.to_path_buf()], false, previous)
}

fn discover_codex_catalog_sources_at(
    home: Option<&Path>, sqlite_roots: &[PathBuf], unknown_location: bool, previous: &[ProjectEntry],
) -> Result<Vec<ProjectEntry>> {
    if home.is_some_and(|home| !home.is_absolute()) || sqlite_roots.iter().any(|root| !root.is_absolute()) {
        return Err(anyhow!("Codex 项目索引配置根无效"));
    }
    let mut entries = Vec::new();
    let mut complete_databases = BTreeSet::new();
    let mut failed_databases = BTreeSet::new();
    for root in sqlite_roots {
        let source = root.join("state_5.sqlite");
        let identity = path_identity(&source);
        if complete_databases.contains(&identity) || !catalog_source_present(&source) { continue; }
        match codex_projects::projects_at(root) {
            Ok(projects) => {
                complete_databases.insert(identity);
                entries.extend(projects);
            },
            Err(_) => { failed_databases.insert(identity); },
        }
    }
    let desktop_source = home.map(|home| home.join(".codex-global-state.json"));
    let mut desktop_complete = false;
    let mut failure = unknown_location
        || failed_databases.iter().any(|source| !complete_databases.contains(source));
    if let Some((home, source)) = home.zip(desktop_source.as_deref())
        && catalog_source_present(source) {
        match discover_codex_desktop_projects(home) {
            Ok(projects) => { desktop_complete = true; entries.extend(projects); },
            Err(_) => { failure = true; },
        }
    }
    // Retain only unobserved/failed sources. A healthy empty catalog must not
    // resurrect its own removed membership because another source is unknown.
    for old in previous.iter().filter(|entry| entry.agents.contains(&AgentKind::Codex)) {
        let has_workspace = old.native_workspaces.iter().any(|workspace| workspace.agent == AgentKind::Codex);
        let mut retained = old.clone();
        retained.retain_source(AgentKind::Codex);
        retained.native_workspaces.retain(|workspace| {
            if is_codex_database_workspace(workspace) {
                !complete_databases.contains(&path_identity(&workspace.path))
            } else { !desktop_complete }
        });
        if !retained.native_workspaces.is_empty() || (!has_workspace && !desktop_complete) {
            entries.push(retained);
        }
    }
    if entries.is_empty() && failure && !previous.iter().any(|entry| entry.agents.contains(&AgentKind::Codex)) {
        return Err(anyhow!("Codex 项目来源暂不可读，覆盖范围未知"));
    }
    if let Some(home) = home {
        let cloud_cache = path_identity(&home.join(".chatgpt-projects"));
        entries.retain(|entry| !entry.path.starts_with(&cloud_cache));
    }
    let mut merged = all_projects(&[], &entries, &BTreeSet::new(), &BTreeSet::new());
    merged.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    Ok(merged)
}

fn is_codex_database_workspace(workspace: &NativeWorkspace) -> bool {
    workspace.agent == AgentKind::Codex && workspace.source_id.is_some()
        && workspace.path.file_name().and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("state_5.sqlite"))
}

fn discover_codex_desktop_projects(home: &Path) -> Result<Vec<ProjectEntry>> {
    use crate::config_read::{read_regular_file_bytes_with_limit, same_regular_file, RegularFileReadError};
    let path = home.join(".codex-global-state.json");
    let before = match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return Err(anyhow!("Codex 项目记录不是普通文件")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
            && std::fs::symlink_metadata(&path).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) => return Ok(Vec::new()),
        Err(_) => return Err(anyhow!("无法读取 Codex 项目记录")),
    };
    let raw = read_regular_file_bytes_with_limit(&path, 16 * 1024 * 1024, || {})
        .map_err(|error| match error {
            RegularFileReadError::TooLarge => anyhow!("Codex 项目记录过大"),
            _ => anyhow!("无法读取 Codex 项目记录"),
        })?;
    let current = std::fs::metadata(&path).map_err(|_| anyhow!("无法读取 Codex 项目记录"))?;
    if !same_regular_file(&before, &raw.opened) || !same_regular_file(&raw.opened, &raw.finished)
        || !same_regular_file(&raw.finished, &current) {
        return Err(anyhow!("Codex 项目记录读取期间发生变化"));
    }
    let state: Value =
        serde_json::from_slice(&raw.bytes).map_err(|_| anyhow!("Codex 项目记录格式无效"))?;
    let object = state
        .as_object()
        .ok_or_else(|| anyhow!("Codex 项目记录格式无效"))?;
    let mut entries = Vec::new();
    if let Some(current) = object.get("local-projects") {
        // The primary root identifies a project; remaining roots only extend
        // its workspace. An empty current catalog supersedes legacy history.
        let projects = current
            .as_object()
            .ok_or_else(|| anyhow!("Codex 项目列表格式无效"))?;
        for (id, project) in projects {
            // ChatGPT cloud projects share this catalog but are not local projects.
            if id.starts_with("g-p-") {
                continue;
            }
            let path = project
                .get("rootPaths")
                .and_then(Value::as_array)
                .and_then(|roots| roots.first())
                .and_then(Value::as_str);
            if let Some(path) = path {
                append_codex(
                    &mut entries,
                    path,
                    project.get("name").and_then(Value::as_str),
                    Some((&home.join(".codex-global-state.json"), id)),
                );
            }
        }
    } else if let Some(roots) = object
        .get("electron-saved-workspace-roots")
        .and_then(Value::as_array)
    {
        for path in roots.iter().filter_map(Value::as_str) {
            append_codex(&mut entries, path, None, None);
        }
    }
    let cloud_cache = path_identity(&home.join(".chatgpt-projects"));
    entries.retain(|entry| !entry.path.starts_with(&cloud_cache));
    let mut merged = all_projects(&[], &entries, &BTreeSet::new(), &BTreeSet::new());
    merged.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    Ok(merged)
}

fn append_codex(
    entries: &mut Vec<ProjectEntry>, path: &str, name: Option<&str>,
    source: Option<(&Path, &str)>,
) {
    let path = Path::new(path);
    if !path.is_absolute() || !path.is_dir() {
        return;
    }
    let path = path_identity(path);
    let workspace = NativeWorkspace {
        agent: AgentKind::Codex,
        source_id: source.map(|(_, id)| id.to_string()),
        display_name: name.filter(|name| !name.trim().is_empty()).map(str::to_string),
        archived: None,
        // Legacy saved roots have no project ID: retain the actual directory
        // identity instead of manufacturing a catalog-wide workspace ID.
        // Keep the native catalog address for provenance. A symlink target's
        // filename is not the source type; id() still canonicalizes identity.
        path: source.map(|(catalog, _)| catalog.to_path_buf()).unwrap_or_else(|| path.clone()),
        project_roots: Arc::new(vec![path.clone()]),
    };
    entries.push(ProjectEntry {
        name: name
            .filter(|name| !name.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| project_name(&path)),
        path,
        agents: vec![AgentKind::Codex],
        proma_workspaces: Vec::new(),
        native_workspaces: vec![workspace], archive_states: Vec::new(),
        // Codex's project-order is a sort preference, not an archive flag.
        archived: false,
        hidden: false,
        retained: false,
    });
}

fn project_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

pub fn project_identity(path: &Path, discovered: &[ProjectEntry]) -> PathBuf {
    let identity = path_identity(path);
    discovered.iter()
        .find(|entry| entry.proma_workspaces.iter().any(|native| path_identity(native) == identity))
        .map(|entry| path_identity(&entry.path))
        .unwrap_or(identity)
}

/// Catalog associations constrain known projects. Global inspection and
/// unregistered directories retain the full compatibility view.
pub fn agents_for_project(
    path: &Path,
    agents: &[crate::agents::AgentIconInfo],
    discovered: &[ProjectEntry],
) -> Vec<crate::agents::AgentIconInfo> {
    if dirs::home_dir().is_some_and(|home| path_identity(&home) == path_identity(path)) {
        return agents.to_vec();
    }
    let identity = project_identity(path, discovered);
    let entry = discovered.iter().find(|entry| path_identity(&entry.path) == identity);
    let local_agents = local_installation_agents(&identity, entry);
    agents.iter().copied().filter(|info| {
        entry.is_none_or(|entry| entry.agents.is_empty()
            || entry.agents.iter().any(|agent| agent.id() == info.id))
            || local_agents.iter().any(|agent| agent.id() == info.id)
    }).collect()
}

fn local_installation_agents(project: &Path, entry: Option<&ProjectEntry>) -> Vec<AgentKind> {
    use crate::agents::{self, installation_root};
    if dirs::home_dir().is_some_and(|home| path_identity(&home) == path_identity(project)) { return Vec::new(); }
    // Compatibility directories inside an Agent's private workspace are not
    // native project registrations. Real catalog associations remain intact.
    if entry.is_some_and(|entry| entry.proma_workspaces.iter()
        .any(|workspace| path_identity(workspace) == path_identity(project))) {
        return Vec::new();
    }
    let targets = agents::PROJECT_INSTALL_TARGETS.iter().copied().filter(|target| {
        installation_root(project, *target).ok().and_then(|root| std::fs::read_dir(root).ok()).into_iter().flatten().flatten()
            .any(|entry| entry.path().join("SKILL.md").is_file())
    }).collect::<Vec<_>>();
    // Plain `skills` and shared .agents roots do not prove an OpenClaw native
    // binding. Its associations come from agentId/workspace catalog entries;
    // explicit compatibility inspection and target operations remain available.
    agents::AGENT_ICON_ORDER.iter().filter(|agent| agent.id != "openclaw" && !agent.global_only && targets.iter().any(|target| {
        agent.owns_target(*target) || (*target == crate::InstallTarget::Universal && agent.shared_agents)
    })).filter_map(|agent| AgentKind::from_id(agent.id)).collect()
}

/// A Finder project is one row, even when an Agent stores Skills elsewhere.
pub fn merge_projects(
    manual: &[PathBuf],
    discovered: &[ProjectEntry],
    hidden: &BTreeSet<PathBuf>,
) -> Vec<ProjectEntry> {
    all_projects(manual, discovered, hidden, &BTreeSet::new())
        .into_iter().filter(ProjectEntry::is_active).collect()
}

pub fn all_projects(
    manual: &[PathBuf],
    discovered: &[ProjectEntry],
    hidden: &BTreeSet<PathBuf>,
    retained: &BTreeSet<PathBuf>,
) -> Vec<ProjectEntry> {
    let hidden = hidden
        .iter()
        .map(|path| project_identity(path, discovered))
        .collect::<BTreeSet<_>>();
    let retained = retained.iter().map(|path| project_identity(path, discovered)).collect::<BTreeSet<_>>();
    let mut merged: Vec<ProjectEntry> = Vec::new();
    for path in manual.iter().chain(hidden.iter()).chain(retained.iter()) {
        let path = project_identity(path, discovered);
        if !merged.iter().any(|entry| entry.path == path) {
            merged.push(ProjectEntry {
                name: project_name(&path),
                path,
                agents: Vec::new(),
                proma_workspaces: Vec::new(),
                native_workspaces: Vec::new(), archive_states: Vec::new(),
                archived: false,
                hidden: false,
                retained: false,
            });
        }
    }
    for candidate in discovered {
        let path = path_identity(&candidate.path);
        if !path.is_dir() {
            continue;
        }
        if let Some(existing) = merged.iter_mut().find(|entry| entry.path == path) {
            if existing.agents.is_empty() {
                existing.name = candidate.name.clone();
            }
            existing.merge_archival(candidate);
            for agent in &candidate.agents {
                if !existing.agents.contains(agent) {
                    existing.agents.push(*agent);
                }
            }
            for workspace in &candidate.proma_workspaces {
                if !existing.proma_workspaces.contains(workspace) {
                    existing.proma_workspaces.push(workspace.clone());
                }
            }
            for workspace in &candidate.native_workspaces {
                if let Some(current) = existing.native_workspaces.iter_mut().find(|current| current.id() == workspace.id()) {
                    if !Arc::ptr_eq(&current.project_roots, &workspace.project_roots) {
                        let roots = Arc::make_mut(&mut current.project_roots);
                        for root in workspace.project_roots.iter() {
                            if !roots.contains(root) { roots.push(root.clone()); }
                        }
                    }
                } else { existing.native_workspaces.push(workspace.clone()); }
            }
        } else {
            let mut entry = ProjectEntry {
                path,
                ..candidate.clone()
            };
            entry.normalize_archive_states();
            merged.push(entry);
        }
    }
    for entry in &mut merged {
        entry.hidden = hidden.contains(&entry.path);
        entry.retained = retained.contains(&entry.path);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codex_project_catalog_fixture(home: &Path, id: &str, roots: &[&Path]) {
        std::fs::create_dir_all(home).unwrap();
        let connection = rusqlite::Connection::open(home.join("state_5.sqlite")).unwrap();
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS projects(id TEXT PRIMARY KEY, name TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS project_roots(project_id TEXT, position INTEGER, path TEXT);",
        ).unwrap();
        connection.execute("INSERT INTO projects(id,name) VALUES (?1,?2)", [id, id]).unwrap();
        for (position, root) in roots.iter().enumerate() {
            connection.execute(
                "INSERT INTO project_roots(project_id,position,path) VALUES (?1,?2,?3)",
                rusqlite::params![id, position as i64, root.to_str().unwrap()],
            ).unwrap();
        }
    }

    #[test]
    fn codex_native_and_desktop_catalogs_merge_shared_directory_and_workspace_membership() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let shared = home.join("shared");
        let native = home.join("native-only");
        let desktop = home.join("desktop-only");
        for path in [&shared, &native, &desktop] { std::fs::create_dir(path).unwrap(); }
        codex_project_catalog_fixture(&home, "native-project", &[&shared, &native]);
        std::fs::write(home.join(".codex-global-state.json"), serde_json::to_vec(&serde_json::json!({
            "local-projects": {
                "desktop-shared": {"name":"Shared", "rootPaths":[shared]},
                "desktop-only": {"name":"Desktop", "rootPaths":[desktop]}
            }
        })).unwrap()).unwrap();
        let rows = discover_codex_projects(&home).unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.agents == [AgentKind::Codex] && !row.archived));
        for path in [&shared, &native] {
            let row = rows.iter().find(|row| &row.path == path).unwrap();
            assert!(row.native_workspaces.iter().any(|workspace| workspace.agent == AgentKind::Codex
                && workspace.source_id.is_some() && workspace.project_roots.contains(&shared)
                && workspace.project_roots.contains(&native)));
        }
    }

    #[test]
    fn codex_unreadable_native_catalog_retains_prior_ownership_and_discovers_desktop() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let old = home.join("native-existing");
        let new = home.join("desktop-new");
        for path in [&old, &new] { std::fs::create_dir(path).unwrap(); }
        codex_project_catalog_fixture(&home, "native-project", &[&old]);
        let previous = discover_codex_projects(&home).unwrap();
        std::fs::write(home.join("state_5.sqlite"), b"invalid-native-catalog").unwrap();
        std::fs::write(home.join(".codex-global-state.json"), serde_json::to_vec(&serde_json::json!({
            "local-projects": {"new": {"rootPaths":[new]}}
        })).unwrap()).unwrap();
        let before = std::fs::read(home.join("state_5.sqlite")).unwrap();
        let rows = discover_codex_catalogs_at(&home, &home, &previous).unwrap();
        assert_eq!(rows.len(), 2);
        let retained = rows.iter().find(|row| row.path == old).unwrap();
        assert_eq!(retained.native_workspaces, previous[0].native_workspaces);
        assert!(rows.iter().all(|row| row.agents == [AgentKind::Codex] && !row.archived));
        assert_eq!(std::fs::read(home.join("state_5.sqlite")).unwrap(), before);
    }

    #[test]
    fn codex_missing_desktop_catalog_does_not_remove_its_previous_native_ownership() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let old = home.join("desktop-existing");
        let new = home.join("native-new");
        for path in [&old, &new] { std::fs::create_dir(path).unwrap(); }
        let desktop = home.join(".codex-global-state.json");
        std::fs::write(&desktop, serde_json::to_vec(&serde_json::json!({
            "local-projects": {"old": {"rootPaths":[old]}}
        })).unwrap()).unwrap();
        let previous = discover_codex_projects(&home).unwrap();
        std::fs::remove_file(&desktop).unwrap();
        codex_project_catalog_fixture(&home, "new", &[&new]);
        let rows = discover_codex_catalogs_at(&home, &home, &previous).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.path == old));
        assert!(rows.iter().any(|row| row.path == new));
        assert!(rows.iter().all(|row| !row.archived));
    }

    #[test]
    fn codex_joint_directory_keeps_desktop_provenance_when_that_source_disappears() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = home.join("shared-project");
        std::fs::create_dir(&project).unwrap();
        codex_project_catalog_fixture(&home, "native-project", &[&project]);
        let desktop = home.join(".codex-global-state.json");
        std::fs::write(&desktop, serde_json::to_vec(&serde_json::json!({
            "local-projects": {"desktop-project": {"rootPaths":[project]}}
        })).unwrap()).unwrap();
        let previous = discover_codex_projects(&home).unwrap();
        assert_eq!(previous.len(), 1);
        std::fs::remove_file(desktop).unwrap();
        let database = rusqlite::Connection::open(home.join("state_5.sqlite")).unwrap();
        database.execute_batch("DELETE FROM project_roots; DELETE FROM projects;").unwrap();
        drop(database);
        let rows = discover_codex_catalogs_at(&home, &home, &previous).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, project);
        assert_eq!(rows[0].agents, [AgentKind::Codex]);
        assert!(!rows[0].archived);
    }

    #[test]
    #[cfg(unix)]
    fn codex_missing_desktop_alias_preserves_its_provenance_with_empty_or_changed_database() {
        for new_database_root in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let home = root.join("profile");
            let aliases = root.join("aliases");
            let old = root.join("old-project");
            let new = root.join("new-project");
            for path in [&home, &aliases, &old, &new] { std::fs::create_dir(path).unwrap(); }
            codex_project_catalog_fixture(&home, "database-project", &[&old]);
            let alias_target = aliases.join("state_5.sqlite");
            std::fs::write(&alias_target, serde_json::to_vec(&serde_json::json!({
                "local-projects": {"desktop-project": {"rootPaths":[old]}}
            })).unwrap()).unwrap();
            let desktop = home.join(".codex-global-state.json");
            std::os::unix::fs::symlink(&alias_target, &desktop).unwrap();
            let previous = discover_codex_projects(&home).unwrap();
            let workspace = previous[0].native_workspaces.iter()
                .find(|workspace| workspace.source_id.as_deref() == Some("desktop-project")).unwrap();
            assert_eq!(workspace.path, desktop);
            assert!(!is_codex_database_workspace(workspace));
            let mut canonical_workspace = workspace.clone();
            canonical_workspace.path = alias_target.clone();
            assert_eq!(workspace.id(), canonical_workspace.id());
            std::fs::remove_file(desktop).unwrap();
            let database = rusqlite::Connection::open(home.join("state_5.sqlite")).unwrap();
            database.execute_batch("DELETE FROM project_roots; DELETE FROM projects;").unwrap();
            drop(database);
            if new_database_root { codex_project_catalog_fixture(&home, "new-native", &[&new]); }
            let rows = discover_codex_catalogs_at(&home, &home, &previous).unwrap();
            assert_eq!(rows.len(), if new_database_root { 2 } else { 1 });
            assert!(rows.iter().any(|row| row.path == old && row.agents == [AgentKind::Codex]));
            assert!(rows.iter().all(|row| !row.archived));
            assert_eq!(rows.iter().any(|row| row.path == new), new_database_root);
        }
    }

    #[test]
    fn codex_complete_empty_database_does_not_retain_removed_database_only_projects() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = home.join("project");
        std::fs::create_dir(&project).unwrap();
        codex_project_catalog_fixture(&home, "native-project", &[&project]);
        let previous = discover_codex_projects(&home).unwrap();
        let database = rusqlite::Connection::open(home.join("state_5.sqlite")).unwrap();
        database.execute_batch("DELETE FROM project_roots; DELETE FROM projects;").unwrap();
        drop(database);
        assert!(discover_codex_catalogs_at(&home, &home, &previous).unwrap().is_empty());
    }

    #[test]
    fn codex_home_empty_native_value_and_relative_value_preserve_native_semantics() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().canonicalize().unwrap();
        assert_eq!(codex_catalog_home_from(None, None, Some(home.clone())).home, Some(home.join(".codex")));
        assert_eq!(codex_catalog_home_from(None, Some(String::new()), Some(home.clone())).home, Some(home.join(".codex")));
        assert_eq!(codex_catalog_home_from(None, Some("relative-home".into()), Some(home.clone())).home, None);
        assert_eq!(codex_catalog_home_from(None, Some("   ".into()), Some(home)).home, None);
        let explicit = root.path().join("explicit-inspector").into_os_string();
        let expected = PathBuf::from(&explicit);
        assert_eq!(codex_catalog_home_from(Some(explicit), Some("ignored".into()), None).home, Some(expected));
    }

    #[cfg(unix)]
    #[test]
    fn codex_absolute_native_home_alias_is_resolved_before_relative_config_paths() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let actual = base.join("different-parent/profile");
        std::fs::create_dir_all(&actual).unwrap();
        let alias = base.join("profile-alias");
        std::os::unix::fs::symlink(&actual, &alias).unwrap();
        assert_eq!(codex_catalog_home_from(None, Some(alias.to_str().unwrap().into()), None).home, Some(actual));
    }

    #[test]
    fn codex_failed_home_resolution_stays_unknown_and_keeps_independent_system_catalog() {
        use codex_config::{SqliteHomeEnv, UnknownCode};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let home = root.join("native-home");
        let system = root.join("system.toml");
        let system_data = root.join("system-data");
        let wrong_data = root.join("user-data-not-observed");
        let old = root.join("previous-project");
        let new = root.join("system-project");
        let wrong = root.join("unobserved-user-project");
        for path in [&home, &system_data, &wrong_data, &old, &new, &wrong] { std::fs::create_dir(path).unwrap(); }
        codex_project_catalog_fixture(&home, "old", &[&old]);
        codex_project_catalog_fixture(&system_data, "system-new", &[&new]);
        codex_project_catalog_fixture(&wrong_data, "wrong", &[&wrong]);
        std::fs::write(home.join("config.toml"), "sqlite_home = '../user-data-not-observed'\n").unwrap();
        std::fs::write(&system, "sqlite_home = 'system-data'\n").unwrap();
        let previous = discover_codex_projects(&home).unwrap();
        let selection = codex_catalog_home_resolving(None, Some(home.to_str().unwrap().into()), Some(root.clone()),
            |_| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)));
        assert!(selection.home.is_none());
        // A later successful observation cannot erase the failed resolution.
        assert!(home.is_dir());
        let locations = codex_sources::locations_for_config_files(selection.home.as_deref(), Some(&root),
            Some(&system), None, SqliteHomeEnv::KnownAbsent, None);
        assert!(locations.diagnostics.iter().any(|item| item.code == UnknownCode::ResolvedHomeUnavailable));
        assert_eq!(locations.candidates.iter().map(|item| item.root.clone()).collect::<Vec<_>>(), [system_data]);
        let rows = discover_codex_locations_for(selection.home.as_deref(), &locations, &previous).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.path == old));
        assert!(rows.iter().any(|row| row.path == new));
        assert!(!rows.iter().any(|row| row.path == wrong));
        assert!(rows.iter().all(|row| !row.archived));
    }

    #[test]
    fn codex_relative_unknown_home_reads_absolute_env_and_clears_only_complete_source_members() {
        use codex_config::SqliteHomeEnv;
        for new_env_record in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let old_home = root.join("previous-native-home");
            let env_data = root.join("env-data");
            let unknown = root.join("unknown-home-project");
            let removed = root.join("removed-env-project");
            let new = root.join("new-env-project");
            for path in [&old_home, &env_data, &unknown, &removed, &new] { std::fs::create_dir(path).unwrap(); }
            codex_project_catalog_fixture(&old_home, "unknown", &[&unknown]);
            codex_project_catalog_fixture(&env_data, "removed", &[&removed]);
            let mut previous = discover_codex_projects(&old_home).unwrap();
            previous.extend(discover_codex_projects(&env_data).unwrap());
            let db = rusqlite::Connection::open(env_data.join("state_5.sqlite")).unwrap();
            db.execute_batch("DELETE FROM project_roots; DELETE FROM projects;").unwrap();
            drop(db);
            if new_env_record { codex_project_catalog_fixture(&env_data, "new", &[&new]); }
            let selection = codex_catalog_home_resolving(None, Some("relative-native-home".into()), Some(root.clone()),
                |_| panic!("relative native home cannot use the inspector cwd"));
            assert!(selection.home.is_none());
            let absent_system = root.join("absent-system.toml");
            let locations = codex_sources::locations_for_config_files(None, Some(&root), Some(&absent_system), None,
                SqliteHomeEnv::Known(env_data.to_str().unwrap()), None);
            let rows = discover_codex_locations_for(None, &locations, &previous).unwrap();
            assert_eq!(rows.len(), if new_env_record { 2 } else { 1 });
            assert!(rows.iter().any(|row| row.path == unknown));
            assert!(!rows.iter().any(|row| row.path == removed));
            assert_eq!(rows.iter().any(|row| row.path == new), new_env_record);
            assert!(rows.iter().all(|row| !row.archived));
        }
    }

    #[test]
    fn codex_configured_catalogs_merge_members_and_keep_each_native_source_identity() {
        use codex_config::SqliteHomeEnv;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let home = root.join("profile");
        let system = root.join("system");
        let extra = root.join("external-catalog");
        let shared = root.join("shared-project");
        let legacy = root.join("legacy-project");
        let unrelated = root.join("unrelated-project");
        for path in [&home, &system, &extra, &shared, &legacy, &unrelated] { std::fs::create_dir(path).unwrap(); }
        codex_project_catalog_fixture(&home, "same-native-id", &[&shared]);
        codex_project_catalog_fixture(&extra, "same-native-id", &[&shared]);
        let user_config = home.join("config.toml");
        let system_config = system.join("config.toml");
        for path in [&user_config, &system_config] {
            std::fs::write(path, "sqlite_home = '../external-catalog'\n").unwrap();
        }
        let desktop = home.join(".codex-global-state.json");
        std::fs::write(&desktop, serde_json::to_vec(&serde_json::json!({
            "local-projects":{"legacy":{"rootPaths":[legacy]}}
        })).unwrap()).unwrap();
        // A SQLite data root does not relocate the desktop catalog.
        std::fs::write(extra.join(".codex-global-state.json"), serde_json::to_vec(&serde_json::json!({
            "local-projects":{"unrelated":{"rootPaths":[unrelated]}}
        })).unwrap()).unwrap();
        let locations = codex_sources::locations_from_config_files(&home, Some(&root),
            Some(&system_config), &user_config, SqliteHomeEnv::Known(extra.to_str().unwrap()), None);
        assert!(locations.diagnostics.is_empty());
        assert_eq!(locations.candidates.len(), 2);
        assert_eq!(locations.candidates.iter().find(|candidate| candidate.root == extra).unwrap().origins.len(), 3);
        let paths = [&user_config, &system_config, &desktop, &home.join("state_5.sqlite"), &extra.join("state_5.sqlite")];
        let before = paths.iter().map(|path| std::fs::read(path).unwrap()).collect::<Vec<_>>();
        let rows = discover_codex_locations_at(&home, &locations, &[]).unwrap();
        assert_eq!(rows.len(), 2);
        let member = rows.iter().find(|row| row.path == shared).unwrap();
        assert_eq!(member.native_workspaces.len(), 2);
        assert!(member.native_workspaces.iter().all(|workspace| workspace.source_id.as_deref() == Some("same-native-id")));
        assert_eq!(member.native_workspaces.iter().map(NativeWorkspace::id).collect::<BTreeSet<_>>().len(), 2);
        assert!(!rows.iter().any(|row| row.path == unrelated));
        assert_eq!(discover_codex_locations_at(&home, &locations, &rows).unwrap(), rows);
        assert_eq!(paths.iter().map(|path| std::fs::read(path).unwrap()).collect::<Vec<_>>(), before);
    }

    #[test]
    fn codex_failed_config_retains_unobserved_catalog_but_not_healthy_empty_membership() {
        use codex_config::SqliteHomeEnv;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let home = root.join("profile");
        let extra = root.join("external-catalog");
        let old = root.join("default-project");
        let retained = root.join("external-project");
        for path in [&home, &extra, &old, &retained] { std::fs::create_dir(path).unwrap(); }
        codex_project_catalog_fixture(&home, "default", &[&old]);
        codex_project_catalog_fixture(&extra, "external", &[&retained]);
        let config = home.join("config.toml");
        std::fs::write(&config, "sqlite_home = '../external-catalog'\n").unwrap();
        let first = codex_sources::locations_from_config_files(&home, Some(&root), None, &config, SqliteHomeEnv::KnownAbsent, None);
        let previous = discover_codex_locations_at(&home, &first, &[]).unwrap();
        assert_eq!(previous.len(), 2);
        let database = rusqlite::Connection::open(home.join("state_5.sqlite")).unwrap();
        database.execute_batch("DELETE FROM project_roots; DELETE FROM projects;").unwrap();
        drop(database);
        std::fs::write(&config, "malformed = [").unwrap();
        let unknown = codex_sources::locations_from_config_files(&home, Some(&root), None, &config, SqliteHomeEnv::KnownAbsent, None);
        assert!(unknown.diagnostics.iter().any(|item| item.code == codex_config::UnknownCode::InvalidToml));
        let rows = discover_codex_locations_at(&home, &unknown, &previous).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, retained);
        assert!(!rows[0].archived);
        assert_eq!(rows[0].native_workspaces, previous.iter().find(|row| row.path == retained).unwrap().native_workspaces);
    }

    #[test]
    fn codex_relative_env_unknown_does_not_hide_known_catalogs_or_use_inspector_cwd() {
        use codex_config::{SqliteHomeEnv, UnknownCode};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let home = root.join("profile");
        let extra = root.join("external-catalog");
        let wrong = home.join("relative-catalog");
        let default_project = root.join("default-project");
        let extra_project = root.join("extra-project");
        let wrong_project = root.join("wrong-project");
        for path in [&home, &extra, &wrong, &default_project, &extra_project, &wrong_project] { std::fs::create_dir(path).unwrap(); }
        codex_project_catalog_fixture(&home, "default", &[&default_project]);
        codex_project_catalog_fixture(&extra, "external", &[&extra_project]);
        codex_project_catalog_fixture(&wrong, "wrong", &[&wrong_project]);
        let config = home.join("config.toml");
        std::fs::write(&config, "sqlite_home = '../external-catalog'\n").unwrap();
        let locations = codex_sources::locations_from_config_files(&home, Some(&root), None, &config,
            SqliteHomeEnv::Known("relative-catalog"), None);
        assert!(locations.diagnostics.iter().any(|diagnostic| diagnostic.code == UnknownCode::NativeCwdUnavailable));
        let rows = discover_codex_locations_at(&home, &locations, &[]).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(!rows.iter().any(|row| row.path == wrong_project));
    }

    #[test]
    #[cfg(unix)]
    fn codex_legacy_fifo_is_unknown_without_opening_or_repairing_it() {
        use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
        let root = tempfile::tempdir().unwrap();
        let fifo = root.path().join(".codex-global-state.json");
        let path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let before = std::fs::symlink_metadata(&fifo).unwrap();
        assert!(discover_codex_projects(root.path()).is_err());
        let after = std::fs::symlink_metadata(&fifo).unwrap();
        assert_eq!((before.dev(), before.ino(), before.mode(), before.len()),
            (after.dev(), after.ino(), after.mode(), after.len()));
    }

    #[test]
    fn codex_native_catalog_accepts_explicit_separate_sqlite_home_without_environment_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let home = root.join("codex");
        let sqlite = root.join("sqlite");
        let project = root.join("project");
        for path in [&home, &sqlite, &project] { std::fs::create_dir(path).unwrap(); }
        codex_project_catalog_fixture(&sqlite, "project", &[&project]);
        let rows = discover_codex_catalogs_at(&home, &sqlite, &[]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, project);
        assert!(!home.join("state_5.sqlite").exists());
    }
    use serde_json::json;

    #[test]
    fn unknown_archive_sources_remain_unknown_and_cannot_hide_shared_projects() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap();
        let mut known = native::project(path.to_str().unwrap(), AgentKind::Hermes).unwrap();
        known.record_archive(AgentKind::Hermes, true);
        let unknown = native::project(path.to_str().unwrap(), AgentKind::Trae).unwrap();
        for sources in [vec![known.clone(), unknown.clone()], vec![unknown.clone(), known.clone()]] {
            let merged = all_projects(&[], &sources, &BTreeSet::new(), &BTreeSet::new());
            assert!(!merged[0].archived);
            assert!(merged[0].archive_states.contains(&NativeArchiveState { agent: AgentKind::Trae, archived: None }));
            let mut retained_unknown = merged[0].clone();
            retained_unknown.retain_source(AgentKind::Trae);
            assert!(!retained_unknown.archived);
            assert_eq!(retained_unknown.archive_states, [NativeArchiveState { agent: AgentKind::Trae, archived: None }]);
        }
    }

    #[test]
    fn failed_source_retains_its_own_archive_observation_after_mixed_merge() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap();
        let mut archived = native::project(path.to_str().unwrap(), AgentKind::Hermes).unwrap();
        archived.record_archive(AgentKind::Hermes, true);
        let mut active = native::project(path.to_str().unwrap(), AgentKind::Proma).unwrap();
        active.record_archive(AgentKind::Proma, false);
        let mut merged = all_projects(&[], &[archived, active], &BTreeSet::new(), &BTreeSet::new()).remove(0);
        assert!(!merged.archived);
        merged.retain_source(AgentKind::Hermes);
        assert!(merged.archived);
        assert_eq!(merged.agents, [AgentKind::Hermes]);
        assert_eq!(merged.archive_states, [NativeArchiveState { agent: AgentKind::Hermes, archived: Some(true) }]);
    }

    #[cfg(unix)]
    #[test]
    fn regression_native_workspace_compatibility_installs_do_not_register_other_agents() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let codex = root.join("codex");
        let proma = root.join("proma");
        let business = root.join("business");
        let codex_only = root.join("codex-only");
        let source = root.join("tabbit");
        std::fs::create_dir_all(&codex).unwrap();
        std::fs::create_dir(&business).unwrap();
        std::fs::create_dir(&codex_only).unwrap();
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("SKILL.md"), "Tabbit compatibility skill").unwrap();
        let natives = ["default", "another-native", "business-a", "business-b"]
            .map(|slug| proma.join("agent-workspaces").join(slug));
        for native in &natives {
            for target in crate::agents::PROJECT_INSTALL_TARGETS {
                let root = crate::agents::installation_root(native, *target).unwrap();
                std::fs::create_dir_all(&root).unwrap();
                std::os::unix::fs::symlink(&source, root.join("tabbit")).unwrap();
            }
        }
        write_state(&codex, json!({"local-projects":{"codex-only":{"rootPaths":[codex_only]}}}));
        std::fs::write(proma.join("agent-workspaces.json"), serde_json::to_vec(&json!({
            "version":2,"workspaces":[
                {"id":"default","slug":"default","name":"Default"},
                {"id":"another-native","slug":"another-native","name":"Another native"},
                {"id":"business-a","slug":"business-a","name":"Business A","projectRootPath":business},
                {"id":"business-b","slug":"business-b","name":"Business B","projectRootPath":business}
            ]
        })).unwrap()).unwrap();
        let entries = discover_catalogs(Some(&codex), Some(&proma), &[]).unwrap();
        assert_eq!(entries.len(), 4, "two workspaces bound to one business project form one row");
        for path in natives.iter().chain([&business]) {
            let identity = project_identity(path, &entries);
            let entry = entries.iter().find(|entry| entry.path == identity).unwrap();
            assert_eq!(entry.agents, [AgentKind::Proma], "{}", path.display());
            assert_eq!(agents_for_project(path, crate::agents::AGENT_ICON_ORDER, &entries)
                .iter().map(|agent| agent.id).collect::<Vec<_>>(), ["proma"], "{}", path.display());
        }
        assert_eq!(entries.iter().find(|entry| entry.path == business).unwrap().proma_workspaces.len(), 2);

        write_state(&codex, json!({"local-projects":{
            "actual-native":{"rootPaths":[natives[1]]},
            "actual-business":{"rootPaths":[business]},
            "codex-only":{"rootPaths":[codex_only]}
        }}));
        let shared = discover_catalogs(Some(&codex), Some(&proma), &[]).unwrap();
        for path in natives.iter().chain([&business, &codex_only]) {
            let expected = if path == &natives[0] { vec!["proma"] }
                else if path == &codex_only { vec!["codex"] } else { vec!["codex", "proma"] };
            assert_eq!(agents_for_project(path, crate::agents::AGENT_ICON_ORDER, &shared)
                .iter().map(|agent| agent.id).collect::<Vec<_>>(), expected, "{}", path.display());
        }
        for native in &natives {
            for target in crate::agents::PROJECT_INSTALL_TARGETS {
                assert!(crate::agents::installation_root(native, *target).unwrap().join("tabbit/SKILL.md").is_file());
            }
        }
    }

    #[test]
    fn explicit_local_installs_extend_agent_scope_without_unrelated_global_agents() {
        let temp = tempfile::tempdir().unwrap(); let project = temp.path().join("business");
        std::fs::create_dir_all(project.join("skills/plain")).unwrap();
        std::fs::write(project.join("skills/plain/SKILL.md"), "ordinary business content").unwrap();
        let entries = vec![ProjectEntry { path:project.clone(), name:"Business".into(), agents:vec![AgentKind::Proma],
            proma_workspaces:vec![], native_workspaces: Vec::new(), archive_states: Vec::new(), archived:false, hidden:false, retained:false }];
        let agents = crate::agents::AGENT_ICON_ORDER.iter().filter(|a| ["codex","cursor","grok","proma"].contains(&a.id)).copied().collect::<Vec<_>>();
        assert_eq!(agents_for_project(&project, &agents, &entries).iter().map(|a| a.id).collect::<Vec<_>>(), vec!["proma"]);
        std::fs::create_dir_all(project.join(".grok/skills/example")).unwrap();
        std::fs::write(project.join(".grok/skills/example/SKILL.md"), "explicit Grok install").unwrap();
        let scope = agents_for_project(&project, &agents, &entries);
        assert!(scope.iter().any(|a| a.id == "grok")); assert!(scope.iter().any(|a| a.id == "proma"));
        assert!(!scope.iter().any(|a| a.id == "codex" || a.id == "cursor"));
        std::fs::remove_file(project.join(".grok/skills/example/SKILL.md")).unwrap();
        assert!(!agents_for_project(&project, &agents, &entries).iter().any(|a| a.id == "grok"));
    }

    #[test]
    fn project_agent_scope_follows_catalog_aliases_and_preserves_global_inspection() {
        let temp = tempfile::tempdir().unwrap();
        let business = temp.path().join("business");
        let native = temp.path().join("proma/default");
        let mut entry = ProjectEntry {
            path: business.clone(), name: "Project".into(),
            agents: vec![AgentKind::Proma], proma_workspaces: vec![native.clone()],
            native_workspaces: Vec::new(), archive_states: Vec::new(),
            archived: false, hidden: false, retained: false,
        };
        let agents = crate::agents::AGENT_ICON_ORDER;
        for path in [&business, &native] {
            let scoped = agents_for_project(path, agents, &[entry.clone()]);
            assert_eq!(scoped.iter().map(|info| info.id).collect::<Vec<_>>(), ["proma"]);
        }
        entry.agents.push(AgentKind::Codex);
        assert_eq!(agents_for_project(&business, agents, &[entry.clone()])
            .iter().map(|info| info.id).collect::<Vec<_>>(), ["codex", "proma"]);
        assert_eq!(agents_for_project(&temp.path().join("unknown"), agents, &[entry.clone()]), agents);
        if let Some(home) = dirs::home_dir() {
            entry.path = home.clone();
            assert_eq!(agents_for_project(&home, agents, &[entry]), agents);
        }
    }

    #[cfg(unix)]
    #[test]
    fn archive_visibility_merges_agents_and_workspaces_and_restores_without_deleting() {
        let temp = tempfile::tempdir().unwrap();
        let business = temp.path().join("project");
        std::fs::create_dir(&business).unwrap();
        for slug in ["first", "second"] {
            std::fs::create_dir_all(temp.path().join("agent-workspaces").join(slug)).unwrap();
        }
        let catalog = temp.path().join("agent-workspaces.json");
        let write = |second_archived| std::fs::write(&catalog, serde_json::to_vec(&json!({
            "version": 2, "workspaces": [
                {"id":"first","slug":"first","name":"First","projectRootPath":business,"archived":true},
                {"id":"second","slug":"second","name":"Second","projectRootPath":business,"archived":second_archived}
            ]
        })).unwrap()).unwrap();
        write(false);
        assert!(all_projects(&[], &discover_proma_projects(temp.path()).unwrap(), &BTreeSet::new(), &BTreeSet::new())[0].is_active());
        write(true);
        let archived = discover_proma_projects(temp.path()).unwrap();
        let entries = all_projects(&[business.clone()], &archived, &BTreeSet::new(), &BTreeSet::new());
        assert_eq!(entries.len(), 1);
        assert!(entries[0].archived && !entries[0].is_active());
        assert_eq!(entries[0].proma_workspaces.len(), 2);
        write_state(temp.path(), json!({"local-projects":{"codex":{"rootPaths":[business]}},"project-order":[]}));
        let combined = discover_catalogs(Some(temp.path()), Some(temp.path()), &[]).unwrap();
        assert_eq!(combined.len(), 1);
        assert!(combined[0].is_active());
        assert_eq!(combined[0].agents.len(), 2);
        let mut config = crate::config::AppConfig::default();
        config.remove_project(&business);
        assert!(config.project_entries(&combined)[0].hidden);
        assert!(!config.project_entries(&combined)[0].is_active());
        config.restore_project(business.clone());
        let restored = config.project_entries(&archived);
        assert!(restored[0].archived && restored[0].retained && restored[0].is_active());
        assert!(business.is_dir());
        assert!(catalog.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn failed_or_missing_catalog_keeps_previous_projects_and_cannot_archive_shared_projects() {
        let temp = tempfile::tempdir().unwrap();
        let codex = temp.path().join("codex");
        let proma = temp.path().join("proma");
        let business = temp.path().join("project");
        std::fs::create_dir(&codex).unwrap();
        std::fs::create_dir(&business).unwrap();
        std::fs::create_dir_all(proma.join("agent-workspaces/first")).unwrap();
        std::fs::write(proma.join("agent-workspaces.json"), serde_json::to_vec(&json!({
            "version":2,"workspaces":[{"id":"first","slug":"first","name":"First","projectRootPath":business,"archived":true}]
        })).unwrap()).unwrap();
        write_state(&codex, json!({"local-projects":{"project":{"rootPaths":[business]}}}));
        let previous = discover_catalogs(Some(&codex), Some(&proma), &[]).unwrap();
        std::fs::write(codex.join(".codex-global-state.json"), "private-invalid").unwrap();
        let failed = discover_catalogs(Some(&codex), Some(&proma), &previous).unwrap();
        assert_eq!(failed.len(), 1);
        assert!(failed[0].is_active());
        assert!(failed[0].agents.contains(&AgentKind::Codex));
        std::fs::remove_file(codex.join(".codex-global-state.json")).unwrap();
        assert!(discover_catalogs(Some(&codex), Some(&proma), &previous).unwrap()[0].is_active());
    }

    #[cfg(unix)]
    #[test]
    fn failed_codex_source_does_not_reactivate_known_proma_archive() {
        let temp = tempfile::tempdir().unwrap();
        let codex = temp.path().join("codex");
        let proma = temp.path().join("proma");
        let project = temp.path().join("project");
        std::fs::create_dir(&codex).unwrap();
        std::fs::create_dir(&project).unwrap();
        std::fs::create_dir_all(proma.join("agent-workspaces/archived")).unwrap();
        write_state(&codex, json!({"local-projects":{}}));
        std::fs::write(proma.join("agent-workspaces.json"), serde_json::to_vec(&json!({
            "version":2,"workspaces":[{"id":"archived","slug":"archived","name":"Archived",
                "projectRootPath":project,"archived":true}]
        })).unwrap()).unwrap();
        let previous = discover_catalogs(Some(&codex), Some(&proma), &[]).unwrap();
        assert!(previous[0].archived && !previous[0].is_active());
        std::fs::write(codex.join(".codex-global-state.json"), "invalid").unwrap();
        let refreshed = discover_catalogs(Some(&codex), Some(&proma), &previous).unwrap();
        assert_eq!(refreshed.len(), 1);
        assert!(refreshed[0].archived && !refreshed[0].is_active());
    }

    #[test]
    fn project_shortcuts_use_shared_and_native_roots_without_creating_them() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("business repo");
        let first = temp.path().join("proma/first");
        let second = temp.path().join("proma/second");
        let entry = ProjectEntry {
            path: project.clone(),
            name: "Project".into(),
            agents: vec![AgentKind::Codex, AgentKind::Proma],
            proma_workspaces: vec![first.clone(), second.clone(), first.clone()],
            native_workspaces: Vec::new(), archive_states: Vec::new(),
            archived: false, hidden: false, retained: false,
        };
        let agent = |id| *crate::agents::AGENT_ICON_ORDER.iter()
            .find(|agent| agent.id == id).unwrap();
        assert_eq!(entry.skill_directories(agent("codex")), [
            project.join(".codex/skills"), project.join(".agents/skills"),
        ]);
        assert_eq!(entry.skill_directories(agent("proma")), [
            first.join("skills"), second.join("skills"),
        ]);
        assert_eq!(entry.skill_directories(agent("openclaw")), [
            project.join("skills"), project.join(".agents/skills"),
        ]);
        assert!(!project.exists());
        assert!(!first.exists());
        assert!(!second.exists());
    }

    #[cfg(unix)]
    #[test]
    fn project_shortcuts_deduplicate_aliases_without_changing_the_display_path() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let shared = project.join(".agents/skills");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::create_dir_all(project.join(".codex")).unwrap();
        std::os::unix::fs::symlink(&shared, project.join(".codex/skills")).unwrap();
        let entry = ProjectEntry {
            path: project.clone(), name: "Project".into(),
            agents: vec![AgentKind::Codex], proma_workspaces: Vec::new(), native_workspaces: Vec::new(), archive_states: Vec::new(), archived: false, hidden: false, retained: false,
        };
        let codex = *crate::agents::AGENT_ICON_ORDER.iter()
            .find(|agent| agent.id == "codex").unwrap();
        assert_eq!(entry.skill_directories(codex), [project.join(".codex/skills")]);
    }

    fn write_state(home: &Path, value: Value) {
        std::fs::write(
            home.join(".codex-global-state.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn proma_discovery_groups_by_business_root() {
        let temp = tempfile::tempdir().unwrap();
        let native = temp.path().join("agent-workspaces/ai-cube-pilot");
        let business = temp.path().join("business-repo");
        std::fs::create_dir_all(&native).unwrap();
        std::fs::create_dir_all(&business).unwrap();
        std::fs::write(temp.path().join("agent-workspaces.json"), serde_json::to_vec(&json!({
            "version": 1, "workspaces": [{"id": "pilot-id", "name": "试点工作区",
                "slug": "ai-cube-pilot", "projectRootPath": business}]
        })).unwrap()).unwrap();
        let projects = discover_proma_projects(temp.path()).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, path_identity(&business));
        assert_eq!(projects[0].name, "试点工作区");
        assert_eq!(projects[0].agents, [AgentKind::Proma]);
        assert_eq!(projects[0].proma_workspaces, [path_identity(&native)]);
        assert_ne!(projects[0].path, path_identity(&native));
    }

    #[cfg(unix)]
    #[test]
    fn multi_workspace_projects_use_business_name_and_keep_native_roots() {
        let temp = tempfile::tempdir().unwrap();
        let business = temp.path().join("shared-project");
        std::fs::create_dir(&business).unwrap();
        for slug in ["first", "second"] {
            std::fs::create_dir_all(temp.path().join("agent-workspaces").join(slug)).unwrap();
        }
        let first = json!({"id": "first", "slug": "first", "name": "演示工作区 A", "projectRootPath": business});
        let second = json!({"id": "second", "slug": "second", "name": "演示工作区 B", "projectRootPath": business});
        for workspaces in [vec![first.clone(), second.clone()], vec![second.clone(), first.clone()]] {
            std::fs::write(temp.path().join("agent-workspaces.json"),
                serde_json::to_vec(&json!({"version": 2, "workspaces": workspaces})).unwrap()).unwrap();
            let discovered = discover_proma_projects(temp.path()).unwrap();
            let projects = merge_projects(&[], &discovered, &BTreeSet::new());
            assert_eq!(projects.len(), 1);
            assert_eq!(projects[0].path, path_identity(&business));
            assert_eq!(projects[0].name, "shared-project");
            assert_eq!(projects[0].proma_workspaces.len(), 2);
            assert!(projects[0].proma_workspaces.contains(&path_identity(&temp.path().join("agent-workspaces/first"))));
            assert!(projects[0].proma_workspaces.contains(&path_identity(&temp.path().join("agent-workspaces/second"))));
        }
        std::fs::write(temp.path().join("agent-workspaces.json"),
            serde_json::to_vec(&json!({"version": 2, "workspaces": [first]})).unwrap()).unwrap();
        assert_eq!(discover_proma_projects(temp.path()).unwrap()[0].name, "演示工作区 A");
    }

    #[test]
    fn malformed_proma_catalog_does_not_hide_codex_projects() {
        let temp = tempfile::tempdir().unwrap();
        let codex = temp.path().join("codex");
        let proma = temp.path().join("proma");
        std::fs::create_dir(&codex).unwrap();
        std::fs::create_dir(&proma).unwrap();
        write_state(&codex, json!({"electron-saved-workspace-roots": [temp.path()]}));
        std::fs::write(proma.join("agent-workspaces.json"), "fixture-private").unwrap();
        let projects = discover_catalogs(Some(&codex), Some(&proma), &[]).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].agents, [AgentKind::Codex]);
    }

    #[cfg(unix)]
    #[test]
    fn linked_proma_and_codex_merge_manual_native_alias_and_hidden_identity() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("proma");
        let native = root.join("agent-workspaces/ai-agent");
        let business = temp.path().join("AI Agent");
        std::fs::create_dir_all(&native).unwrap();
        std::fs::create_dir(&business).unwrap();
        std::fs::write(root.join("agent-workspaces.json"), serde_json::to_vec(&json!({
            "version": 2, "workspaces": [{"id": "workspace", "slug": "ai-agent",
                "name": "AI Agent", "projectRootPath": business}]
        })).unwrap()).unwrap();
        write_state(temp.path(), json!({"local-projects": {
            "project": {"name": "AI Agent", "rootPaths": [business]}
        }}));
        let discovered = discover_catalogs(Some(temp.path()), Some(&root), &[]).unwrap();
        let merged = merge_projects(&[native.clone(), business.clone()], &discovered, &BTreeSet::new());
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].path, path_identity(&business));
        assert_eq!(merged[0].agents, [AgentKind::Codex, AgentKind::Proma]);
        assert_eq!(merged[0].proma_workspaces, [path_identity(&native)]);
        assert_eq!(project_identity(&native, &discovered), path_identity(&business));
        for hidden in [native.clone(), business.clone()] {
            assert!(merge_projects(&[native.clone()], &discovered, &BTreeSet::from([hidden])).is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn unlinked_or_invalid_business_roots_keep_distinct_native_workspaces() {
        let temp = tempfile::tempdir().unwrap();
        let workspaces = [
            json!({"id": "standalone", "slug": "standalone", "name": "Same name"}),
            json!({"id": "missing", "slug": "missing", "name": "Same name",
                "projectRootPath": temp.path().join("deleted-project")}),
            json!({"id": "relative", "slug": "relative", "name": "Same name", "projectRootPath": "relative"}),
        ];
        for slug in ["standalone", "missing", "relative"] {
            std::fs::create_dir_all(temp.path().join("agent-workspaces").join(slug)).unwrap();
        }
        std::fs::write(temp.path().join("agent-workspaces.json"),
            serde_json::to_vec(&json!({"version": 2, "workspaces": workspaces})).unwrap()).unwrap();
        let discovered = discover_proma_projects(temp.path()).unwrap();
        let merged = merge_projects(&[], &discovered, &BTreeSet::new());
        assert_eq!(merged.len(), 3);
        assert!(merged.iter().all(|entry| entry.path == entry.proma_workspaces[0]));
    }

    #[cfg(windows)]
    #[test]
    fn proma_discovery_rejects_unsupported_platform_without_changing_catalog() {
        let temp = tempfile::tempdir().unwrap();
        let native = temp.path().join("agent-workspaces/pilot");
        std::fs::create_dir_all(&native).unwrap();
        let catalog = temp.path().join("agent-workspaces.json");
        let bytes = serde_json::to_vec(&json!({"version": 2, "workspaces": [
            {"id": "pilot", "slug": "pilot", "name": "Pilot"}
        ]})).unwrap();
        std::fs::write(&catalog, &bytes).unwrap();
        let error = discover_proma_projects(temp.path()).unwrap_err();
        assert!(error.to_string().contains("此平台尚未支持 Proma 受管写入"));
        assert_eq!(std::fs::read(&catalog).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(native).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn linked_business_symlink_identity_merges_agents() {
        let temp = tempfile::tempdir().unwrap();
        let business = temp.path().join("actual");
        let alias = temp.path().join("alias");
        let native = temp.path().join("agent-workspaces/pilot");
        std::fs::create_dir_all(&native).unwrap();
        std::fs::create_dir(&business).unwrap();
        std::os::unix::fs::symlink(&business, &alias).unwrap();
        std::fs::write(temp.path().join("agent-workspaces.json"),
            serde_json::to_vec(&json!({"version": 1, "workspaces": [
                {"id": "pilot", "slug": "pilot", "name": "Proma title", "projectRootPath": alias}
            ]})).unwrap()).unwrap();
        write_state(temp.path(), json!({"electron-saved-workspace-roots": [business]}));
        let projects = discover_catalogs(Some(temp.path()), Some(temp.path()), &[]).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, path_identity(&business));
        assert_eq!(projects[0].agents, [AgentKind::Codex, AgentKind::Proma]);
    }

    #[test]
    fn current_catalog_uses_primary_root_and_ignores_stale_legacy() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("primary");
        let extra = temp.path().join("extra");
        let cloud_cache = temp.path().join(".chatgpt-projects/g-p-cache");
        std::fs::create_dir(&project).unwrap();
        std::fs::create_dir(&extra).unwrap();
        std::fs::create_dir_all(&cloud_cache).unwrap();
        write_state(
            temp.path(),
            json!({"local-projects": {
            "valid": {"name": "项目名称", "rootPaths": [project, extra]},
            "g-p-cloud": {"rootPaths": [extra]},
            "cache": {"rootPaths": [cloud_cache]},
            "deleted": {"rootPaths": [temp.path().join("missing"), extra]},
            "remote": {"rootPaths": ["ssh://host/project"]}
        }, "electron-saved-workspace-roots": [extra]}),
        );
        let found = discover_codex_projects(temp.path()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, path_identity(&project));
        assert_eq!(found[0].name, "项目名称");
    }

    #[test]
    fn empty_current_catalog_does_not_restore_removed_legacy_projects() {
        let temp = tempfile::tempdir().unwrap();
        write_state(
            temp.path(),
            json!({"local-projects": {}, "electron-saved-workspace-roots": [temp.path()]}),
        );
        assert!(discover_codex_projects(temp.path()).unwrap().is_empty());
    }

    #[test]
    fn legacy_catalog_filters_missing_and_relative_paths() {
        let temp = tempfile::tempdir().unwrap();
        let cloud_cache = temp.path().join(".chatgpt-projects/g-p-cache");
        std::fs::create_dir_all(&cloud_cache).unwrap();
        write_state(
            temp.path(),
            json!({"electron-saved-workspace-roots": [temp.path(), temp.path(), "relative", temp.path().join("missing"), cloud_cache]}),
        );
        let found = discover_codex_projects(temp.path()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].agents, [AgentKind::Codex]);
    }

    #[test]
    fn missing_or_invalid_state_does_not_expose_contents() {
        let temp = tempfile::tempdir().unwrap();
        assert!(discover_codex_projects(temp.path()).unwrap().is_empty());
        std::fs::write(
            temp.path().join(".codex-global-state.json"),
            "private contents",
        )
        .unwrap();
        let error = discover_codex_projects(temp.path())
            .unwrap_err()
            .to_string();
        assert!(!error.contains("private contents"));
    }

    #[test]
    fn shared_project_merges_agents_and_respects_hidden_projects() {
        let temp = tempfile::tempdir().unwrap();
        let path = path_identity(temp.path());
        let codex = ProjectEntry {
            path: path.clone(),
            name: "Shared".into(),
            agents: vec![AgentKind::Codex],
            proma_workspaces: Vec::new(), native_workspaces: Vec::new(), archive_states: Vec::new(), archived: false, hidden: false, retained: false,
        };
        let other = ProjectEntry {
            agents: vec![AgentKind::ClaudeCode, AgentKind::Codex],
            ..codex.clone()
        };
        let merged = merge_projects(&[path.clone()], &[codex.clone(), other], &BTreeSet::new());
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].agents, [AgentKind::Codex, AgentKind::ClaudeCode]);
        assert!(merge_projects(&[], &[codex], &BTreeSet::from([path])).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_and_real_directory_are_one_project() {
        let temp = tempfile::tempdir().unwrap();
        let actual = temp.path().join("real");
        let alias = temp.path().join("alias");
        std::fs::create_dir(&actual).unwrap();
        std::os::unix::fs::symlink(&actual, &alias).unwrap();
        write_state(
            temp.path(),
            json!({"electron-saved-workspace-roots": [actual, alias]}),
        );
        assert_eq!(discover_codex_projects(temp.path()).unwrap().len(), 1);
    }
}
