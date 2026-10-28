//! Read-only native SQLite catalogs. Never initialize databases or read messages.
use std::{collections::BTreeSet, path::{Path, PathBuf}};

use anyhow::{Result, anyhow, bail};
use rusqlite::{Connection, types::ValueRef};

use super::{AgentKind, ProjectEntry, project_name, metadata_worker::{self, PathQuery as QueryId, Projection, Reply}};

const MAX_PATHS: usize = 10_000;
const MAX_PATH_BYTES: usize = 16 * 1024;

// LIMIT includes a sentinel row: truncation must never look like a full catalog.
// No ORDER BY: discovery does not need recency, and sorting could scan every row.
pub(super) const WORKBUDDY_PATHS_SQL: &str = "SELECT path FROM workspaces LIMIT 10001";

// WorkBuddy 5.8.2: visibleRows -> applyListParams -> toSpace ->
// listSavedWorkspacePaths. Never project session settings, titles or messages.
// Duplicate visibility keys fail closed instead of trusting SQLite's first-key
// JSON lookup where the host's JSON.parse would use the last key.
const WORKBUDDY_SESSIONS_SQL: &str = "SELECT cwd, is_playground, project_id,
    CASE WHEN typeof(session_settings) NOT IN ('text', 'null') THEN -1
    WHEN session_settings IS NULL THEN 0
    WHEN length(CAST(session_settings AS BLOB)) > 16384 THEN -1
    WHEN length(CAST(session_settings AS BLOB)) = 0 THEN 0
    WHEN json_valid(session_settings) THEN
      CASE WHEN json_type(session_settings) = 'object' THEN COALESCE((
        SELECT CASE WHEN COUNT(*) > 1 THEN -1 ELSE MAX(
          CASE WHEN tags.type = 'object' THEN COALESCE((
            SELECT CASE WHEN COUNT(*) > 1 THEN -1 ELSE MAX(
              CASE WHEN tag.type = 'text' AND tag.value COLLATE BINARY = '1' THEN 1 ELSE 0 END
            ) END FROM json_each(tags.value) AS tag WHERE tag.key COLLATE BINARY = 'wbQuickAsk'
          ), 0) ELSE 0 END
        ) END FROM json_each(session_settings) AS tags WHERE tags.key COLLATE BINARY = 'conversationTags'
      ), 0) ELSE 0 END
    ELSE -1 END
    FROM sessions
    WHERE transport COLLATE BINARY = 'local' AND deleted_at IS NULL
      AND (user_id COLLATE BINARY = ?1 OR user_id COLLATE BINARY = '')
    LIMIT 10001";

const WORKBUDDY_SESSIONS: PathQuery = PathQuery {
    id: None,
    sql: WORKBUDDY_SESSIONS_SQL,
    table: "sessions",
    columns: &[("cwd", "TEXT"), ("user_id", "TEXT"), ("transport", "TEXT"),
        ("deleted_at", "INTEGER"), ("is_playground", "INTEGER"),
        ("project_id", "TEXT"), ("session_settings", "TEXT")],
};

/// Both values must come from a verified current WorkBuddy runtime projection,
/// bound to this database. A stored session owner or guessed Claw path is not
/// authority. No safe offline provider is established yet; production is unknown.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkBuddyScope {
    pub current_user_id: String,
    pub assistant_cwd: PathBuf,
}

impl std::fmt::Debug for WorkBuddyScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkBuddyScope").finish_non_exhaustive()
    }
}

impl WorkBuddyScope {
    pub(super) fn validate(&self) -> Result<()> {
        if self.current_user_id.trim().is_empty() || self.current_user_id.len() > MAX_PATH_BYTES
            || self.current_user_id.contains('\0') || !self.assistant_cwd.is_absolute()
            || self.assistant_cwd.as_os_str().len() > MAX_PATH_BYTES
            || self.assistant_cwd.to_str().is_none_or(|path| path.contains('\0')) {
            bail!("WorkBuddy 当前账户或空间身份未核验，原生项目状态未知");
        }
        Ok(())
    }
}

struct PathQuery {
    id: Option<QueryId>,
    sql: &'static str,
    table: &'static str,
    columns: &'static [(&'static str, &'static str)],
}

const WORKBUDDY_CATALOG: PathQuery = PathQuery {
    id: Some(QueryId::WorkBuddy),
    sql: WORKBUDDY_PATHS_SQL,
    table: "workspaces",
    columns: &[("path", "TEXT"), ("last_opened_at", "INTEGER")],
};

// Extend only with source-verified native table/column mappings. The
// caller's &[&str] is not SQL authority: exact membership here is required.
const PATH_QUERIES: &[PathQuery] = &[
    WORKBUDDY_CATALOG,
    PathQuery {
        id: Some(QueryId::OpenCode),
        sql: super::cli::OPENCODE_PROJECTS_SQL,
        table: "project",
        columns: &[("worktree", "TEXT")],
    },
    PathQuery {
        id: Some(QueryId::HermesProjects),
        sql: super::runtime_agents::HERMES_PROJECTS_SQL,
        table: "projects",
        columns: &[("id", "TEXT"), ("primary_path", "TEXT"), ("archived", "INTEGER"), ("name", "TEXT")],
    },
    PathQuery {
        id: Some(QueryId::HermesFolders),
        sql: super::runtime_agents::HERMES_FOLDERS_SQL,
        table: "project_folders",
        columns: &[("project_id", "TEXT"), ("path", "TEXT"), ("is_primary", "INTEGER"), ("added_at", "INTEGER")],
    },
    PathQuery {
        id: Some(QueryId::HermesSessions),
        sql: super::runtime_agents::HERMES_SESSIONS_SQL,
        table: "sessions",
        columns: &[("git_repo_root", "TEXT"), ("cwd", "TEXT")],
    },
    PathQuery {
        id: Some(QueryId::DimAgentSessions),
        sql: super::dimagent::SESSIONS_SQL,
        table: "sessions",
        columns: &[("sessionId", "TEXT"), ("cwd", "TEXT"), ("version", "INTEGER")],
    },
];

// Native v1 table and v2 split/migration table. These are deliberately outside
// sqlite_paths' allowlist: only an observed project(global, /) unlocks them.
const OPENCODE_GLOBAL_SESSIONS: &[PathQuery] = &[
    PathQuery {
        id: None,
        sql: "SELECT directory FROM session WHERE project_id COLLATE BINARY = 'global' LIMIT 10001",
        table: "session",
        columns: &[("project_id", "TEXT"), ("directory", "TEXT")],
    },
    PathQuery {
        id: None,
        sql: "SELECT directory FROM session_v2 WHERE project_id COLLATE BINARY = 'global' LIMIT 10001",
        table: "session_v2",
        columns: &[("project_id", "TEXT"), ("directory", "TEXT")],
    },
];

/// Completeness of the native catalog + required session-derived workspaces.
/// A readable catalog alone must not authorize removal of previous ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkBuddyCompleteness {
    Complete,
    CatalogOnlySessionScopeUnknown,
    CatalogOnlySessionsUnreadable,
}

#[derive(Debug)]
pub(super) struct WorkBuddyDiscovery {
    pub projects: Vec<ProjectEntry>,
    pub completeness: WorkBuddyCompleteness,
}

pub(super) fn workbuddy_projects(home: &Path) -> Result<WorkBuddyDiscovery> {
    let root = crate::agents::workbuddy_desktop_database_root(home)?;
    // Desktop DB routing is independent of personal skills; scope stays unknown.
    workbuddy_projects_at(&root, None)
}

pub(super) fn workbuddy_projects_at(root: &Path, scope: Option<WorkBuddyScope>) -> Result<WorkBuddyDiscovery> {
    if !root.is_absolute() {
        bail!("WorkBuddy 项目索引配置根无效");
    }
    let db = root.join("workbuddy.db");
    let (paths, completeness) = match scope {
        Some(scope) => {
            match metadata_worker::read(&db, Projection::WorkBuddyProjects { scope }) {
                Ok(Reply::Paths { paths }) => (paths, WorkBuddyCompleteness::Complete),
                // A failed combined projection never certifies session absence.
                // The independent catalog is still useful if its own fixed
                // reader succeeds; any catalog failure continues to return Err.
                _ => (sqlite_paths(&db, &[WORKBUDDY_PATHS_SQL])?,
                    WorkBuddyCompleteness::CatalogOnlySessionsUnreadable),
            }
        }
        // Reuse the fixed catalog projection. Without a bound scope, do not
        // inspect sessions, infer a UID, or claim the required source is complete.
        None => (sqlite_paths(&db, &[WORKBUDDY_PATHS_SQL])?,
            WorkBuddyCompleteness::CatalogOnlySessionScopeUnknown),
    };
    let mut seen = BTreeSet::new();
    let mut projects = Vec::new();
    for raw in paths {
        let path = Path::new(&raw);
        if !path.is_absolute() || !path.is_dir() { continue; }
        let Ok(path) = path.canonicalize() else { continue; };
        if !seen.insert(path.clone()) { continue; }
        projects.push(ProjectEntry {
            name: project_name(&path), path, agents: vec![AgentKind::WorkBuddy],
            proma_workspaces: Vec::new(), native_workspaces: Vec::new(), archive_states: Vec::new(), archived: false, hidden: false, retained: false,
        });
    }
    Ok(WorkBuddyDiscovery { projects, completeness })
}

/// A single locked snapshot and aggregate row budget for catalog + sessions.
/// Scope is transported on worker stdin; only eligible paths leave the worker.
pub(super) fn workbuddy_paths_local(db: &Path, scope: &WorkBuddyScope) -> Result<Vec<String>> {
    scope.validate()?;
    let connection = super::sqlite_vfs::open_readonly(db)
        .map_err(|_| anyhow!("WorkBuddy 原生项目索引暂不可读，状态未知"))?;
    let mut paths = Vec::new();
    let mut rows_seen = 0usize;
    for query in [&WORKBUDDY_CATALOG, &WORKBUDDY_SESSIONS] {
        validate_schema(&connection, query)
            .map_err(|_| anyhow!("WorkBuddy 原生项目索引结构无效，状态未知"))?;
        let mut statement = connection.prepare(query.sql)
            .map_err(|_| anyhow!("WorkBuddy 原生项目索引格式无效，状态未知"))?;
        let mut rows = if query.table == "sessions" {
            statement.query([scope.current_user_id.trim()])
        } else { statement.query([]) }
            .map_err(|_| anyhow!("WorkBuddy 原生项目索引暂不可读，状态未知"))?;
        while let Some(row) = rows.next().map_err(|_| anyhow!("WorkBuddy 原生项目索引格式无效，状态未知"))? {
            if rows_seen >= MAX_PATHS { bail!("Agent 项目索引超过读取上限"); }
            rows_seen += 1;
            let cwd = workbuddy_text(row.get_ref(0)
                .map_err(|_| anyhow!("WorkBuddy 原生项目索引格式无效"))?)?;
            if query.table == "sessions" {
                let playground = match row.get_ref(1) {
                    Ok(ValueRef::Null) => None,
                    Ok(ValueRef::Integer(value)) => Some(value != 0),
                    _ => bail!("WorkBuddy 原生项目空间格式无效，状态未知"),
                };
                let project = match row.get_ref(2) {
                    Ok(ValueRef::Null) => None,
                    Ok(value) => Some(workbuddy_text(value)?),
                    _ => bail!("WorkBuddy 原生项目空间格式无效，状态未知"),
                };
                let quick_ask = match row.get_ref(3) {
                    Ok(ValueRef::Integer(0)) => false,
                    Ok(ValueRef::Integer(1)) => true,
                    _ => bail!("WorkBuddy 原生项目可见性未核验，状态未知"),
                };
                if !workbuddy_session_workspace(cwd, playground, project, quick_ask, scope) { continue; }
            }
            paths.push(cwd.to_owned());
        }
    }
    Ok(paths)
}

fn workbuddy_text<'a>(value: ValueRef<'a>) -> Result<&'a str> {
    let ValueRef::Text(bytes) = value else { bail!("WorkBuddy 原生项目元数据格式无效"); };
    if bytes.len() > MAX_PATH_BYTES { bail!("WorkBuddy 原生项目元数据超过读取上限"); }
    std::str::from_utf8(bytes).map_err(|_| anyhow!("WorkBuddy 原生项目元数据格式无效"))
}

// The host compares F.resolve(cwd).toLowerCase(), not realpath or a '/Claw'
// suffix. Directory canonicalization/dedup remains in workbuddy_projects_at.
fn workbuddy_path_key(path: &Path) -> String {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        if component == std::path::Component::ParentDir { normalized.pop(); }
        else { normalized.push(component.as_os_str()); }
    }
    normalized.to_string_lossy().to_lowercase()
}

fn workbuddy_session_workspace(cwd: &str, playground: Option<bool>, project: Option<&str>,
    quick_ask: bool, scope: &WorkBuddyScope) -> bool {
    if quick_ask || !Path::new(cwd).is_absolute() || playground == Some(true)
        || project.is_some_and(|id| !id.is_empty())
        || workbuddy_path_key(Path::new(cwd)) == workbuddy_path_key(&scope.assistant_cwd) {
        return false;
    }
    // toSpace falls back to /^\d{4}-\d{2}-\d{2}-\d{2}-\d{2}-\d{2}$/
    // only for NULL is_playground; an explicit false must remain visible.
    let normalized = cwd.replace('\\', "/");
    let name = normalized.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let generated = name.len() == 19 && name.bytes().enumerate().all(|(i, byte)| {
        if [4, 7, 10, 13, 16].contains(&i) { byte == b'-' } else { byte.is_ascii_digit() }
    });
    playground == Some(false) || !generated
}

/// Read bounded path-only projections from this module's SQL allowlist.
/// Errors deliberately omit database paths, SQLite messages, SQL and row data.
/// An empty authoritative table is success; missing/bad/oversized data is not.
pub(super) fn sqlite_paths(db: &Path, queries: &[&str]) -> Result<Vec<String>> {
    if queries.is_empty() || queries.iter().any(|sql| !PATH_QUERIES.iter().any(|q| q.sql == *sql)) {
        bail!("Agent 项目索引查询不在白名单中");
    }
    let queries = queries.iter().map(|sql| {
        PATH_QUERIES.iter().find(|query| query.sql == *sql).and_then(|query| query.id)
            .ok_or_else(|| anyhow!("Agent 项目索引查询不在白名单中"))
    }).collect::<Result<Vec<_>>>()?;
    match metadata_worker::read(db, Projection::Paths { queries })? {
        Reply::Paths { paths } => Ok(paths),
        _ => bail!("Agent 项目索引格式无效"),
    }
}

pub(super) fn sqlite_paths_local(db: &Path, queries: &[QueryId]) -> Result<Vec<String>> {
    let queries = queries.iter().map(|id| {
        PATH_QUERIES.iter().find(|query| query.id == Some(*id))
            .ok_or_else(|| anyhow!("Agent 项目索引查询不在白名单中"))
    }).collect::<Result<Vec<_>>>()?;
    Ok(sqlite_metadata(db, &queries, false)?.into_iter().map(|row| row.path).collect())
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct ProjectPathRow {
    pub path: String,
    pub archived: Option<bool>,
    pub is_global_root: bool,
}

/// A fixed metadata projection, not an additional caller-controlled SQL API.
pub(super) fn opencode_project_rows(db: &Path) -> Result<Vec<ProjectPathRow>> {
    match metadata_worker::read(db, Projection::OpenCodeProjects)? {
        Reply::OpenCodeProjects { rows } => Ok(rows),
        _ => bail!("Agent 项目索引格式无效"),
    }
}

pub(super) fn opencode_project_rows_local(db: &Path) -> Result<Vec<ProjectPathRow>> {
    let query = PATH_QUERIES.iter()
        .find(|query| query.sql == super::cli::OPENCODE_PROJECTS_SQL)
        .ok_or_else(|| anyhow!("Agent 项目索引查询不在白名单中"))?;
    sqlite_metadata(db, &[query], true)
}

fn sqlite_metadata(db: &Path, queries: &[&PathQuery], opencode_archive: bool) -> Result<Vec<ProjectPathRow>> {
    if opencode_archive && queries.iter().any(|query| query.sql != super::cli::OPENCODE_PROJECTS_SQL) {
        bail!("Agent 项目索引查询不在白名单中");
    }
    if !db.is_file() {
        bail!("Agent 项目索引暂不可读");
    }
    // The restricted VFS opens one locked, read-only transaction without
    // mutating the source database, WAL, SHM or directory on open/read/close.
    let connection = super::sqlite_vfs::open_readonly(db)
        .map_err(|_| anyhow!("Agent 项目索引暂不可读"))?;
    let mut paths = Vec::new();
    let mut rows_seen = 0usize;
    let mut pending = queries.to_vec();
    let mut next = 0;
    while let Some(query) = pending.get(next).copied() {
        next += 1;
        let is_project = opencode_archive && query.table == "project";
        let mut has_global_root = false;
        let columns = validate_schema(&connection, query)
            .map_err(|_| anyhow!("Agent 项目索引结构无效"))?;
        let has_archive = is_project && optional_column(&columns, "time_archived", "INTEGER")?;
        let has_id = is_project && optional_column(&columns, "id", "TEXT")?;
        // All variants are fixed projections of the same verified native table.
        // NULL placeholders preserve the path-only contract of older schemas.
        let sql = if is_project {
            match (has_archive, has_id) {
                (true, true) => "SELECT worktree, time_archived, id FROM project LIMIT 10001",
                (true, false) => "SELECT worktree, time_archived, NULL FROM project LIMIT 10001",
                (false, true) => "SELECT worktree, NULL, id FROM project LIMIT 10001",
                (false, false) => "SELECT worktree, NULL, NULL FROM project LIMIT 10001",
            }
        } else { query.sql };
        let mut statement = connection.prepare(sql)
            .map_err(|_| anyhow!("Agent 项目索引格式无效"))?;
        let mut rows = statement.query([])
            .map_err(|_| anyhow!("Agent 项目索引暂不可读"))?;
        while let Some(row) = rows.next().map_err(|_| anyhow!("Agent 项目索引格式无效"))? {
            if rows_seen >= MAX_PATHS {
                bail!("Agent 项目索引超过读取上限");
            }
            rows_seen += 1;
            let ValueRef::Text(bytes) = row.get_ref(0)
                .map_err(|_| anyhow!("Agent 项目索引路径格式无效"))? else {
                bail!("Agent 项目索引路径格式无效");
            };
            // Check the borrowed value before allocating a Rust String.
            if bytes.len() > MAX_PATH_BYTES {
                bail!("Agent 项目索引路径超过读取上限");
            }
            let archived = if has_archive {
                Some(match row.get_ref(1).map_err(|_| anyhow!("Agent 项目索引归档格式无效"))? {
                    ValueRef::Null => false,
                    // Project.Time.archived is NonNegativeInt; zero is archived too.
                    ValueRef::Integer(value) if value >= 0 => true,
                    _ => bail!("Agent 项目索引归档格式无效"),
                })
            } else { None };
            let is_global_root = if has_id {
                let ValueRef::Text(id) = row.get_ref(2)
                    .map_err(|_| anyhow!("Agent 项目索引标识格式无效"))? else {
                    bail!("Agent 项目索引标识格式无效");
                };
                if id.len() > MAX_PATH_BYTES {
                    bail!("Agent 项目索引标识超过读取上限");
                }
                std::str::from_utf8(id).map_err(|_| anyhow!("Agent 项目索引标识格式无效"))?;
                id == b"global" && bytes == b"/"
            } else { false };
            has_global_root |= is_global_root;
            paths.push(ProjectPathRow {
                path: std::str::from_utf8(bytes)
                    .map_err(|_| anyhow!("Agent 项目索引路径格式无效"))?.to_owned(),
                archived,
                is_global_root,
            });
        }
        if has_global_root {
            // The same locked transaction gates and reads both native indexes.
            // Coexisting v1/v2 histories are combined; no global row means no
            // session schema lookup, and an empty project table stays empty.
            let mut found = false;
            for session in OPENCODE_GLOBAL_SESSIONS {
                let exists: bool = connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = ?1)",
                    [session.table], |row| row.get(0),
                ).map_err(|_| anyhow!("Agent 项目索引结构无效"))?;
                if exists {
                    pending.push(session);
                    found = true;
                }
            }
            if !found { bail!("Agent 项目索引结构无效"); }
        }
    }
    Ok(paths)
    // Dropping the read-only connection closes the read transaction on all paths.
}

fn optional_column(columns: &[(String, String, i64)], name: &str, kind: &str) -> Result<bool> {
    let Some((_, ty, hidden)) = columns.iter().find(|(actual, _, _)| actual.eq_ignore_ascii_case(name)) else {
        return Ok(false);
    };
    if !ty.eq_ignore_ascii_case(kind) || *hidden != 0 {
        bail!("Agent 项目索引结构无效");
    }
    Ok(true)
}

pub(super) fn validate_native_schema(
    connection: &Connection,
    table: &'static str,
    columns: &'static [(&'static str, &'static str)],
) -> rusqlite::Result<()> {
    validate_schema(connection, &PathQuery { id: None, sql: "", table, columns }).map(|_| ())
}

fn validate_schema(connection: &Connection, query: &PathQuery) -> rusqlite::Result<Vec<(String, String, i64)>> {
    let (kind, definition): (String, String) = connection.query_row(
        "SELECT type, sql FROM sqlite_schema WHERE name = ?1",
        [query.table], |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    // Reject views/virtual tables and generated path expressions. They are not
    // the verified native table and could execute unrelated queries on a read.
    if kind != "table" || !definition.trim_start().to_ascii_uppercase().starts_with("CREATE TABLE") {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let mut statement = connection.prepare(
        "SELECT name, type, hidden FROM pragma_table_xinfo(?1)",
    )?;
    let columns = statement.query_map([query.table], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    if query.columns.iter().any(|(name, kind)| !columns.iter().any(|(actual, ty, hidden)| {
        actual.as_str() == *name && ty.eq_ignore_ascii_case(kind) && *hidden == 0
    })) {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(columns)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(path: &str) -> String {
        if cfg!(windows) { format!("C:{path}") } else { path.to_owned() }
    }

    // Keep distinct database keys even when Windows PathBuf::join would
    // normalize a verbatim path's dot component before insertion.
    fn dotted(path: &Path) -> PathBuf {
        let path = path.to_str().unwrap();
        #[cfg(windows)]
        let path = path.strip_prefix(r"\\?\").unwrap_or(path);
        PathBuf::from(format!("{path}{sep}.", sep = std::path::MAIN_SEPARATOR))
    }

    fn catalog(root: &Path) -> Connection {
        let connection = Connection::open(root.join("workbuddy.db")).unwrap();
        connection.execute_batch(
            "CREATE TABLE workspaces(path TEXT PRIMARY KEY, last_opened_at INTEGER NOT NULL);
             CREATE TABLE sessions(cwd TEXT, user_id TEXT, transport TEXT, deleted_at INTEGER,
                 is_playground INTEGER, project_id TEXT, session_settings TEXT, title TEXT, status TEXT);
             INSERT INTO sessions(cwd,user_id,transport,deleted_at,is_playground,project_id,session_settings,title)
                 VALUES('/not-a-project-source', 'other-user', 'local', NULL,
                 0, NULL, NULL, 'do-not-read');",
        ).unwrap();
        connection
    }

    fn workbuddy_scope(root: &Path) -> WorkBuddyScope {
        WorkBuddyScope { current_user_id: "reader-current".into(), assistant_cwd: root.join("assistant") }
    }

    #[test]
    fn workbuddy_desktop_database_routing_keeps_personal_instance_root_separate() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let desktop = home.join("desktop-config");
        let desktop_default = home.join(".workbuddy");
        let personal = crate::agents::workbuddy_config_root_from(&home, None, Some("2")).unwrap();
        let desktop_project = home.join("desktop-project");
        let default_project = home.join("default-project");
        let personal_project = home.join("personal-project");
        for path in [&desktop, &desktop_default, &personal, &desktop_project, &default_project, &personal_project] {
            std::fs::create_dir(path).unwrap();
        }
        for (root, project) in [(&desktop, &desktop_project), (&desktop_default, &default_project), (&personal, &personal_project)] {
            let connection = catalog(root);
            connection.execute("INSERT INTO workspaces VALUES(?1,1)",
                [project.to_str().unwrap()]).unwrap();
        }
        for (selected, expected_root, expected_project) in [
            (crate::agents::workbuddy_desktop_database_root_from(&home, Some(" \t"), desktop.to_str()).unwrap(),
                &desktop, &desktop_project),
            (crate::agents::workbuddy_desktop_database_root_from(&home, None, None).unwrap(),
                &desktop_default, &default_project),
        ] {
            let discovery = workbuddy_projects_at(&selected, None).unwrap();
            assert_eq!(&selected, expected_root);
            assert_eq!(discovery.completeness, WorkBuddyCompleteness::CatalogOnlySessionScopeUnknown);
            assert_eq!(discovery.projects.len(), 1);
            assert_eq!(&discovery.projects[0].path, expected_project);
            assert_ne!(discovery.projects[0].path, personal_project);
        }
    }

    #[test]
    fn readonly_discovery_canonicalizes_and_does_not_modify_database() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        let file = root.join("ordinary-file");
        std::fs::write(&file, "not a directory").unwrap();
        let connection = catalog(&root);
        let mut paths = vec![project.clone(), dotted(&project), root.join("missing"),
            PathBuf::from("relative"), file];
        #[cfg(unix)]
        {
            let alias = root.join("alias");
            std::os::unix::fs::symlink(&project, &alias).unwrap();
            paths.push(alias);
        }
        for path in paths {
            connection.execute("INSERT INTO workspaces VALUES(?1, 1)",
                [path.to_str().unwrap()]).unwrap();
        }
        drop(connection);
        let db = root.join("workbuddy.db");
        let before = std::fs::read(&db).unwrap();
        let discovery = workbuddy_projects_at(&root, Some(workbuddy_scope(&root))).unwrap();
        assert_eq!(discovery.completeness, WorkBuddyCompleteness::Complete);
        let projects = discovery.projects;
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, project);
        assert_eq!(projects[0].agents, [AgentKind::WorkBuddy]);
        assert!(!projects[0].archived);
        assert_eq!(std::fs::read(db).unwrap(), before); // schema and data bytes
    }

    #[test]
    fn workbuddy_empty_sources_do_not_fall_back_to_another_account() {
        let temp = tempfile::tempdir().unwrap();
        drop(catalog(temp.path()));
        let discovery = workbuddy_projects_at(temp.path(), Some(workbuddy_scope(temp.path()))).unwrap();
        assert_eq!(discovery.completeness, WorkBuddyCompleteness::Complete);
        assert!(discovery.projects.is_empty());
    }

    #[test]
    fn workbuddy_partial_catalog_does_not_promote_sessions_or_require_their_schema() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let registered = root.join("registered"); let session_only = root.join("session-only");
        std::fs::create_dir(&registered).unwrap(); std::fs::create_dir(&session_only).unwrap();
        let connection = catalog(&root);
        for path in [&registered, &dotted(&registered)] {
            connection.execute("INSERT INTO workspaces VALUES(?1, 1)", [path.to_str().unwrap()]).unwrap();
        }
        for user in ["reader-current", "", "other-user"] {
            connection.execute("INSERT INTO sessions(cwd,user_id,transport,is_playground) VALUES(?1,?2,'local',0)",
                rusqlite::params![session_only.to_str().unwrap(), user]).unwrap();
        }
        drop(connection);
        let db = root.join("workbuddy.db"); let before = std::fs::read(&db).unwrap();
        let discovery = workbuddy_projects_at(&root, None).unwrap();
        assert_eq!(discovery.completeness, WorkBuddyCompleteness::CatalogOnlySessionScopeUnknown);
        assert_eq!(discovery.projects.len(), 1);
        assert_eq!(discovery.projects[0].path, registered);
        assert_eq!(discovery.projects[0].agents, [AgentKind::WorkBuddy]);
        assert!(!discovery.projects[0].archived);
        assert_eq!(std::fs::read(&db).unwrap(), before);
        let connection = Connection::open(&db).unwrap();
        connection.execute_batch("DROP TABLE sessions; CREATE VIEW sessions AS SELECT * FROM absent_private_table;").unwrap();
        drop(connection);
        let before = std::fs::read(&db).unwrap();
        let discovery = workbuddy_projects_at(&root, None).unwrap();
        assert_eq!(discovery.completeness, WorkBuddyCompleteness::CatalogOnlySessionScopeUnknown);
        assert_eq!(discovery.projects[0].path, registered);
        assert_eq!(std::fs::read(&db).unwrap(), before);
        for user in ["", " \t", "bad\0user"] {
            let scope = WorkBuddyScope { current_user_id: user.into(), ..workbuddy_scope(&root) };
            assert!(workbuddy_paths_local(&root.join("absent.db"), &scope).is_err());
        }
        assert!(!root.join("absent.db").exists());
    }

    #[test]
    fn workbuddy_partial_empty_catalog_still_has_required_session_gap() {
        let temp = tempfile::tempdir().unwrap();
        let connection = catalog(temp.path());
        connection.execute_batch("DROP TABLE sessions;").unwrap();
        drop(connection);
        let discovery = workbuddy_projects_at(temp.path(), None).unwrap();
        assert!(discovery.projects.is_empty());
        assert_eq!(discovery.completeness, WorkBuddyCompleteness::CatalogOnlySessionScopeUnknown);
    }

    #[test]
    fn workbuddy_known_scope_with_failed_sessions_keeps_independent_catalog_partial() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let registered = root.join("registered");
        std::fs::create_dir(&registered).unwrap();
        let connection = catalog(&root);
        connection.execute("INSERT INTO workspaces VALUES(?1,1)", [registered.to_str().unwrap()]).unwrap();
        connection.execute_batch("DROP TABLE sessions; CREATE VIEW sessions AS SELECT * FROM absent_private_table;").unwrap();
        drop(connection);
        let db = root.join("workbuddy.db"); let before = std::fs::read(&db).unwrap();
        let discovery = workbuddy_projects_at(&root, Some(workbuddy_scope(&root))).unwrap();
        assert_eq!(discovery.completeness, WorkBuddyCompleteness::CatalogOnlySessionsUnreadable);
        assert_eq!(discovery.projects.len(), 1);
        assert_eq!(discovery.projects[0].path, registered);
        assert_eq!(std::fs::read(&db).unwrap(), before);
        let connection = Connection::open(&db).unwrap();
        connection.execute_batch("DROP TABLE workspaces;").unwrap(); drop(connection);
        assert!(workbuddy_projects_at(&root, Some(workbuddy_scope(&root))).is_err());
    }

    #[test]
    fn workbuddy_partial_catalog_rejects_missing_corrupt_or_invalid_catalogs() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("workbuddy.db");
        assert!(workbuddy_projects_at(temp.path(), None).is_err());
        assert!(!db.exists());
        std::fs::write(&db, "fixture-private-corrupt-body").unwrap();
        let before = std::fs::read(&db).unwrap();
        let error = workbuddy_projects_at(temp.path(), None).unwrap_err().to_string();
        assert!(!error.contains("fixture-private-corrupt-body") && !error.contains(temp.path().to_str().unwrap()));
        assert_eq!(std::fs::read(&db).unwrap(), before);
        std::fs::remove_file(&db).unwrap();
        let connection = Connection::open(&db).unwrap();
        connection.execute_batch("CREATE VIEW workspaces AS SELECT '/fixture' AS path, 1 AS last_opened_at;").unwrap();
        drop(connection);
        assert!(workbuddy_projects_at(temp.path(), None).is_err());
    }

    #[test]
    fn workbuddy_unreadable_source_is_unknown_and_does_not_leak_or_create() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("workbuddy.db"); let scope = workbuddy_scope(temp.path());
        let error = workbuddy_paths_local(&db, &scope).unwrap_err().to_string();
        assert!(error.contains("状态未知") && !db.exists());
        std::fs::write(&db, "private-corrupt-body").unwrap();
        let before = std::fs::read(&db).unwrap();
        let error = workbuddy_paths_local(&db, &scope).unwrap_err().to_string();
        assert!(error.contains("状态未知") && !error.contains("private-corrupt-body"));
        assert!(!error.contains(temp.path().to_str().unwrap()));
        assert_eq!(std::fs::read(&db).unwrap(), before);
    }

    #[test]
    fn workbuddy_combines_session_only_paths_with_catalog_and_keeps_archive_unknown() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let one = root.join("one"); let two = root.join("two");
        std::fs::create_dir(&one).unwrap(); std::fs::create_dir(&two).unwrap();
        let connection = catalog(&root);
        connection.execute("INSERT INTO workspaces VALUES(?1, 1)", [one.to_str().unwrap()]).unwrap();
        for (path, user) in [(one.join("."), "reader-current"), (two.clone(), "")] {
            connection.execute("INSERT INTO sessions(cwd,user_id,transport,is_playground) VALUES(?1,?2,'local',0)",
                rusqlite::params![path.to_str().unwrap(), user]).unwrap();
        }
        // A session's archived status does not establish workspace archival.
        connection.execute("UPDATE sessions SET status='archived' WHERE user_id=''", []).unwrap();
        #[cfg(unix)]
        {
            let alias = root.join("alias");
            std::os::unix::fs::symlink(&two, &alias).unwrap();
            connection.execute("INSERT INTO workspaces VALUES(?1, 2)", [alias.to_str().unwrap()]).unwrap();
        }
        drop(connection);
        let db = root.join("workbuddy.db"); let before = std::fs::read(&db).unwrap();
        let mut scope = workbuddy_scope(&root); scope.current_user_id = " reader-current ".into();
        let discovery = workbuddy_projects_at(&root, Some(scope)).unwrap();
        assert_eq!(discovery.completeness, WorkBuddyCompleteness::Complete);
        let entries = discovery.projects;
        assert_eq!(entries.iter().map(|p| p.path.clone()).collect::<BTreeSet<_>>(), BTreeSet::from([one, two]));
        for entry in entries {
            assert_eq!(entry.agents, [AgentKind::WorkBuddy]);
            assert!(!entry.archived && !entry.hidden && !entry.retained);
            assert!(entry.archive_states.is_empty() && entry.native_workspaces.is_empty());
        }
        assert_eq!(std::fs::read(db).unwrap(), before);
        for suffix in ["-wal", "-shm", "-journal"] {
            assert!(!root.join(format!("workbuddy.db{suffix}")).exists());
        }
    }

    #[test]
    fn workbuddy_session_sql_filters_account_transport_deletion_and_quick_ask() {
        let temp = tempfile::tempdir().unwrap();
        let connection = catalog(temp.path());
        connection.execute("DELETE FROM sessions", []).unwrap();
        // Only the first two accounts are visible. NULL is not legacy empty uid.
        for (path, user, transport, deleted, settings) in [
            ("/current", Some("reader-current"), "local", None, None),
            ("/legacy", Some(""), "local", None, None),
            ("/foreign", Some("other-user"), "local", None, None),
            ("/uid-case", Some("READER-CURRENT"), "local", None, None),
            ("/null-uid", None, "local", None, None),
            ("/cloud", Some("reader-current"), "cloud", None, None),
            ("/transport-case", Some("reader-current"), "LOCAL", None, None),
            ("/deleted", Some("reader-current"), "local", Some(0), None),
            ("/tombstone", Some("reader-current"), "local", Some(-1), None),
            ("/quick", Some("reader-current"), "local", None, Some(r#"{"conversationTags":{"wbQuickAsk":"1"}}"#)),
            ("/numeric-tag", Some("reader-current"), "local", None, Some(r#"{"conversationTags":{"wbQuickAsk":1}}"#)),
            ("/empty-settings", Some("reader-current"), "local", None, Some("")),
        ] {
            connection.execute("INSERT INTO sessions(cwd,user_id,transport,deleted_at,is_playground,session_settings,title) VALUES(?1,?2,?3,?4,0,?5,'private-body-not-projected')",
                rusqlite::params![p(path),user,transport,deleted,settings]).unwrap();
        }
        drop(connection);
        let paths = workbuddy_paths_local(&temp.path().join("workbuddy.db"), &workbuddy_scope(temp.path())).unwrap();
        assert_eq!(paths.into_iter().collect::<BTreeSet<_>>(), BTreeSet::from([
            p("/current"), p("/legacy"), p("/numeric-tag"), p("/empty-settings"),
        ]));
    }

    #[test]
    fn workbuddy_space_filter_matches_explicit_false_null_and_assistant_identity() {
        let scope = WorkBuddyScope { current_user_id: "reader-current".into(), assistant_cwd: PathBuf::from(p("/native/Claw")) };
        assert!(workbuddy_session_workspace(&p("/native/project"), None, None, false, &scope));
        assert!(workbuddy_session_workspace(&p("/native/2026-10-21-10-11-12"), Some(false), None, false, &scope));
        assert!(!workbuddy_session_workspace(&p("/native/2026-10-21-10-11-12/"), None, None, false, &scope));
        assert!(workbuddy_session_workspace(&p("/native/2026-10-21-10-11-1x"), None, None, false, &scope));
        assert!(!workbuddy_session_workspace(&p("/native/project"), Some(true), None, false, &scope));
        assert!(!workbuddy_session_workspace(&p("/native/project"), Some(false), Some("team-project"), false, &scope));
        assert!(!workbuddy_session_workspace(&p("/NATIVE/x/../Claw/"), Some(false), None, false, &scope));
        assert!(workbuddy_session_workspace(&p("/other/Claw"), Some(false), Some(""), false, &scope));
        assert!(!workbuddy_session_workspace("relative", Some(false), None, false, &scope));
    }

    #[test]
    fn workbuddy_ambiguous_visibility_and_missing_schema_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let connection = catalog(temp.path());
        assert!(sqlite_paths(&temp.path().join("workbuddy.db"), &[WORKBUDDY_SESSIONS_SQL]).is_err());
        connection.execute("DELETE FROM sessions", []).unwrap();
        for settings in [
            r#"{"conversationTags":{"wbQuickAsk":"1","wbQuickAsk":"0"}}"#,
            r#"{"conversationTags":{"wbQuickAsk":"1"},"conversationTags":{}}"#,
        ] {
            connection.execute("INSERT INTO sessions(cwd,user_id,transport,is_playground,session_settings) VALUES('/project','reader-current','local',0,?1)", [settings]).unwrap();
            let error = workbuddy_paths_local(&temp.path().join("workbuddy.db"), &workbuddy_scope(temp.path())).unwrap_err().to_string();
            assert!(error.contains("状态未知") && !error.contains(settings));
            connection.execute("DELETE FROM sessions", []).unwrap();
        }
        connection.execute_batch("DROP TABLE sessions; CREATE VIEW sessions AS SELECT '/private' AS cwd;").unwrap();
        let error = workbuddy_paths_local(&temp.path().join("workbuddy.db"), &workbuddy_scope(temp.path())).unwrap_err().to_string();
        assert!(error.contains("结构无效") && !error.contains("/private"));
    }

    #[test]
    fn workbuddy_json_parse_failure_is_unknown_without_partial_paths() {
        let temp = tempfile::tempdir().unwrap();
        let connection = catalog(temp.path());
        connection.execute("INSERT INTO workspaces VALUES('/before-catalog', 1)", []).unwrap();
        connection.execute("INSERT INTO sessions(cwd,user_id,transport,is_playground) VALUES('/earlier','reader-current','local',0)", []).unwrap();
        let deep = [r#"{"conversationTags":{"wbQuickAsk":"1"},"extra":"#, &"[".repeat(2200),
            "0", &"]".repeat(2200), "}"].concat();
        assert!(deep.len() < MAX_PATH_BYTES);
        for settings in ["not-json", deep.as_str()] {
            connection.execute("INSERT INTO sessions(cwd,user_id,transport,is_playground,session_settings) VALUES('/hidden','reader-current','local',0,?1)", [settings]).unwrap();
            let db = temp.path().join("workbuddy.db");
            let before = std::fs::read(&db).unwrap();
            let error = workbuddy_paths_local(&db, &workbuddy_scope(temp.path())).unwrap_err().to_string();
            assert!(error.contains("状态未知") && !error.contains(settings) && !error.contains("/earlier"));
            assert_eq!(std::fs::read(db).unwrap(), before);
            connection.execute("DELETE FROM sessions WHERE cwd='/hidden'", []).unwrap();
        }
    }

    #[test]
    fn workbuddy_settings_byte_limit_precedes_json_and_rejects_partial_paths() {
        use rusqlite::types::Value;
        let temp = tempfile::tempdir().unwrap();
        let connection = catalog(temp.path());
        connection.execute("INSERT INTO workspaces VALUES(?1, 1)", [p("/before-catalog")]).unwrap();
        connection.execute("INSERT INTO sessions(cwd,user_id,transport,is_playground) VALUES(?1,'reader-current','local',0)", [p("/session")]).unwrap();
        let overhead = serde_json::json!({"padding": ""}).to_string().len();
        let exact = serde_json::json!({"padding": "x".repeat(MAX_PATH_BYTES - overhead)}).to_string();
        let over = serde_json::json!({"padding": "x".repeat(MAX_PATH_BYTES + 1 - overhead)}).to_string();
        let unicode = serde_json::json!({"padding": "界".repeat(MAX_PATH_BYTES / 3)}).to_string();
        assert_eq!(exact.len(), MAX_PATH_BYTES);
        assert_eq!(over.len(), MAX_PATH_BYTES + 1);
        assert!(unicode.len() > MAX_PATH_BYTES && unicode.chars().count() < MAX_PATH_BYTES);
        let db = temp.path().join("workbuddy.db");
        for (settings, valid) in [(Value::Null, true), (Value::Text(String::new()), true),
            (Value::Text(exact), true), (Value::Text(over), false), (Value::Text(unicode), false),
            (Value::Blob(b"{}".to_vec()), false)] {
            connection.execute("UPDATE sessions SET session_settings=?1", [settings]).unwrap();
            let before = std::fs::read(&db).unwrap();
            let result = workbuddy_paths_local(&db, &workbuddy_scope(temp.path()));
            if valid {
                assert_eq!(result.unwrap().into_iter().collect::<BTreeSet<_>>(),
                    BTreeSet::from([p("/before-catalog"), p("/session")]));
            } else {
                assert!(result.unwrap_err().to_string().contains("状态未知"));
            }
            assert_eq!(std::fs::read(&db).unwrap(), before);
        }
    }

    #[test]
    fn workbuddy_combined_raw_row_and_metadata_limits_fail_without_partial_success() {
        let temp = tempfile::tempdir().unwrap();
        let connection = catalog(temp.path());
        connection.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000)
            INSERT INTO workspaces SELECT '/' || x, x FROM n;").unwrap();
        let db = temp.path().join("workbuddy.db"); let scope = workbuddy_scope(temp.path());
        assert_eq!(workbuddy_paths_local(&db, &scope).unwrap().len(), MAX_PATHS);
        connection.execute("INSERT INTO sessions(cwd,user_id,transport,is_playground) VALUES('/overflow','reader-current','local',0)", []).unwrap();
        assert!(workbuddy_paths_local(&db, &scope).unwrap_err().to_string().contains("读取上限"));
        connection.execute("DELETE FROM workspaces", []).unwrap();
        connection.execute("UPDATE sessions SET cwd=?1 WHERE user_id='reader-current'", ["x".repeat(MAX_PATH_BYTES + 1)]).unwrap();
        assert!(workbuddy_paths_local(&db, &scope).unwrap_err().to_string().contains("读取上限"));
        connection.execute("UPDATE sessions SET cwd=x'00' WHERE user_id='reader-current'", []).unwrap();
        assert!(workbuddy_paths_local(&db, &scope).is_err());
    }

    #[test]
    fn missing_corrupt_and_untrusted_schema_fail_without_creating_or_leaking() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("workbuddy.db");
        assert!(sqlite_paths(&db, &[WORKBUDDY_PATHS_SQL]).is_err());
        assert!(!db.exists());
        std::fs::write(&db, "secret-corrupt-data").unwrap();
        let error = sqlite_paths(&db, &[WORKBUDDY_PATHS_SQL]).unwrap_err().to_string();
        assert!(!error.contains("secret") && !error.contains(temp.path().to_str().unwrap()));
        std::fs::remove_file(&db).unwrap();
        let connection = Connection::open(&db).unwrap();
        connection.execute_batch("CREATE VIEW workspaces AS SELECT 'secret' AS path, 1 AS last_opened_at").unwrap();
        assert!(sqlite_paths(&db, &[WORKBUDDY_PATHS_SQL]).is_err());
        connection.execute_batch("DROP VIEW workspaces; CREATE TABLE workspaces(path BLOB, last_opened_at INTEGER)").unwrap();
        assert!(sqlite_paths(&db, &[WORKBUDDY_PATHS_SQL]).is_err());
        assert!(sqlite_paths(&db, &["SELECT title FROM sessions"]).is_err());
        assert!(sqlite_paths(&db, &[]).is_err());
    }

    #[test]
    fn row_and_path_limits_fail_instead_of_returning_partial_catalogs() {
        let temp = tempfile::tempdir().unwrap();
        let connection = catalog(temp.path());
        connection.execute_batch(
            "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000)
             INSERT INTO workspaces SELECT '/' || x, x FROM n;",
        ).unwrap();
        let db = temp.path().join("workbuddy.db");
        assert_eq!(sqlite_paths(&db, &[WORKBUDDY_PATHS_SQL]).unwrap().len(), MAX_PATHS);
        connection.execute("INSERT INTO workspaces VALUES('/overflow', 0)", []).unwrap();
        assert!(sqlite_paths(&db, &[WORKBUDDY_PATHS_SQL]).unwrap_err().to_string().contains("读取上限"));
        connection.execute("DELETE FROM workspaces", []).unwrap();
        connection.execute("INSERT INTO workspaces VALUES(?1, 0)", ["x".repeat(MAX_PATH_BYTES + 1)]).unwrap();
        assert!(sqlite_paths(&db, &[WORKBUDDY_PATHS_SQL]).unwrap_err().to_string().contains("路径超过"));
        connection.execute("UPDATE workspaces SET path = x'00'", []).unwrap();
        assert!(sqlite_paths(&db, &[WORKBUDDY_PATHS_SQL]).is_err());
    }
}
