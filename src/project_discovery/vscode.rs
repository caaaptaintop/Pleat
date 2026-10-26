//! Cursor 3.24.18 metadata protocol, verified against its signed main.js.
//! No composer/session databases, settings, labels, or credential projections.
use super::{AgentKind, ProjectEntry};
use anyhow::{Result, anyhow, bail};
use rusqlite::types::ValueRef;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, Metadata, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

const MAX_BYTES: usize = 1024 * 1024;
const MAX_PATH: usize = 16 * 1024;
const MAX_ENTRIES: usize = 10_000;
const PROJECTS_SQL: &str =
    "SELECT value FROM ItemTable WHERE key = 'glass.localAgentProjects.v1' LIMIT 2";
const HISTORY_SQL: &str =
    "SELECT value FROM ItemTable WHERE key = 'history.recentlyOpenedPathsList' LIMIT 2";

/// The native workspace configuration remains separate from its member cwd rows.
/// Callers must retain this mapping if exposing workspace-level management.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkspaceRegistration {
    pub config_path: PathBuf,
    pub folders: Vec<PathBuf>,
    pub archived: Option<bool>,
}

#[derive(Debug)]
pub(super) struct CursorCatalog {
    pub projects: Vec<ProjectEntry>,
    pub workspaces: Vec<WorkspaceRegistration>,
}

/// Default official macOS root, or an explicitly selected native userData root.
/// Never guess another process's --user-data-dir or portable configuration.
pub(super) fn cursor_projects(home: &Path) -> Result<Vec<ProjectEntry>> {
    let root = match std::env::var_os("KITTER_CURSOR_PROJECTS_USER_DATA") {
        Some(root) if !root.is_empty() && Path::new(&root).is_absolute() => PathBuf::from(root),
        Some(_) => bail!("Cursor 项目数据根必须是非空绝对路径"),
        None => home.join("Library/Application Support/Cursor"),
    };
    cursor_projects_at(&root)
}

/// Explicit resolved userData root for isolated verification / known overrides.
pub(super) fn cursor_projects_at(user_data: &Path) -> Result<Vec<ProjectEntry>> {
    editor_projects_at(user_data, AgentKind::Cursor)
}

/// Used only by editor products whose native history protocol was verified.
pub(super) fn editor_projects_at(user_data: &Path, agent: AgentKind) -> Result<Vec<ProjectEntry>> {
    let catalog = editor_catalog_at(user_data, agent)?;
    let mut projects = catalog.projects;
    let positions = projects.iter().enumerate().map(|(index, entry)| (entry.path.clone(), index)).collect::<BTreeMap<_, _>>();
    for workspace in catalog.workspaces {
        let context = super::NativeWorkspace { agent, source_id: None, display_name: None, archived: workspace.archived,
            path: workspace.config_path, project_roots: std::sync::Arc::new(workspace.folders) };
        for root in context.project_roots.iter() {
            if let Some(index) = positions.get(root) { projects[*index].native_workspaces.push(context.clone()); }
        }
    }
    Ok(projects)
}

#[derive(Default, Deserialize, Serialize)]
pub(super) struct History {
    // An absent storage key defaults in read_history_local; a present value
    // must prove the native entries schema, even when the list is empty.
    entries: Vec<Recent>,
}
#[derive(Deserialize, Serialize)]
struct Recent {
    #[serde(rename = "folderUri")]
    folder_uri: Option<String>,
    workspace: Option<WorkspaceUri>,
    #[serde(rename = "remoteAuthority")]
    remote_authority: Option<String>,
}
#[derive(Deserialize, Serialize)]
struct WorkspaceUri {
    id: String,
    #[serde(rename = "configPath")]
    config_path: String,
}
#[derive(Deserialize)]
struct Sidecar {
    folder: Option<String>,
    workspace: Option<String>,
}
#[derive(Deserialize)]
struct WorkspaceFile {
    #[serde(default)]
    folders: Vec<Folder>,
}
#[derive(Deserialize)]
struct Folder {
    path: Option<String>,
    uri: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub(super) struct NativeProject {
    workspace: NativeWorkspace,
    #[serde(default, rename = "isArchived")]
    archived: bool,
}
#[derive(Deserialize, Serialize)]
struct NativeWorkspace {
    uri: Option<UriComponents>,
    #[serde(rename = "configPath")]
    config_path: Option<UriComponents>,
}
#[derive(Deserialize, Serialize)]
struct UriComponents {
    scheme: String,
    path: String,
    #[serde(default)]
    authority: String,
}
impl UriComponents {
    fn local_path(self) -> Result<Option<PathBuf>> {
        check_path(&self.path)?;
        // URI.toJSON stores the already-decoded path. Never decode it twice.
        Ok((self.scheme == "file"
            && self.authority.is_empty()
            && Path::new(&self.path).is_absolute())
        .then(|| PathBuf::from(self.path)))
    }
}

pub(super) fn cursor_catalog_at(user_data: &Path) -> Result<CursorCatalog> {
    editor_catalog_at(user_data, AgentKind::Cursor)
}

fn editor_catalog_at(user_data: &Path, agent: AgentKind) -> Result<CursorCatalog> {
    if !user_data.is_absolute() || !user_data.is_dir() {
        bail!("Cursor 数据根暂不可读");
    }
    let db = user_data.join("User/globalStorage/state.vscdb");
    let mut read_budget = 32 * MAX_BYTES;
    let mut folders = BTreeSet::new();
    let mut configs = BTreeSet::new();
    let mut native_folders = BTreeMap::new();
    let mut native_configs = BTreeMap::new();
    if db
        .try_exists()
        .map_err(|_| anyhow!("Cursor 项目索引暂不可读"))?
    {
        let (history, native) = read_history(&db, agent == AgentKind::Cursor)?;
        if native.len() > MAX_ENTRIES {
            bail!("Cursor 原生项目数量超过读取上限");
        }
        for entry in native {
            if let Some(uri) = entry.workspace.uri {
                if let Some(path) = uri.local_path()?.and_then(existing_directory) {
                    folders.insert(path.clone());
                    merge_archival(&mut native_folders, path, entry.archived);
                }
            } else if let Some(uri) = entry.workspace.config_path {
                if let Some(path) = uri.local_path()? {
                    if let Ok(path) = path.canonicalize() {
                        configs.insert(path.clone());
                        merge_archival(&mut native_configs, path, entry.archived);
                    }
                }
            }
        }
        if history.entries.len() > MAX_ENTRIES {
            bail!("Cursor 项目数量超过读取上限");
        }
        for entry in history.entries {
            if entry
                .remote_authority
                .as_deref()
                .is_some_and(|s| !s.is_empty())
            {
                continue;
            }
            // Match the native restorer's workspace > folder > file precedence.
            if let Some(workspace) = entry.workspace {
                let _native_id = workspace.id;
                insert_uri(&mut configs, &workspace.config_path)?;
            } else if let Some(folder) = entry.folder_uri {
                insert_uri(&mut folders, &folder)?;
            }
        }
    }
    // Proven native workspace identifiers only; never open per-workspace DBs.
    let storage = user_data.join("User/workspaceStorage");
    if storage
        .try_exists()
        .map_err(|_| anyhow!("Cursor 工作区索引暂不可读"))?
    {
        let entries = fs::read_dir(storage).map_err(|_| anyhow!("Cursor 工作区索引暂不可读"))?;
        for (count, entry) in entries.enumerate() {
            if count >= MAX_ENTRIES {
                bail!("Cursor 工作区数量超过读取上限");
            }
            let entry = entry.map_err(|_| anyhow!("Cursor 工作区索引暂不可读"))?;
            if !entry
                .file_type()
                .map_err(|_| anyhow!("Cursor 工作区索引暂不可读"))?
                .is_dir()
            {
                continue;
            }
            let meta = entry.path().join("workspace.json");
            // Do not resolve this fixed metadata leaf as a workspace alias.
            match fs::symlink_metadata(&meta) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => bail!("Cursor 工作区索引暂不可读"),
                Ok(_) => {}
            }
            let sidecar: Sidecar = serde_json::from_slice(&read_budgeted(&meta, &mut read_budget)?)
                .map_err(|_| anyhow!("Cursor 工作区索引格式无效"))?;
            if let Some(uri) = sidecar.folder {
                insert_uri(&mut folders, &uri)?;
            } else if let Some(uri) = sidecar.workspace {
                insert_uri(&mut configs, &uri)?;
            }
        }
    }
    let mut workspaces = Vec::new();
    let mut total_members = 0usize;
    for config_path in configs {
        if !config_path.is_file() {
            continue;
        }
        let config_path =
            fs::canonicalize(config_path).map_err(|_| anyhow!("Cursor 工作区配置暂不可读"))?;
        let bytes = read_budgeted(&config_path, &mut read_budget)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| anyhow!("Cursor 工作区配置格式无效"))?;
        super::runtime_agents::check_json5_structure(text, 0)
            .map_err(|_| anyhow!("Cursor 工作区配置结构深度超限"))?;
        // Typed deserialization retains only folders.path/uri, including JSONC
        // comments/trailing commas; unrelated workspace settings are discarded.
        let config: WorkspaceFile =
            json5::from_str(text).map_err(|_| anyhow!("Cursor 工作区配置格式无效"))?;
        total_members = total_members.saturating_add(config.folders.len());
        if total_members > MAX_ENTRIES {
            bail!("Cursor 工作区目录数量超过读取上限");
        }
        let mut members = BTreeSet::new();
        for member in config.folders {
            let candidate = if let Some(path) = member.path {
                check_path(&path)?;
                if path.is_empty() {
                    continue;
                }
                Some(
                    config_path
                        .parent()
                        .ok_or_else(|| anyhow!("Cursor 工作区路径无效"))?
                        .join(path),
                )
            } else if let Some(uri) = member.uri {
                local_file_uri(&uri)?
            } else {
                None
            };
            if let Some(path) = candidate.and_then(existing_directory) {
                members.insert(path);
            }
        }
        if let Some(archived) = native_configs.get(&config_path) {
            for path in &members {
                merge_archival(&mut native_folders, path.clone(), *archived);
            }
        }
        folders.extend(members.iter().cloned());
        workspaces.push(WorkspaceRegistration {
            archived: native_configs.get(&config_path).copied(),
            config_path,
            folders: members.into_iter().collect(),
        });
    }
    let mut projects = BTreeMap::new();
    for path in folders {
        if let Some(path) = existing_directory(path) {
            if let Some(mut project) =
                super::native::project(path.to_str().unwrap_or(""), agent)
            {
                if let Some(archived) = native_folders.get(&path) { project.record_archive(agent, *archived); }
                projects.insert(path, project);
            }
        }
        if projects.len() > MAX_ENTRIES {
            bail!("Cursor 项目数量超过读取上限");
        }
    }
    // Multiple URI spellings / symlinks can refer to the same native config.
    workspaces.sort_by(|a, b| a.config_path.cmp(&b.config_path));
    workspaces.dedup_by(|a, b| a.config_path == b.config_path);
    Ok(CursorCatalog {
        projects: projects.into_values().collect(),
        workspaces,
    })
}

fn read_budgeted(path: &Path, remaining: &mut usize) -> Result<Vec<u8>> {
    let bytes = read_bounded(path)?;
    *remaining = remaining
        .checked_sub(bytes.len())
        .ok_or_else(|| anyhow!("Cursor 项目元数据总量超过读取上限"))?;
    Ok(bytes)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let before = fs::symlink_metadata(path).map_err(|_| anyhow!("Cursor 项目元数据暂不可读"))?;
    read_bounded_file(path, &before)
}

fn read_bounded_file(path: &Path, before: &Metadata) -> Result<Vec<u8>> {
    if !before.is_file() {
        bail!("Cursor 项目元数据不是文件");
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT: open the leaf itself, not its target.
        options.custom_flags(0x0020_0000);
    }
    let mut file = options
        .open(path)
        .map_err(|_| anyhow!("Cursor 项目元数据暂不可读"))?;
    let opened = file
        .metadata()
        .map_err(|_| anyhow!("Cursor 项目元数据暂不可读"))?;
    let current = fs::symlink_metadata(path).map_err(|_| anyhow!("Cursor 项目元数据暂不可读"))?;
    if !opened.is_file()
        || !current.is_file()
        || !same_metadata_file(before, &opened)
        || !same_metadata_file(&opened, &current)
    {
        bail!("Cursor 项目元数据文件已变化");
    }
    if opened.len() > MAX_BYTES as u64 {
        bail!("Cursor 项目元数据超过读取上限");
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow!("Cursor 项目元数据暂不可读"))?;
    if bytes.len() > MAX_BYTES {
        bail!("Cursor 项目元数据超过读取上限");
    }
    let after = fs::symlink_metadata(path).map_err(|_| anyhow!("Cursor 项目元数据暂不可读"))?;
    if !after.is_file() || !same_metadata_file(&opened, &after) {
        bail!("Cursor 项目元数据文件已变化");
    }
    Ok(bytes)
}

fn same_metadata_file(before: &Metadata, after: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        before.dev() == after.dev() && before.ino() == after.ino()
    }
    #[cfg(not(unix))]
    {
        before.file_type() == after.file_type()
            && before.len() == after.len()
            && before.created().ok() == after.created().ok()
            && before.modified().ok() == after.modified().ok()
    }
}

fn read_history(db: &Path, cursor_native: bool) -> Result<(History, Vec<NativeProject>)> {
    read_history_inner(db, cursor_native).map_err(|_| anyhow!("编辑器最近项目索引暂不可读或格式无效"))
}

fn read_history_inner(db: &Path, cursor_native: bool) -> Result<(History, Vec<NativeProject>)> {
    match super::metadata_worker::read(db, super::metadata_worker::Projection::EditorHistory { cursor_native })? {
        super::metadata_worker::Reply::EditorHistory { history, projects } => Ok((history, projects)),
        _ => bail!("编辑器最近项目索引格式无效"),
    }
}

pub(super) fn read_history_local(db: &Path, cursor_native: bool) -> Result<(History, Vec<NativeProject>)> {
    let connection = super::sqlite_vfs::open_readonly(db)?;
    // Reject views, virtual tables and generated columns before projecting.
    let (kind, sql): (String, String) = connection.query_row(
        "SELECT type, sql FROM sqlite_schema WHERE name = 'ItemTable'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if kind != "table"
        || !sql
            .trim_start()
            .to_ascii_uppercase()
            .starts_with("CREATE TABLE")
    {
        bail!("invalid metadata table");
    }
    let mut schema = connection
        .prepare("SELECT name, type, hidden FROM pragma_table_xinfo('ItemTable') LIMIT 33")?;
    let columns = schema
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if columns.len() > 32
        || ![("key", "TEXT"), ("value", "BLOB")]
            .iter()
            .all(|(name, ty)| {
                columns
                    .iter()
                    .any(|(n, t, h)| n == name && t.eq_ignore_ascii_case(ty) && *h == 0)
            })
    {
        bail!("invalid metadata columns");
    }
    // Exact, private SQL allowlist: no caller-provided keys or SQL.
    let native = if cursor_native { read_value::<Vec<NativeProject>>(&connection, PROJECTS_SQL)?.unwrap_or_default() }
        else { Vec::new() };
    let history = read_value::<History>(&connection, HISTORY_SQL)?.unwrap_or_default();
    Ok((history, native))
}

fn read_value<T: serde::de::DeserializeOwned>(
    connection: &rusqlite::Connection,
    sql: &str,
) -> Result<Option<T>> {
    if ![PROJECTS_SQL, HISTORY_SQL].contains(&sql) {
        bail!("metadata query rejected");
    }
    let mut statement = connection.prepare(sql)?;
    let mut rows = statement.query([])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let bytes = match row.get_ref(0)? {
        ValueRef::Text(b) | ValueRef::Blob(b) if b.len() <= MAX_BYTES => b,
        _ => bail!("invalid metadata value"),
    };
    let value = serde_json::from_slice(bytes)?;
    if rows.next()?.is_some() {
        bail!("duplicate metadata key");
    }
    Ok(Some(value))
}

fn merge_archival(states: &mut BTreeMap<PathBuf, bool>, path: PathBuf, archived: bool) {
    states
        .entry(path)
        .and_modify(|state| *state &= archived)
        .or_insert(archived);
}

fn check_path(raw: &str) -> Result<()> {
    if raw.len() > MAX_PATH || raw.contains('\0') {
        bail!("Cursor 项目路径无效或超过读取上限");
    }
    Ok(())
}

fn insert_uri(paths: &mut BTreeSet<PathBuf>, uri: &str) -> Result<()> {
    if let Some(path) = local_file_uri(uri)? {
        paths.insert(path);
    }
    if paths.len() > MAX_ENTRIES {
        bail!("Cursor 项目数量超过读取上限");
    }
    Ok(())
}

fn existing_directory(path: PathBuf) -> Option<PathBuf> {
    if !path.is_absolute() || !path.is_dir() {
        return None;
    }
    path.canonicalize().ok()
}

/// URI decoding is byte based (UTF-8, %XX); '+' remains '+'. Remote schemes,
/// authorities and query/fragment contents never become local project paths.
fn local_file_uri(raw: &str) -> Result<Option<PathBuf>> {
    check_path(raw)?;
    let Some((scheme, tail)) = raw.split_once(':') else {
        return Ok(None);
    };
    if !scheme.eq_ignore_ascii_case("file") {
        return Ok(None);
    }
    let path = if let Some(tail) = tail.strip_prefix("//") {
        let Some((authority, path)) = tail.split_once('/') else {
            return Ok(None);
        };
        if !authority.is_empty() {
            return Ok(None);
        }
        format!("/{path}")
    } else {
        tail.to_owned()
    };
    let path = path.split(['?', '#']).next().unwrap_or("");
    let input = path.as_bytes();
    let mut bytes = Vec::new();
    let mut index = 0;
    while index < input.len() {
        if input[index] == b'%' {
            let hex = input
                .get(index + 1..index + 3)
                .and_then(|v| std::str::from_utf8(v).ok());
            let value = hex
                .and_then(|v| u8::from_str_radix(v, 16).ok())
                .ok_or_else(|| anyhow!("Cursor 文件 URI 编码无效"))?;
            bytes.push(value);
            index += 3;
        } else {
            bytes.push(input[index]);
            index += 1;
        }
    }
    let path = String::from_utf8(bytes).map_err(|_| anyhow!("Cursor 文件 URI 编码无效"))?;
    check_path(&path)?;
    Ok(Path::new(&path).is_absolute().then(|| PathBuf::from(path)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use serde_json::json;
    use std::fs::File;

    fn uri(path: &Path) -> String {
        format!(
            "file://{}",
            path.to_str()
                .unwrap()
                .replace('%', "%25")
                .replace(' ', "%20")
                .replace('#', "%23")
        )
    }
    fn database(root: &Path, history: serde_json::Value) -> PathBuf {
        let db = root.join("User/globalStorage/state.vscdb");
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE ItemTable(key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
            INSERT INTO ItemTable VALUES('composer.secret', 'must-never-be-read');",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable VALUES('history.recentlyOpenedPathsList', ?1)",
            [history.to_string()],
        )
        .unwrap();
        db
    }

    #[test]
    fn recent_sidecar_and_multiroot_keep_native_ownership_without_chat() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let data = root.join("isolated");
        let one = root.join("中文 one+#");
        let two = root.join("two");
        fs::create_dir(&one).unwrap();
        fs::create_dir(&two).unwrap();
        let config = root.join("native.code-workspace");
        fs::write(&config, format!("{{ // JSONC\n \"folders\":[{{\"path\":\"中文 one+#\"}},{{\"uri\":{}}},],\"settings\":{{\"secret\":\"ignored\"}} }}", serde_json::to_string(&uri(&two)).unwrap())).unwrap();
        let db = database(
            &data,
            json!({"entries":[
                {"folderUri":uri(&one)}, {"workspace":{"id":"native-id","configPath":uri(&config)}},
                {"fileUri":uri(&root)}, {"folderUri":uri(&root),"remoteAuthority":"ssh-remote+host"},
                {"folderUri":"vscode-remote://ssh-remote+host/tmp"}
            ]}),
        );
        let side = data.join("User/workspaceStorage/native-id");
        fs::create_dir_all(&side).unwrap();
        fs::write(
            side.join("workspace.json"),
            json!({"folder":uri(&two)}).to_string(),
        )
        .unwrap();
        // This per-workspace database is deliberately invalid and must not open.
        fs::write(side.join("state.vscdb"), "do-not-read-chat").unwrap();
        let before = fs::read(&db).unwrap();
        let catalog = cursor_catalog_at(&data).unwrap();
        assert_eq!(catalog.projects.len(), 2);
        assert_eq!(
            catalog.workspaces,
            [WorkspaceRegistration {
                config_path: config,
                archived: None,
                folders: vec![two.clone(), one.clone()]
                    .into_iter()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect()
            }]
        );
        assert!(
            catalog
                .projects
                .iter()
                .all(|p| p.agents == [AgentKind::Cursor])
        );
        assert_eq!(fs::read(&db).unwrap(), before);
        assert!(!db.with_extension("vscdb-journal").exists());
        assert_eq!(cursor_projects_at(&data).unwrap().len(), 2);
        assert!(
            cursor_projects(&root).is_err(),
            "explicit root must not leak into default lookup"
        );
    }

    #[test]
    fn workspace_json5_depth_is_checked_before_parsing_unknown_settings() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let config = root.join("native.code-workspace");
        database(
            &root,
            json!({"entries":[{"workspace":{"id":"native", "configPath":uri(&config)}}]}),
        );
        for (open, close) in [("[", "]"), ("{x:", "}")] {
            let nested = format!("{}0{}", open.repeat(100_000), close.repeat(100_000));
            let text = format!("{{folders:[{{path:'.'}}],settings:{{deep:{nested}}}}}");
            assert!(text.len() < MAX_BYTES);
            fs::write(&config, text).unwrap();
            assert_eq!(
                cursor_catalog_at(&root).unwrap_err().to_string(),
                "Cursor 工作区配置结构深度超限"
            );
        }
        // Two containing objects leave exactly thirty containers for deep.
        for (depth, accepted) in [(30, true), (31, false)] {
            let nested = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
            fs::write(
                &config,
                format!("{{folders:[{{path:'.'}}],settings:{{deep:{nested}}}}}"),
            )
            .unwrap();
            assert_eq!(cursor_catalog_at(&root).is_ok(), accepted);
        }
    }

    #[test]
    fn workspace_json5_brackets_in_quotes_escapes_and_comments_are_not_depth() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let config = root.join("native.code-workspace");
        database(
            &root,
            json!({"entries":[{"workspace":{"id":"native", "configPath":uri(&config)}}]}),
        );
        let brackets = "[{".repeat(100);
        fs::write(
            &config,
            format!(
                r#"{{ folders: [{{path: '.'}}], settings: {{
            single: 'escaped \' quote " and \\ {brackets}',
            double: "escaped \" quote ' and \\ {brackets}",
            continued: 'line\
{brackets}',
            // ' " {brackets}
            /* ' " {brackets} */
            nested: [{{ok: true}}],
        }} }}"#
            ),
        )
        .unwrap();
        let catalog = cursor_catalog_at(&root).unwrap();
        assert_eq!(catalog.workspaces[0].folders, [root.clone()]);
        assert_eq!(catalog.projects[0].path, root);
    }

    #[cfg(unix)]
    #[test]
    fn sidecar_links_are_rejected_but_explicit_workspace_config_aliases_work() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let side = root.join("User/workspaceStorage/native/workspace.json");
        fs::create_dir_all(side.parent().unwrap()).unwrap();
        let actual = root.join("actual");
        fs::create_dir(&actual).unwrap();
        let config = actual.join("native.code-workspace");
        fs::write(&config, "{folders:[{path:'.'}]}").unwrap();
        let alias = root.join("alias.code-workspace");
        symlink(&config, &alias).unwrap();
        let metadata = json!({"workspace":uri(&alias)}).to_string();
        fs::write(&side, &metadata).unwrap();
        let catalog = cursor_catalog_at(&root).unwrap();
        assert_eq!(catalog.workspaces[0].config_path, config);
        assert_eq!(catalog.workspaces[0].folders, [actual]);
        let before = fs::symlink_metadata(&side).unwrap();
        fs::rename(&side, side.with_extension("saved")).unwrap();
        let sentinel = root.join("synthetic-chat-sentinel.txt");
        // Even content that happens to parse as metadata is outside this leaf.
        fs::write(&sentinel, &metadata).unwrap();
        symlink(&sentinel, &side).unwrap();
        assert_eq!(
            cursor_catalog_at(&root).unwrap_err().to_string(),
            "Cursor 项目元数据不是文件"
        );
        assert!(read_bounded_file(&side, &before).is_err());
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), metadata);
    }

    #[cfg(unix)]
    #[test]
    fn sidecar_fifo_and_regular_to_fifo_replacement_do_not_wait_for_a_writer() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt, sync::mpsc, time::Duration};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let side = root.join("User/workspaceStorage/native/workspace.json");
        fs::create_dir_all(side.parent().unwrap()).unwrap();
        fs::write(&side, "{}").unwrap();
        let before = fs::symlink_metadata(&side).unwrap();
        fs::rename(&side, side.with_extension("saved")).unwrap();
        let name = CString::new(side.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a live NUL-terminated path; this creates only a fixture.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            tx.send((
                cursor_catalog_at(&root).is_err(),
                read_bounded_file(&side, &before).is_err(),
            ))
            .unwrap();
        });
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2))
                .expect("metadata FIFO blocked"),
            (true, true)
        );
        worker.join().unwrap();
    }

    #[test]
    fn metadata_identity_rejects_replaced_regular_leaf_before_reading() {
        let temp = tempfile::tempdir().unwrap();
        let side = temp.path().join("workspace.json");
        fs::write(&side, "{}").unwrap();
        let before = fs::symlink_metadata(&side).unwrap();
        fs::rename(&side, temp.path().join("saved.json")).unwrap();
        fs::write(&side, "[]").unwrap();
        assert!(read_bounded_file(&side, &before).is_err());
        assert_eq!(read_bounded(&side).unwrap(), b"[]");
    }

    #[test]
    fn agents_registry_is_primary_and_component_paths_are_not_double_decoded() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let cwd = root.join("literal%20project");
        fs::create_dir(&cwd).unwrap();
        let db = database(&root, json!({"entries":[{"folderUri": uri(&cwd)}]}));
        let conn = Connection::open(&db).unwrap();
        conn.execute("INSERT INTO ItemTable VALUES('glass.localAgentProjects.v1', ?1)",
            [json!([{"workspace":{"id":"native","uri":{"$mid":1,"scheme":"file","path":cwd,"authority":""}},"isArchived":true,"name":"ignored"}]).to_string()]).unwrap();
        let projects = cursor_projects_at(&root).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, cwd);
        assert!(
            projects[0].archived,
            "stale recent entries must not unarchive native projects"
        );
    }

    #[test]
    fn uri_decodes_unicode_plus_and_reserved_bytes_and_rejects_remote() {
        assert_eq!(
            local_file_uri("file:///tmp/%E4%B8%AD%20a+%23%25").unwrap(),
            Some(PathBuf::from("/tmp/中 a+#%"))
        );
        for raw in [
            "file://server/tmp",
            "vscode-remote://host/tmp",
            "/tmp/plain",
            "file:relative",
        ] {
            assert_eq!(local_file_uri(raw).unwrap(), None);
        }
        for raw in [
            "file:///tmp/%00",
            "file:///tmp/%ff",
            "file:///tmp/%QZ",
            "file:///tmp/%",
        ] {
            assert!(local_file_uri(raw).is_err());
        }
    }

    #[test]
    fn sidecar_only_and_symlink_alias_merge_without_parent_guess() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let cwd = root.join("real");
        fs::create_dir(&cwd).unwrap();
        let data = root.join("data");
        for (id, path) in [("one", cwd.clone()), ("two", cwd.join("."))] {
            let side = data.join("User/workspaceStorage").join(id);
            fs::create_dir_all(&side).unwrap();
            fs::write(
                side.join("workspace.json"),
                json!({"folder":uri(&path)}).to_string(),
            )
            .unwrap();
        }
        #[cfg(unix)]
        {
            let alias = root.join("alias");
            std::os::unix::fs::symlink(&cwd, &alias).unwrap();
            database(&data, json!({"entries":[{"folderUri":uri(&alias)}]}));
        }
        let projects = cursor_projects_at(&data).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, cwd);
    }

    #[test]
    fn bounds_and_untrusted_schema_fail_closed_with_sanitized_errors() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let db = database(root, json!({"entries":[]}));
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("DROP TABLE ItemTable; CREATE VIEW ItemTable AS SELECT 'secret' AS key, 'secret' AS value").unwrap();
        let error = cursor_projects_at(root).unwrap_err().to_string();
        assert!(!error.contains("secret") && !error.contains(root.to_str().unwrap()));
        conn.execute_batch("DROP VIEW ItemTable; CREATE TABLE ItemTable(key TEXT, value BLOB)")
            .unwrap();
        conn.execute(
            "INSERT INTO ItemTable VALUES('history.recentlyOpenedPathsList', ?1)",
            ["x".repeat(MAX_BYTES + 1)],
        )
        .unwrap();
        assert!(cursor_projects_at(root).is_err());
        let file = root.join("large.json");
        File::create(&file)
            .unwrap()
            .set_len((MAX_BYTES + 1) as u64)
            .unwrap();
        assert!(read_bounded(&file).is_err());
        assert!(local_file_uri(&format!("file:///{}", "a".repeat(MAX_PATH))).is_err());
    }

    #[test]
    fn present_history_requires_entries_array_for_all_editors() {
        for invalid in [
            json!({}),
            json!({"unknown": "fixture-unrelated-content"}),
            json!({"entries": null}),
            json!({"entries": {}}),
            json!({"entries": "fixture-unrelated-content"}),
            json!({"entries": 42}),
            json!({"entries": false}),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            database(&root, invalid);
            for agent in [AgentKind::Cursor, AgentKind::Trae, AgentKind::Antigravity] {
                let error = editor_projects_at(&root, agent).unwrap_err().to_string();
                assert_eq!(error, "编辑器最近项目索引暂不可读或格式无效");
            }
        }
    }

    #[test]
    fn no_history_key_is_empty_and_duplicate_key_is_error() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let db = database(&root, json!({"entries":[]}));
        let conn = Connection::open(&db).unwrap();
        for agent in [AgentKind::Cursor, AgentKind::Trae, AgentKind::Antigravity] {
            assert!(editor_projects_at(&root, agent).unwrap().is_empty());
        }
        conn.execute_batch("DELETE FROM ItemTable WHERE key='history.recentlyOpenedPathsList'")
            .unwrap();
        for agent in [AgentKind::Cursor, AgentKind::Trae, AgentKind::Antigravity] {
            assert!(editor_projects_at(&root, agent).unwrap().is_empty());
        }
        conn.execute_batch(
            "DROP TABLE ItemTable; CREATE TABLE ItemTable(key TEXT, value BLOB);
            INSERT INTO ItemTable VALUES('history.recentlyOpenedPathsList','{\"entries\":[]}');
            INSERT INTO ItemTable VALUES('history.recentlyOpenedPathsList','{\"entries\":[]}');",
        )
        .unwrap();
        assert!(editor_projects_at(&root, AgentKind::Cursor).is_err());
    }
}
