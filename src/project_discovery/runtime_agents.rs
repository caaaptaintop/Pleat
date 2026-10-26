//! Read-only runtime-agent project catalogs. This module only consumes paths,
//! explicit archive flags, and bounded Hermes session path metadata.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow};
use serde_json::Value;

use super::{AgentKind, ProjectEntry, path_identity, project_name};

const MAX_OPENCLAW_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_OPENCLAW_TOTAL_BYTES: u64 = 8 * 1024 * 1024;
const MAX_OPENCLAW_INCLUDE_DEPTH: usize = 10;
// Shared by the pre-parser scan and include resolution. Merging objects and
// concatenating arrays cannot increase the maximum depth of their operands.
const MAX_OPENCLAW_STRUCTURE_DEPTH: usize = 32;
const MAX_OPENCLAW_INCLUDE_PATH: usize = 4096;

// Values are NUL-delimited only inside SQLite's TEXT result; none contains a
// NUL in the native schemas. workspaces::sqlite_paths enforces its own exact
// SQL allowlist, schema validation, read-only connection, and row/byte bounds.
pub(super) const HERMES_PROJECTS_SQL: &str = "SELECT 'P' || char(0) || id || char(0) || COALESCE(primary_path, '') || char(0) || CAST(archived AS TEXT) || char(0) || COALESCE(name, '') FROM projects LIMIT 10001";
pub(super) const HERMES_FOLDERS_SQL: &str = "SELECT 'F' || char(0) || project_id || char(0) || path || char(0) || CAST(is_primary AS TEXT) || char(0) || CAST(added_at AS TEXT) FROM project_folders LIMIT 10001";
pub(super) const HERMES_SESSIONS_SQL: &str =
    "SELECT COALESCE(git_repo_root, '') || char(0) || COALESCE(cwd, '') FROM sessions LIMIT 10001";

pub(super) fn catalogs(home: &Path) -> Vec<(AgentKind, Result<Vec<ProjectEntry>>)> {
    vec![
        (AgentKind::OpenClaw, openclaw_catalog(home)),
        (AgentKind::Hermes, hermes_catalog(home)),
    ]
}

fn openclaw_catalog(home: &Path) -> Result<Vec<ProjectEntry>> {
    let (state_dir, config_path) = crate::agents::openclaw_paths(home)?;
    openclaw_catalog_at(&config_path, &state_dir, home)
}

fn openclaw_projects_from_config(
    config: &Value,
    state_dir: &Path,
    home: &Path,
) -> Result<Vec<ProjectEntry>> {
    let mut projects = BTreeMap::<PathBuf, ProjectEntry>::new();
    for (id, path) in crate::agents::openclaw_workspace_routes(config, state_dir, home)? {
        if !path.is_dir() { continue; }
        let path = path_identity(&path);
        let workspace = super::NativeWorkspace {
            agent: AgentKind::OpenClaw, source_id: Some(id), display_name: None, archived: None,
            path: path.clone(), project_roots: std::sync::Arc::new(vec![path.clone()]),
        };
        let entry = projects.entry(path.clone()).or_insert_with(|| ProjectEntry {
            name: project_name(&path), path, agents: vec![AgentKind::OpenClaw],
            proma_workspaces: Vec::new(), native_workspaces: Vec::new(),
            archive_states: Vec::new(), archived: false, hidden: false, retained: false,
        });
        if !entry.native_workspaces.iter().any(|old| old.id() == workspace.id()) {
            entry.native_workspaces.push(workspace);
        }
    }
    Ok(projects.into_values().collect())
}

fn openclaw_catalog_at(
    config_path: &Path,
    state_dir: &Path,
    home: &Path,
) -> Result<Vec<ProjectEntry>> {
    // Missing configuration is not an archive signal: preserve the previous
    // catalog instead of inferring ownership from a leftover workspace folder.
    let snapshot = crate::agents::openclaw_snapshot_at(home, state_dir, config_path)
        .map_err(|failure| anyhow!("{}", failure.message()))?;
    openclaw_projects_from_config(&snapshot.config, state_dir, home)
}

/// Read and resolve OpenClaw JSON5 configuration without exposing parser or IO
/// errors, which can contain arbitrary configuration text.
pub(crate) fn openclaw_config_at(path: &Path) -> Result<Value> {
    let mut state = IncludeState {
        total_bytes: 0,
        allowed_roots: include_roots(path),
        stack: BTreeSet::new(),
    };
    read_openclaw_config(path, 0, 0, &mut state)
}

struct IncludeState {
    total_bytes: u64,
    allowed_roots: Vec<PathBuf>,
    stack: BTreeSet<PathBuf>,
}

fn include_roots(config_path: &Path) -> Vec<PathBuf> {
    let mut roots = vec![
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
    ];
    if let Some(raw) = std::env::var_os("OPENCLAW_INCLUDE_ROOTS") {
        roots.extend(std::env::split_paths(&raw).filter(|root| root.is_absolute()));
    }
    roots
}

fn check_openclaw_structure(text: &str, parent_depth: usize) -> Result<()> {
    check_json5_structure(text, parent_depth).map_err(|_| anyhow!("OpenClaw 配置结构深度超限"))
}

/// Count only containers outside JSON5 strings/comments, before Pest or Value
/// gets a chance to recurse. The parser still validates the actual grammar.
pub(super) fn check_json5_structure(text: &str, parent_depth: usize) -> Result<()> {
    let mut depth = parent_depth;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\'' | '"' => {
                while let Some(next) = chars.next() {
                    if next == '\\' {
                        chars.next();
                    } else if next == ch {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'/') => {
                chars.next();
                for next in chars.by_ref() {
                    if matches!(next, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                while let Some(next) = chars.next() {
                    if next == '*' && chars.peek() == Some(&'/') {
                        chars.next();
                        break;
                    }
                }
            }
            '{' | '[' => {
                depth += 1;
                if depth > MAX_OPENCLAW_STRUCTURE_DEPTH {
                    return Err(anyhow!("JSON5 配置结构深度超限"));
                }
            }
            '}' | ']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

fn read_openclaw_config(
    path: &Path,
    depth: usize,
    parent_depth: usize,
    state: &mut IncludeState,
) -> Result<Value> {
    if depth > MAX_OPENCLAW_INCLUDE_DEPTH {
        return Err(anyhow!("OpenClaw include 深度超限"));
    }
    let canonical = fs::canonicalize(path).map_err(|_| anyhow!("OpenClaw 配置文件不可读"))?;
    if !state.stack.insert(canonical.clone()) {
        return Err(anyhow!("OpenClaw include 存在循环"));
    }
    let result = (|| {
        let metadata = fs::metadata(&canonical).map_err(|_| anyhow!("OpenClaw 配置文件不可读"))?;
        if !metadata.is_file() || metadata.len() > MAX_OPENCLAW_FILE_BYTES {
            return Err(anyhow!("OpenClaw 配置文件超出读取限制"));
        }
        let mut file =
            fs::File::open(&canonical).map_err(|_| anyhow!("OpenClaw 配置文件不可读"))?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.by_ref()
            .take(MAX_OPENCLAW_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| anyhow!("OpenClaw 配置文件不可读"))?;
        let actual_len = bytes.len() as u64;
        if actual_len != metadata.len() || actual_len > MAX_OPENCLAW_FILE_BYTES {
            return Err(anyhow!("OpenClaw 配置文件超出读取限制"));
        }
        state.total_bytes = state
            .total_bytes
            .checked_add(actual_len)
            .filter(|size| *size <= MAX_OPENCLAW_TOTAL_BYTES)
            .ok_or_else(|| anyhow!("OpenClaw 配置总读取量超限"))?;
        let text = std::str::from_utf8(&bytes).map_err(|_| anyhow!("OpenClaw 配置编码无效"))?;
        check_openclaw_structure(text, parent_depth)?;
        let parsed: Value =
            json5::from_str(text).map_err(|_| anyhow!("OpenClaw JSON5 配置无效"))?;
        resolve_openclaw_value(parsed, &canonical, depth, parent_depth, state)
    })();
    state.stack.remove(&canonical);
    result
}

fn resolve_openclaw_value(
    value: Value,
    containing_file: &Path,
    depth: usize,
    parent_depth: usize,
    state: &mut IncludeState,
) -> Result<Value> {
    match value {
        Value::Array(values) => values
            .into_iter()
            .map(|value| {
                resolve_openclaw_value(value, containing_file, depth, parent_depth + 1, state)
            })
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        Value::Object(mut object) if object.contains_key("$include") => {
            let (includes, multiple) = match object.remove("$include") {
                Some(Value::String(path)) => (vec![path], false),
                Some(Value::Array(values)) => (
                    values
                        .into_iter()
                        .map(|value| match value {
                            Value::String(path) => Ok(path),
                            _ => Err(anyhow!("OpenClaw $include 项格式无效")),
                        })
                        .collect::<Result<Vec<_>>>()?,
                    true,
                ),
                _ => return Err(anyhow!("OpenClaw $include 格式无效")),
            };
            let mut included = if !multiple {
                let path = resolve_include_path(&includes[0], containing_file, state)?;
                read_openclaw_config(&path, depth + 1, parent_depth, state)?
            } else {
                Value::Object(serde_json::Map::new())
            };
            if multiple {
                for include in includes {
                    let path = resolve_include_path(&include, containing_file, state)?;
                    let next = read_openclaw_config(&path, depth + 1, parent_depth, state)?;
                    included = deep_merge(included, next);
                }
            }
            let mut siblings = serde_json::Map::new();
            for (key, child) in object {
                siblings.insert(
                    key,
                    resolve_openclaw_value(child, containing_file, depth, parent_depth + 1, state)?,
                );
            }
            if siblings.is_empty() {
                Ok(included)
            } else if included.is_object() {
                Ok(deep_merge(included, Value::Object(siblings)))
            } else {
                Err(anyhow!("OpenClaw $include 兄弟字段要求对象内容"))
            }
        }
        Value::Object(object) => object
            .into_iter()
            .map(|(key, value)| {
                resolve_openclaw_value(value, containing_file, depth, parent_depth + 1, state)
                    .map(|value| (key, value))
            })
            .collect::<Result<serde_json::Map<_, _>>>()
            .map(Value::Object),
        primitive => Ok(primitive),
    }
}

fn resolve_include_path(
    raw: &str,
    containing_file: &Path,
    state: &IncludeState,
) -> Result<PathBuf> {
    if raw.contains('\0') || raw.len() >= MAX_OPENCLAW_INCLUDE_PATH {
        return Err(anyhow!("OpenClaw include 路径无效"));
    }
    let target = Path::new(raw);
    let resolved = if target.is_absolute() {
        target.to_path_buf()
    } else {
        containing_file
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(target)
    };
    if resolved.to_string_lossy().len() >= MAX_OPENCLAW_INCLUDE_PATH {
        return Err(anyhow!("OpenClaw include 路径无效"));
    }
    let canonical =
        fs::canonicalize(&resolved).map_err(|_| anyhow!("OpenClaw include 文件不可读"))?;
    if !state.allowed_roots.iter().any(|root| {
        fs::canonicalize(root)
            .map(|root| canonical.starts_with(root))
            .unwrap_or(false)
    }) {
        return Err(anyhow!("OpenClaw include 路径越界"));
    }
    Ok(canonical)
}

fn deep_merge(target: Value, source: Value) -> Value {
    match (target, source) {
        (Value::Object(mut target), Value::Object(source)) => {
            for (key, value) in source {
                let merged = target
                    .remove(&key)
                    .map(|old| deep_merge(old, value.clone()))
                    .unwrap_or(value);
                target.insert(key, merged);
            }
            Value::Object(target)
        }
        (Value::Array(mut target), Value::Array(source)) => {
            target.extend(source);
            Value::Array(target)
        }
        (_, source) => source,
    }
}

fn absolute_user_path(raw: &Path, home: &Path) -> Result<PathBuf> {
    let text = raw.to_string_lossy();
    let path = if text == "~" {
        home.to_path_buf()
    } else if let Some(suffix) = text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\")) {
        home.join(suffix)
    } else {
        raw.to_path_buf()
    };
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(anyhow!("Agent 原生路径不是绝对路径"))
    }
}

fn hermes_catalog(home: &Path) -> Result<Vec<ProjectEntry>> {
    let hermes_home = hermes_home(home)?;
    hermes_catalog_at(&hermes_home)
}

fn hermes_catalog_at(hermes_home: &Path) -> Result<Vec<ProjectEntry>> {
    let projects_db = hermes_home.join("projects.db");
    let present = match fs::metadata(&projects_db) {
        Ok(metadata) if metadata.is_file() => true,
        Ok(_) => return Err(anyhow!("Hermes projects.db 来源不是文件，状态未知")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && projects_db.symlink_metadata().is_err() => false,
        Err(_) => return Err(anyhow!("Hermes projects.db 来源不可读，状态未知")),
    };
    if present {
        let rows = super::workspaces::sqlite_paths(
            &projects_db,
            &[HERMES_PROJECTS_SQL, HERMES_FOLDERS_SQL],
        )?;
        return hermes_explicit_projects(rows, &projects_db);
    }

    let state_db = hermes_home.join("state.db");
    match fs::metadata(&state_db) {
        Ok(metadata) if metadata.is_file() => {},
        Ok(_) => return Err(anyhow!("Hermes state.db 来源不是文件，状态未知")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && state_db.symlink_metadata().is_err() => return Ok(Vec::new()),
        Err(_) => return Err(anyhow!("Hermes state.db 来源不可读，状态未知")),
    }
    let rows = super::workspaces::sqlite_paths(&state_db, &[HERMES_SESSIONS_SQL])?;
    let mut paths = Vec::new();
    for row in rows {
        let Some((repo, cwd)) = row.split_once('\0') else {
            return Err(anyhow!("Hermes 会话路径元数据格式无效"));
        };
        let repository = Path::new(repo);
        let selected = if repository.is_absolute() && repository.is_dir() {
            repo
        } else {
            cwd
        };
        paths.push(selected.to_owned());
    }
    existing_projects(
        paths.into_iter().map(PathBuf::from).collect(),
        AgentKind::Hermes,
    )
}

fn hermes_home(home: &Path) -> Result<PathBuf> {
    crate::agents::hermes_home(home)
}

fn hermes_explicit_projects(rows: Vec<String>, source: &Path) -> Result<Vec<ProjectEntry>> {
    let mut projects = BTreeMap::<String, (String, bool, Option<String>)>::new();
    let mut folders = BTreeMap::<String, Vec<(String, bool, i64)>>::new();
    for row in rows {
        let mut fields = row.split('\0');
        match fields.next() {
            Some("P") => {
                let (Some(id), Some(primary), Some(archived), Some(name), None) =
                    (fields.next(), fields.next(), fields.next(), fields.next(), fields.next())
                else {
                    return Err(anyhow!("Hermes 项目记录格式无效"));
                };
                let archived = parse_flag(archived)?;
                if projects
                    .insert(id.to_owned(), (primary.to_owned(), archived, (!name.trim().is_empty()).then(|| name.to_owned())))
                    .is_some()
                {
                    return Err(anyhow!("Hermes 项目记录重复"));
                }
            }
            Some("F") => {
                let (Some(id), Some(path), Some(primary), Some(added), None) = (
                    fields.next(),
                    fields.next(),
                    fields.next(),
                    fields.next(),
                    fields.next(),
                ) else {
                    return Err(anyhow!("Hermes 项目文件夹记录格式无效"));
                };
                folders.entry(id.to_owned()).or_default().push((
                    path.to_owned(),
                    parse_flag(primary)?,
                    added
                        .parse()
                        .map_err(|_| anyhow!("Hermes 项目文件夹记录格式无效"))?,
                ));
            }
            _ => return Err(anyhow!("Hermes 项目记录格式无效")),
        }
    }
    let mut projects_out = BTreeMap::<PathBuf, ProjectEntry>::new();
    for (id, (primary_path, archived, display_name)) in projects {
        let folder_rows = folders.remove(&id).unwrap_or_default();
        let preferred = folder_rows
            .iter()
            .find(|(_, is_primary, _)| *is_primary)
            .or_else(|| folder_rows.iter().min_by_key(|(_, _, added)| added));
        let mut candidates = Vec::with_capacity(folder_rows.len() + 1);
        if !primary_path.trim().is_empty() {
            candidates.push(primary_path);
        }
        if let Some((path, _, _)) = preferred {
            candidates.push(path.clone());
        }
        for (path, _, _) in folder_rows {
            if !candidates.contains(&path) {
                candidates.push(path);
            }
        }
        let mut members = BTreeMap::new();
        for path in candidates {
            if let Some(project) = project_for_native_path(&path, AgentKind::Hermes, Some(archived)) {
                members.insert(project.path.clone(), project);
            }
        }
        let workspace = super::NativeWorkspace { agent: AgentKind::Hermes, source_id: Some(id), display_name, archived: Some(archived),
            path: path_identity(source), project_roots: std::sync::Arc::new(members.keys().cloned().collect()) };
        for (path, mut project) in members {
            project.native_workspaces.push(workspace.clone());
            if let Some(existing) = projects_out.get_mut(&path) {
                existing.merge_archival(&project);
                existing.native_workspaces.push(workspace.clone());
            } else { projects_out.insert(path, project); }
        }
    }
    Ok(projects_out.into_values().collect())
}

fn parse_flag(value: &str) -> Result<bool> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(anyhow!("Hermes 项目标记格式无效")),
    }
}

fn existing_projects(paths: Vec<PathBuf>, agent: AgentKind) -> Result<Vec<ProjectEntry>> {
    let mut projects = Vec::new();
    let mut seen = BTreeSet::new();
    for path in paths {
        let Some(project) = project_for_native_path(&path.to_string_lossy(), agent, None) else {
            continue;
        };
        if seen.insert(project.path.clone()) {
            projects.push(project);
        }
    }
    Ok(projects)
}

fn project_for_native_path(raw: &str, agent: AgentKind, archived: Option<bool>) -> Option<ProjectEntry> {
    let path = Path::new(raw);
    if !path.is_absolute() || !path.is_dir() {
        return None;
    }
    let canonical = path_identity(path);
    let mut project = super::native::project(&canonical.to_string_lossy(), agent)?;
    project.name = project_name(&canonical);
    if let Some(archived) = archived { project.record_archive(agent, archived); }
    Some(project)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn hermes_nonfile_and_malformed_sources_are_not_empty_catalogs() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path();
        assert!(hermes_catalog_at(profile).unwrap().is_empty());
        fs::create_dir(profile.join("projects.db")).unwrap();
        assert!(hermes_catalog_at(profile).is_err());
        fs::remove_dir(profile.join("projects.db")).unwrap();
        fs::write(profile.join("projects.db"), "not sqlite").unwrap();
        assert!(hermes_catalog_at(profile).is_err());
        fs::remove_file(profile.join("projects.db")).unwrap();
        fs::create_dir(profile.join("state.db")).unwrap();
        assert!(hermes_catalog_at(profile).is_err());
    }

    #[test]
    fn hermes_explicit_projects_group_folders_and_keep_archived_state() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let rows = vec![
            ["P", "p1", "", "1", "Native project title"].join("\0"),
            ["F", "p1", first.to_str().unwrap(), "1", "10"].join("\0"),
            ["F", "p1", second.to_str().unwrap(), "0", "20"].join("\0"),
        ];
        let projects = hermes_explicit_projects(rows, &root.join("projects.db")).unwrap();
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].path, first);
        assert!(projects.iter().all(|entry| entry.archived));
        assert!(projects.iter().all(|entry| entry.native_workspaces[0].source_id.as_deref() == Some("p1")));
        assert!(projects.iter().all(|entry| entry.native_workspaces[0].display_name.as_deref() == Some("Native project title")));
        assert_eq!(projects[0].native_workspaces[0].project_roots.as_ref(), &[first, second]);
        assert!(std::sync::Arc::ptr_eq(&projects[0].native_workspaces[0].project_roots, &projects[1].native_workspaces[0].project_roots));
    }

    #[test]
    fn hermes_shared_folder_keeps_each_native_project_id_and_archive_state() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let rows = vec![["P", "p1", root.to_str().unwrap(), "1", "Archived project"].join("\0"),
            ["P", "p2", root.to_str().unwrap(), "0", "Active project"].join("\0")];
        let projects = hermes_explicit_projects(rows, &root.join("projects.db")).unwrap();
        assert_eq!(projects.len(), 1);
        assert!(!projects[0].archived);
        let contexts = &projects[0].native_workspaces;
        assert_eq!(contexts.len(), 2);
        assert_ne!(contexts[0].id(), contexts[1].id());
        assert_eq!(contexts[0].archived, Some(true));
        assert_eq!(contexts[1].archived, Some(false));
        let json = serde_json::to_value(&projects).unwrap();
        assert!(json[0]["native_workspaces"][0].get("project_roots").is_none(),
            "CLI membership uses project rows, not quadratic repeated root lists");
    }

    #[test]
    fn invalid_and_duplicate_paths_are_skipped_and_deduplicated() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let project = root.join("project");
        fs::create_dir(&project).unwrap();
        let projects = existing_projects(
            vec![
                project.clone(),
                project.join("."),
                root.join("missing"),
                PathBuf::from("relative"),
                root.join("ordinary-file"),
            ],
            AgentKind::OpenClaw,
        )
        .unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, project);
    }

    #[test]
    fn openclaw_reads_configured_agent_roster_workspaces_from_real_json() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let shared = root.join("shared");
        let child = shared.join("agent-b");
        fs::create_dir_all(&child).unwrap();
        let config_path = root.join("openclaw.json");
        fs::write(
            &config_path,
            serde_json::to_vec(&serde_json::json!({
                "agents": {
                    "defaults": {"workspace": shared},
                    "ownership": "explicit",
                    "entries": {"agent-a": {}, "agent-b": {}}
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let config = openclaw_config_at(&config_path).unwrap();
        let projects = openclaw_projects_from_config(&config, &root, &root).unwrap();
        assert_eq!(
            projects
                .iter()
                .map(|p| p.path.clone())
                .collect::<BTreeSet<_>>(),
            [child].into()
        );
    }

    #[test]
    fn openclaw_json5_includes_merge_relative_files_and_detect_cycles() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::write(
            root.join("base.json5"),
            "{ agents: { defaults: { workspace: './shared' }, entries: { main: {} } }, list: [1] }",
        )
        .unwrap();
        fs::create_dir(root.join("shared")).unwrap();
        let config = root.join("openclaw.json");
        fs::write(
            &config,
            "{ $include: ['base.json5'], list: [2], extra: { ok: true, }, }",
        )
        .unwrap();
        let parsed = openclaw_config_at(&config).unwrap();
        assert_eq!(parsed["list"], serde_json::json!([1, 2]));
        assert_eq!(parsed["extra"]["ok"], true);

        fs::write(&config, "{ $include: 'loop.json5' }").unwrap();
        fs::write(root.join("loop.json5"), "{ $include: 'openclaw.json' }").unwrap();
        assert_eq!(
            openclaw_config_at(&config).unwrap_err().to_string(),
            "OpenClaw include 存在循环"
        );
    }

    #[test]
    fn openclaw_requires_config_before_discovering_default_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let state_dir = root.join(".openclaw");
        let workspace = state_dir.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        for config_path in [state_dir.join("openclaw.json"), root.join("explicit.json5")] {
            assert_eq!(
                openclaw_catalog_at(&config_path, &state_dir, &root)
                    .unwrap_err()
                    .to_string(),
                "OpenClaw 配置缺失，技能生效状态无法确认"
            );
        }
        let config_path = state_dir.join("openclaw.json");
        fs::write(&config_path, "{}").unwrap();
        let projects = openclaw_catalog_at(&config_path, &state_dir, &root).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, workspace);
        assert!(!projects[0].archived);
    }

    #[test]
    fn openclaw_structure_scan_skips_json5_quotes_escapes_and_comments() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("quotes.json5");
        let brackets = "[{".repeat(MAX_OPENCLAW_STRUCTURE_DEPTH * 2);
        let text = format!(
            r#"{{
                single: 'escaped \' quote " and \\ {brackets}',
                double: "escaped \" quote ' and \\ {brackets}",
                continued: 'line\
{brackets}',
                // ' " {brackets}
                /* ' " {brackets} */
                nested: [{{ ok: true }}],
            }}"#
        );
        fs::write(&path, text).unwrap();
        let config = openclaw_config_at(&path).unwrap();
        assert_eq!(config["nested"][0]["ok"], true);
        // JSON5 also terminates single-line comments at Unicode line separators.
        for terminator in ['\r', '\u{2028}', '\u{2029}'] {
            assert!(
                check_openclaw_structure(&format!("// ignored{terminator}{}", "[".repeat(33)), 0)
                    .is_err()
            );
        }
    }

    #[test]
    fn openclaw_rejects_deep_body_before_json5_parsing() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("deep.json5");
        // Far below 2 MiB, but enough to overflow an unguarded recursive parser.
        for (open, close) in [("[", "]"), ("{x:", "}")] {
            let text = format!("{}0{}", open.repeat(100_000), close.repeat(100_000));
            assert!(text.len() < MAX_OPENCLAW_FILE_BYTES as usize);
            fs::write(&path, text).unwrap();
            assert_eq!(
                openclaw_config_at(&path).unwrap_err().to_string(),
                "OpenClaw 配置结构深度超限"
            );
        }
        let text = format!(
            "{}0{}",
            "[".repeat(MAX_OPENCLAW_STRUCTURE_DEPTH),
            "]".repeat(MAX_OPENCLAW_STRUCTURE_DEPTH)
        );
        fs::write(&path, text).unwrap();
        assert!(openclaw_config_at(&path).is_ok());
    }

    #[test]
    fn openclaw_include_uses_remaining_structure_depth() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let path = root.join("openclaw.json");
        fs::write(&path, "{ child: { $include: 'body.json5' } }").unwrap();
        // Both files are individually within the limit; their combined depth
        // must still be checked before resolving or merging the included body.
        for (depth, accepted) in [(31, true), (32, false)] {
            fs::write(
                root.join("body.json5"),
                format!("{}0{}", "[".repeat(depth), "]".repeat(depth)),
            )
            .unwrap();
            let result = openclaw_config_at(&path);
            if accepted {
                assert!(result.is_ok());
            } else {
                assert_eq!(result.unwrap_err().to_string(), "OpenClaw 配置结构深度超限");
            }
        }
    }

    #[test]
    fn hermes_home_variable_expansion_preserves_path_separator() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let path_value = std::env::var("PATH").unwrap();
        let expected = PathBuf::from(format!("{path_value}/hermes/data"));
        let actual = crate::agents::hermes::expand_path("$PATH/hermes/data", home, &std::env::current_dir().unwrap()).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn hermes_queries_are_read_only_bounded_and_only_project_path_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let project = root.join("project");
        fs::create_dir(&project).unwrap();
        let db = root.join("projects.db");
        let connection = Connection::open(&db).unwrap();
        connection.execute_batch(
            "CREATE TABLE projects(id TEXT PRIMARY KEY, name TEXT NOT NULL DEFAULT '', primary_path TEXT, archived INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE project_folders(project_id TEXT NOT NULL, path TEXT NOT NULL, is_primary INTEGER NOT NULL DEFAULT 0, added_at INTEGER NOT NULL);
             CREATE TABLE messages(content TEXT, title TEXT);
             INSERT INTO messages VALUES('never read', 'never read');",
        ).unwrap();
        connection
            .execute(
                "INSERT INTO projects(id,primary_path,archived) VALUES('p1','',1)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO project_folders VALUES('p1',?1,1,1)",
                [project.to_string_lossy().as_ref()],
            )
            .unwrap();
        let before = fs::read(&db).unwrap();
        drop(connection);
        let rows = crate::project_discovery::workspaces::sqlite_paths(&db, &[HERMES_PROJECTS_SQL, HERMES_FOLDERS_SQL])
            .unwrap();
        let projects = hermes_explicit_projects(rows, &db).unwrap();
        assert_eq!(projects.len(), 1);
        assert!(projects[0].archived);
        assert_eq!(fs::read(&db).unwrap(), before);
    }

    #[test]
    fn hermes_session_fallback_prefers_repository_root_and_uses_cwd_when_needed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let repository = root.join("repo");
        let cwd = root.join("cwd");
        fs::create_dir(&repository).unwrap();
        fs::create_dir(&cwd).unwrap();
        let db = root.join("state.db");
        let connection = Connection::open(&db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE sessions(cwd TEXT, git_repo_root TEXT);
             CREATE TABLE messages(content TEXT, title TEXT);
             INSERT INTO messages VALUES('never read', 'never read');",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO sessions VALUES(?1,?2)",
                [
                    cwd.to_string_lossy().as_ref(),
                    repository.to_string_lossy().as_ref(),
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO sessions VALUES(?1,?2)",
                [
                    cwd.to_string_lossy().as_ref(),
                    root.join("missing").to_string_lossy().as_ref(),
                ],
            )
            .unwrap();
        let before = fs::read(&db).unwrap();
        drop(connection);
        let rows = crate::project_discovery::workspaces::sqlite_paths(&db, &[HERMES_SESSIONS_SQL]).unwrap();
        let mut paths = Vec::new();
        for row in rows {
            let (repo, cwd) = row.split_once('\0').unwrap();
            let repository = Path::new(repo);
            paths.push(if repository.is_absolute() && repository.is_dir() {
                repo.to_owned()
            } else {
                cwd.to_owned()
            });
        }
        let projects = existing_projects(
            paths.into_iter().map(PathBuf::from).collect(),
            AgentKind::Hermes,
        )
        .unwrap();
        assert_eq!(
            projects
                .iter()
                .map(|p| p.path.clone())
                .collect::<BTreeSet<_>>(),
            [repository, cwd].into()
        );
        assert_eq!(fs::read(&db).unwrap(), before);
    }
}
