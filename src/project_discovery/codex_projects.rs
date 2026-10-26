//! Read-only Codex rust-v0.163.0 project-table projection (not thread history).
//! The parent owns SQLite-home selection, source completeness and legacy merging.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Result, anyhow, bail};
use rusqlite::types::ValueRef;
use serde::{Deserialize, Serialize};

use super::{
    AgentKind, NativeWorkspace, ProjectEntry, metadata_worker::{self, Projection, Reply},
    path_identity, project_name,
};

const DATABASE: &str = "state_5.sqlite";
const MAX_ROWS: usize = 10_000;
const MAX_TEXT_BYTES: usize = 16 * 1024;
// Charge every joined row, including duplicates, before any owned text is made.
// This also leaves room for worst-case JSON escaping in the 32 MiB worker reply.
const MAX_METADATA_BYTES: usize = 4 * 1024 * 1024;
const PROJECTS_SQL: &str = "SELECT p.id, p.name, r.path, r.position FROM projects AS p
    JOIN project_roots AS r ON p.id COLLATE BINARY = r.project_id COLLATE BINARY
    LIMIT 10001";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CatalogProject {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) roots: Vec<String>,
}

/// Runs only inside the metadata worker in product builds. The shared VFS has
/// already started the read transaction; validation and projection share it.
pub(super) fn read_local(db: &Path) -> Result<Vec<CatalogProject>> {
    let connection = super::sqlite_vfs::open_readonly(db)
        .map_err(|_| anyhow!("Codex 原生项目索引暂不可读"))?;
    super::workspaces::validate_native_schema(
        &connection, "projects", &[("id", "TEXT"), ("name", "TEXT")],
    ).map_err(|_| anyhow!("Codex 原生项目索引结构无效"))?;
    super::workspaces::validate_native_schema(
        &connection, "project_roots",
        &[("project_id", "TEXT"), ("path", "TEXT"), ("position", "INTEGER")],
    ).map_err(|_| anyhow!("Codex 原生项目索引结构无效"))?;
    let mut statement = connection.prepare(PROJECTS_SQL)
        .map_err(|_| anyhow!("Codex 原生项目索引结构无效"))?;
    let mut rows = statement.query([])
        .map_err(|_| anyhow!("Codex 原生项目索引暂不可读"))?;
    let mut projects = BTreeMap::<String, (String, BTreeMap<i64, String>)>::new();
    let mut rows_seen = 0usize;
    let mut bytes_seen = 0usize;
    while let Some(row) = rows.next()
        .map_err(|_| anyhow!("Codex 原生项目索引格式无效"))? {
        if rows_seen >= MAX_ROWS { bail!("Codex 原生项目索引超过读取上限"); }
        rows_seen += 1;
        // get_ref + borrowed validation prevents allocating an unbounded value.
        let id = text(row.get_ref(0).map_err(|_| anyhow!("Codex 原生项目元数据格式无效"))?)?;
        let name = text(row.get_ref(1).map_err(|_| anyhow!("Codex 原生项目元数据格式无效"))?)?;
        let root = text(row.get_ref(2).map_err(|_| anyhow!("Codex 原生项目元数据格式无效"))?)?;
        let position = match row.get_ref(3)
            .map_err(|_| anyhow!("Codex 原生项目根顺序格式无效"))? {
            ValueRef::Integer(value) if value >= 0 => value,
            _ => bail!("Codex 原生项目根顺序格式无效"),
        };
        if id.trim().is_empty() { bail!("Codex 原生项目标识无效"); }
        charge(&mut bytes_seen, id.len() + name.len() + root.len(), MAX_METADATA_BYTES)?;
        if let Some((previous_name, roots)) = projects.get_mut(id) {
            if previous_name.as_str() != name { bail!("Codex 原生项目标识冲突"); }
            if let Some(previous_root) = roots.get(&position) {
                if previous_root.as_str() != root { bail!("Codex 原生项目根顺序冲突"); }
            } else {
                roots.insert(position, root.to_owned());
            }
        } else {
            projects.insert(id.to_owned(), (name.to_owned(), BTreeMap::from([(position, root.to_owned())])));
        }
    }
    // Native root order is position ASC. Sort only the bounded result in Rust;
    // duplicate paths retain their earliest position without a SQL-wide sort.
    Ok(projects.into_iter().map(|(id, (name, roots))| {
        let mut seen = BTreeSet::new();
        CatalogProject {
            id, name, roots: roots.into_values().filter(|root| seen.insert(root.clone())).collect(),
        }
    }).collect())
}

fn text<'a>(value: ValueRef<'a>) -> Result<&'a str> {
    let ValueRef::Text(bytes) = value else { bail!("Codex 原生项目元数据格式无效"); };
    if bytes.len() > MAX_TEXT_BYTES { bail!("Codex 原生项目元数据超过读取上限"); }
    if bytes.contains(&0) { bail!("Codex 原生项目元数据格式无效"); }
    std::str::from_utf8(bytes).map_err(|_| anyhow!("Codex 原生项目元数据格式无效"))
}

fn charge(used: &mut usize, bytes: usize, limit: usize) -> Result<()> {
    if bytes > limit.saturating_sub(*used) { bail!("Codex 原生项目索引超过读取上限"); }
    *used += bytes;
    Ok(())
}

/// Pure selection after the caller has resolved native config and env paths.
/// rust-v0.163.0 applies managed sqlite_home requirements to effective config,
/// then chooses effective config > resolved CODEX_SQLITE_HOME > Codex home.
/// The env input must already reflect var().ok(), trim/empty handling, tilde/
/// platform normalization and resolution against the native resolved_cwd.
/// This is not a config loader; an unknown effective config is not proven None.
pub(super) fn sqlite_home_from_resolved(
    codex_home: &Path,
    effective_config_home: Option<&Path>,
    resolved_env_home: Option<&Path>,
) -> Result<PathBuf> {
    let selected = effective_config_home.or(resolved_env_home).unwrap_or(codex_home);
    if !selected.is_absolute() || selected.as_os_str().len() > MAX_TEXT_BYTES
        || selected.to_str().is_none_or(|s| s.contains('\0')) {
        bail!("Codex SQLite 数据根必须是有效绝对路径");
    }
    Ok(selected.to_path_buf())
}

/// The caller supplies an already selected absolute SQLite home. Missing/error
/// handling and any CODEX_SQLITE_HOME/config precedence belong to the parent.
pub(super) fn projects_at(sqlite_home: &Path) -> Result<Vec<ProjectEntry>> {
    if !sqlite_home.is_absolute() { bail!("Codex SQLite 数据根必须是绝对路径"); }
    let db = sqlite_home.join(DATABASE);
    let rows = match metadata_worker::read(&db, Projection::CodexProjects)? {
        Reply::CodexProjects { rows } => rows,
        _ => bail!("Codex 原生项目索引格式无效"),
    };
    entries(rows, &db)
}

fn entries(rows: Vec<CatalogProject>, db: &Path) -> Result<Vec<ProjectEntry>> {
    if rows.len() > MAX_ROWS { bail!("Codex 原生项目索引超过读取上限"); }
    let mut projects = BTreeMap::<PathBuf, ProjectEntry>::new();
    let mut ids = BTreeSet::new();
    let mut roots_seen = 0usize;
    let mut bytes_seen = 0usize;
    // Like Hermes, anchor workspace identity to the catalog + opaque native id,
    // not to one member root. Membership changes do not change workspace id.
    // This path does not claim a native primary root. Member directories remain
    // separate, retaining position order after canonical aliases are deduplicated.
    let source = path_identity(db);
    for row in rows {
        text(ValueRef::Text(row.id.as_bytes()))?;
        text(ValueRef::Text(row.name.as_bytes()))?;
        if row.id.trim().is_empty() || !ids.insert(row.id.clone()) {
            bail!("Codex 原生项目标识无效或重复");
        }
        charge(&mut bytes_seen, row.id.len() + row.name.len(), MAX_METADATA_BYTES)?;
        let mut members = Vec::new();
        let mut seen = BTreeSet::new();
        for raw in row.roots {
            charge(&mut roots_seen, 1, MAX_ROWS)?;
            text(ValueRef::Text(raw.as_bytes()))?;
            charge(&mut bytes_seen, raw.len(), MAX_METADATA_BYTES)?;
            let path = Path::new(&raw);
            if !path.is_absolute() { continue; }
            let Ok(path) = path.canonicalize() else { continue; };
            if !path.is_absolute() || !path.is_dir()
                || path.to_str().is_none_or(|s| s.len() > MAX_TEXT_BYTES || s.contains('\0')) {
                continue;
            }
            if seen.insert(path.clone()) { members.push(path); }
        }
        let workspace = NativeWorkspace {
            agent: AgentKind::Codex,
            source_id: Some(row.id),
            display_name: (!row.name.trim().is_empty()).then_some(row.name),
            archived: None,
            path: source.clone(),
            project_roots: Arc::new(members),
        };
        // One Finder row per canonical directory; all distinct native ids survive.
        for path in workspace.project_roots.iter().cloned() {
            let entry = projects.entry(path.clone()).or_insert_with(|| ProjectEntry {
                name: project_name(&path), path, agents: vec![AgentKind::Codex],
                proma_workspaces: Vec::new(), native_workspaces: Vec::new(),
                archive_states: Vec::new(), archived: false, hidden: false, retained: false,
            });
            entry.native_workspaces.push(workspace.clone());
        }
    }
    Ok(projects.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{Connection, params, types::Value};
    use std::fs;

    // Synthetic contract fixtures, never proof of native Codex registration.
    const SCHEMA: &str = "CREATE TABLE projects(id TEXT, name TEXT);
        CREATE TABLE project_roots(project_id TEXT, path TEXT, position INTEGER);";

    fn fixture(schema: &str) -> (tempfile::TempDir, PathBuf, Connection) {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let connection = Connection::open(home.join(DATABASE)).unwrap();
        connection.execute_batch(schema).unwrap();
        (temp, home, connection)
    }

    fn directory(home: &Path, name: &str) -> PathBuf {
        let dir = home.join(name);
        fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn project(connection: &Connection, id: &str, name: &str, roots: &[&str]) {
        connection.execute("INSERT INTO projects(id, name) VALUES (?1, ?2)", params![id, name]).unwrap();
        for (position, root) in roots.iter().enumerate() {
            connection.execute(
                "INSERT INTO project_roots(project_id, path, position) VALUES (?1, ?2, ?3)",
                params![id, root, position as i64],
            ).unwrap();
        }
    }

    #[test]
    fn contract_two_projects_multi_root_and_shared_directory_keep_identity() {
        let (_temp, home, connection) = fixture(SCHEMA);
        let first = directory(&home, "first");
        let second = directory(&home, "second");
        project(&connection, "p1", "Native one", &[first.to_str().unwrap(), second.to_str().unwrap(), first.to_str().unwrap()]);
        project(&connection, "p2", "Native two", &[first.to_str().unwrap()]);
        project(&connection, "without-roots", "Empty", &[]);
        connection.execute(
            "INSERT INTO project_roots VALUES ('orphan', ?1, 0)", [second.to_str().unwrap()],
        ).unwrap();
        drop(connection);
        let catalog = read_local(&home.join(DATABASE)).unwrap();
        assert_eq!(catalog.len(), 2);
        assert_eq!(catalog[0].id, "p1");
        assert_eq!(catalog[0].roots.len(), 2);
        let entries = projects_at(&home).unwrap();
        assert_eq!(entries.len(), 2);
        let first_entry = entries.iter().find(|entry| entry.path == first).unwrap();
        let second_entry = entries.iter().find(|entry| entry.path == second).unwrap();
        assert_eq!(first_entry.native_workspaces.len(), 2);
        assert_eq!(second_entry.native_workspaces.len(), 1);
        let w1 = &first_entry.native_workspaces[0];
        let w2 = &first_entry.native_workspaces[1];
        assert_eq!(w1.source_id.as_deref(), Some("p1"));
        assert_eq!(w2.source_id.as_deref(), Some("p2"));
        assert_ne!(w1.id(), w2.id());
        assert_eq!(w1.id(), second_entry.native_workspaces[0].id());
        assert_eq!(w1.path, home.join(DATABASE));
        assert_eq!(w1.display_name.as_deref(), Some("Native one"));
        assert_eq!(w1.project_roots.as_ref(), &[first.clone(), second]);
        assert!(Arc::ptr_eq(&w1.project_roots, &second_entry.native_workspaces[0].project_roots));
        assert_eq!(w2.project_roots.as_ref(), &[first]);
        assert!(entries.iter().all(|entry| entry.agents == [AgentKind::Codex]
            && !entry.archived && !entry.hidden && !entry.retained
            && entry.archive_states.is_empty() && entry.proma_workspaces.is_empty()
            && entry.native_workspaces.iter().all(|workspace| workspace.archived.is_none())));
    }

    #[test]
    fn contract_invalid_roots_are_skipped_and_empty_catalog_is_valid() {
        let (_temp, home, connection) = fixture(SCHEMA);
        let valid = directory(&home, "valid");
        let file = home.join("file");
        fs::write(&file, b"not a directory").unwrap();
        let missing = home.join("missing");
        project(&connection, "p", "", &["", "relative", "file:///not-a-path",
            file.to_str().unwrap(), missing.to_str().unwrap(), valid.to_str().unwrap()]);
        drop(connection);
        let entries = projects_at(&home).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, valid);
        assert!(entries[0].native_workspaces[0].display_name.is_none());
        let (_empty_temp, empty_home, connection) = fixture(SCHEMA);
        drop(connection);
        assert!(projects_at(&empty_home).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn contract_directory_aliases_merge_without_losing_ids() {
        let (_temp, home, connection) = fixture(SCHEMA);
        let valid = directory(&home, "valid");
        let alias = home.join("alias");
        std::os::unix::fs::symlink(&valid, &alias).unwrap();
        project(&connection, "p1", "One", &[valid.to_str().unwrap(), alias.to_str().unwrap()]);
        project(&connection, "p2", "Two", &[alias.to_str().unwrap()]);
        drop(connection);
        let entries = projects_at(&home).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, valid);
        assert_eq!(entries[0].native_workspaces.len(), 2);
        assert!(entries[0].native_workspaces.iter().all(|workspace| workspace.project_roots.len() == 1));
    }

    #[test]
    fn contract_null_blob_numeric_and_nul_metadata_fail_closed() {
        for field in ["id", "name", "path"] {
            for value in [Value::Null, Value::Blob(vec![b'x']), Value::Text("a\0b".into())] {
                let (_temp, home, connection) = fixture(SCHEMA);
                project(&connection, "p", "Name", &["/synthetic"]);
                match field {
                    "id" => {
                        connection.execute("UPDATE projects SET id = ?1", params![value]).unwrap();
                        connection.execute("UPDATE project_roots SET project_id = ?1", params![value]).unwrap();
                    }
                    "name" => { connection.execute("UPDATE projects SET name = ?1", params![value]).unwrap(); }
                    _ => { connection.execute("UPDATE project_roots SET path = ?1", params![value]).unwrap(); }
                }
                drop(connection);
                // NULL join keys are orphans and never appear in the projection.
                if field == "id" && matches!(value, Value::Null) {
                    assert!(read_local(&home.join(DATABASE)).unwrap().is_empty());
                } else {
                    assert!(read_local(&home.join(DATABASE)).is_err(), "{field}");
                }
            }
        }
        // TEXT affinity converts ordinary numeric inserts to text; use borrowed
        // values directly to verify the runtime storage-class guard.
        assert!(text(ValueRef::Integer(7)).is_err());
        assert!(text(ValueRef::Real(1.5)).is_err());
        for id in ["", "  "] {
            let (_temp, home, connection) = fixture(SCHEMA);
            project(&connection, id, "Name", &["/synthetic"]);
            drop(connection);
            assert!(read_local(&home.join(DATABASE)).is_err());
        }
    }

    #[test]
    fn contract_invalid_utf8_in_each_projected_column_fails_closed() {
        for sql in [
            "UPDATE projects SET id = CAST(x'80' AS TEXT);
             UPDATE project_roots SET project_id = CAST(x'80' AS TEXT);",
            "UPDATE projects SET name = CAST(x'80' AS TEXT);",
            "UPDATE project_roots SET path = CAST(x'80' AS TEXT);",
        ] {
            let (_temp, home, connection) = fixture(SCHEMA);
            project(&connection, "p", "Name", &["/synthetic"]);
            connection.execute_batch(sql).unwrap();
            drop(connection);
            assert!(read_local(&home.join(DATABASE)).is_err());
        }
    }

    #[test]
    fn contract_text_lengths_and_aggregate_budget_are_bounded() {
        assert!(text(ValueRef::Text(&vec![b'x'; MAX_TEXT_BYTES])).is_ok());
        assert!(text(ValueRef::Text(&vec![b'x'; MAX_TEXT_BYTES + 1])).is_err());
        for sql in [
            "UPDATE projects SET id = ?1",
            "UPDATE projects SET name = ?1",
            "UPDATE project_roots SET path = ?1",
        ] {
            let (_temp, home, connection) = fixture(SCHEMA);
            project(&connection, "p", "Name", &["/synthetic"]);
            let oversized = "x".repeat(MAX_TEXT_BYTES + 1);
            connection.execute(sql, [&oversized]).unwrap();
            if sql == "UPDATE projects SET id = ?1" {
                connection.execute("UPDATE project_roots SET project_id = ?1", [&oversized]).unwrap();
            }
            drop(connection);
            assert!(read_local(&home.join(DATABASE)).is_err());
        }
        // Parameterized small budget proves the arithmetic without giant data.
        let mut used = 0;
        charge(&mut used, 8, 8).unwrap();
        assert!(charge(&mut used, 1, 8).is_err());
        assert_eq!(used, 8);
        assert!(charge(&mut used, usize::MAX, 8).is_err());
    }

    #[test]
    fn contract_conflicting_names_for_one_id_fail_closed() {
        let (_temp, home, connection) = fixture(SCHEMA);
        project(&connection, "p", "One", &["/synthetic"]);
        connection.execute("INSERT INTO projects VALUES ('p', 'Two')", []).unwrap();
        drop(connection);
        assert!(read_local(&home.join(DATABASE)).is_err());
    }

    #[test]
    fn contract_views_virtual_generated_and_wrong_schemas_are_rejected() {
        for schema in [
            "CREATE VIEW projects AS SELECT 'p' AS id, 'Name' AS name;
             CREATE TABLE project_roots(project_id TEXT, path TEXT, position INTEGER);",
            "CREATE TABLE projects(id TEXT, name TEXT);
             CREATE VIEW project_roots AS SELECT 'p' AS project_id, '/x' AS path, 0 AS position;",
            "CREATE VIRTUAL TABLE projects USING fts5(id, name);
             CREATE TABLE project_roots(project_id TEXT, path TEXT, position INTEGER);",
            "CREATE TABLE projects(id TEXT, name TEXT);
             CREATE VIRTUAL TABLE project_roots USING fts5(project_id, path, position);",
            "CREATE TABLE projects(id TEXT, name BLOB);
             CREATE TABLE project_roots(project_id TEXT, path TEXT, position INTEGER);",
            "CREATE TABLE projects(id TEXT, name TEXT);
             CREATE TABLE project_roots(project_id TEXT, path TEXT, position TEXT);",
            "CREATE TABLE projects(id TEXT, name TEXT);
             CREATE TABLE project_roots(project_id TEXT, path TEXT);",
            "CREATE TABLE projects(seed TEXT, id TEXT GENERATED ALWAYS AS (seed) VIRTUAL, name TEXT);
             CREATE TABLE project_roots(project_id TEXT, path TEXT, position INTEGER);",
            "CREATE TABLE projects(id TEXT, name TEXT);
             CREATE TABLE project_roots(project_id TEXT, seed TEXT, path TEXT GENERATED ALWAYS AS (seed) STORED, position INTEGER);",
            "CREATE TABLE unrelated(path TEXT);",
        ] {
            let (_temp, home, connection) = fixture(schema);
            drop(connection);
            assert!(read_local(&home.join(DATABASE)).is_err(), "{schema}");
        }
    }

    #[test]
    fn contract_join_uses_exact_ids_even_when_schema_has_nocase_collation() {
        let (_temp, home, connection) = fixture(
            "CREATE TABLE projects(id TEXT COLLATE NOCASE, name TEXT);
             CREATE TABLE project_roots(project_id TEXT, path TEXT, position INTEGER);",
        );
        project(&connection, "p", "Name", &[]);
        connection.execute("INSERT INTO project_roots VALUES ('P', '/orphan', 0)", []).unwrap();
        drop(connection);
        assert!(read_local(&home.join(DATABASE)).unwrap().is_empty());
    }

    #[test]
    fn contract_join_row_sentinel_counts_duplicate_roots_before_deduplication() {
        for count in [MAX_ROWS, MAX_ROWS + 1] {
            let (_temp, home, mut connection) = fixture(SCHEMA);
            project(&connection, "p", "Name", &[]);
            let transaction = connection.transaction().unwrap();
            {
                let mut insert = transaction.prepare("INSERT INTO project_roots VALUES ('p', '/synthetic', ?1)").unwrap();
                for position in 0..count { insert.execute([position as i64]).unwrap(); }
            }
            transaction.commit().unwrap();
            drop(connection);
            let result = read_local(&home.join(DATABASE));
            if count == MAX_ROWS {
                let rows = result.unwrap();
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].roots, ["/synthetic"]);
            } else { assert!(result.is_err()); }
        }
    }

    #[test]
    fn contract_native_root_positions_survive_canonicalization_and_reordering_keeps_id() {
        let (_temp, home, connection) = fixture(SCHEMA);
        let primary = directory(&home, "z-native-first");
        let secondary = directory(&home, "a-native-second");
        project(&connection, "p", "Name", &[]);
        for (position, root) in [(30, &secondary), (10, &primary), (20, &secondary), (20, &secondary)] {
            connection.execute(
                "INSERT INTO project_roots VALUES ('p', ?1, ?2)",
                params![root.to_str().unwrap(), position],
            ).unwrap();
        }
        drop(connection);
        let rows = read_local(&home.join(DATABASE)).unwrap();
        assert_eq!(rows[0].roots, [primary.to_str().unwrap(), secondary.to_str().unwrap()]);
        let before = entries(rows, &home.join(DATABASE)).unwrap();
        let workspace = &before[0].native_workspaces[0];
        assert_eq!(workspace.project_roots.as_ref(), &[primary.clone(), secondary.clone()]);
        assert_eq!(workspace.path, home.join(DATABASE));
        assert!(Arc::ptr_eq(&workspace.project_roots, &before[1].native_workspaces[0].project_roots));
        let reordered = entries(vec![CatalogProject {
            id: "p".into(), name: "Name".into(),
            roots: vec![secondary.to_str().unwrap().into(), primary.to_str().unwrap().into()],
        }], &home.join(DATABASE)).unwrap();
        assert_eq!(workspace.id(), reordered[0].native_workspaces[0].id());
        assert_eq!(reordered[0].native_workspaces[0].project_roots.as_ref(), &[secondary, primary]);
    }

    #[test]
    fn contract_invalid_or_conflicting_root_positions_fail_closed() {
        for value in [Value::Null, Value::Integer(-1), Value::Real(1.5),
            Value::Blob(vec![b'0']), Value::Text("invalid".into())] {
            let (_temp, home, connection) = fixture(SCHEMA);
            project(&connection, "p", "Name", &["/synthetic"]);
            connection.execute("UPDATE project_roots SET position = ?1", params![value]).unwrap();
            drop(connection);
            assert!(read_local(&home.join(DATABASE)).is_err());
        }
        let (_temp, home, connection) = fixture(SCHEMA);
        project(&connection, "p", "Name", &["/first"]);
        connection.execute("INSERT INTO project_roots VALUES ('p', '/different', 0)", []).unwrap();
        drop(connection);
        assert!(read_local(&home.join(DATABASE)).is_err());
    }

    #[test]
    fn contract_only_selected_absolute_root_and_state_5_are_read() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let old = Connection::open(home.join("state_4.sqlite")).unwrap();
        old.execute_batch(SCHEMA).unwrap();
        drop(old);
        assert!(projects_at(Path::new("relative")).is_err());
        assert!(projects_at(&home).is_err());
        assert!(!home.join(DATABASE).exists());
        let missing_home = home.join("missing-home");
        assert!(projects_at(&missing_home).is_err());
        assert!(!missing_home.exists());
        fs::write(home.join(DATABASE), b"not sqlite").unwrap();
        assert!(projects_at(&home).is_err());
    }

    #[derive(Debug, PartialEq, Eq)]
    struct FileSnapshot {
        bytes: Vec<u8>,
        modified: std::time::SystemTime,
        readonly: bool,
        #[cfg(unix)]
        identity: (u64, u64, u32, u32, u32, u64, i64, i64),
    }

    fn snapshot(path: &Path) -> FileSnapshot {
        let bytes = fs::read(path).unwrap();
        let metadata = fs::metadata(path).unwrap();
        FileSnapshot {
            bytes, modified: metadata.modified().unwrap(), readonly: metadata.permissions().readonly(),
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (metadata.dev(), metadata.ino(), metadata.mode(), metadata.uid(), metadata.gid(),
                    metadata.nlink(), metadata.ctime(), metadata.ctime_nsec())
            },
        }
    }

    fn children(path: &Path) -> BTreeSet<PathBuf> {
        fs::read_dir(path).unwrap().map(|entry| entry.unwrap().path()).collect()
    }

    #[test]
    fn contract_read_preserves_database_identity_bytes_and_local_metadata() {
        let (_temp, home, connection) = fixture(SCHEMA);
        let member = directory(&home, "member");
        project(&connection, "p", "Name", &[member.to_str().unwrap()]);
        drop(connection);
        let local = member.join("local-metadata.json");
        let legacy = home.join(".codex-global-state.json");
        fs::write(&local, br#"{"retained":true,"hidden":false}"#).unwrap();
        fs::write(&legacy, br#"{"synthetic-contract-canary":true}"#).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&local, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let files = [home.join(DATABASE), local, legacy];
        let before = files.iter().map(|file| snapshot(file)).collect::<Vec<_>>();
        let before_children = children(&home);
        let before_member = children(&member);
        let directory_modified = fs::metadata(&home).unwrap().modified().unwrap();
        assert_eq!(projects_at(&home).unwrap().len(), 1);
        assert_eq!(projects_at(&home).unwrap().len(), 1);
        assert_eq!(before, files.iter().map(|file| snapshot(file)).collect::<Vec<_>>());
        assert_eq!(before_children, children(&home));
        assert_eq!(before_member, children(&member));
        assert_eq!(directory_modified, fs::metadata(&home).unwrap().modified().unwrap());
    }

    #[test]
    fn contract_ipc_project_shape_rejects_unknown_fields() {
        assert!(serde_json::from_str::<CatalogProject>(
            r#"{"id":"p","name":"Name","roots":[],"metadata":{}}"#,
        ).is_err());
        assert!(serde_json::from_str::<CatalogProject>(
            r#"{"id":"p","name":"Name","roots":[]}"#,
        ).is_ok());
    }

    #[test]
    fn contract_failed_projection_preserves_database_and_local_metadata() {
        let (_temp, home, connection) = fixture(SCHEMA);
        project(&connection, "p", "Name", &["/synthetic"]);
        connection.execute_batch("UPDATE project_roots SET path = CAST(x'80' AS TEXT)").unwrap();
        drop(connection);
        let local = home.join("local-metadata.json");
        fs::write(&local, br#"{"retained":true}"#).unwrap();
        let files = [home.join(DATABASE), local];
        let before = files.iter().map(|file| snapshot(file)).collect::<Vec<_>>();
        let before_children = children(&home);
        assert!(projects_at(&home).is_err());
        assert_eq!(before, files.iter().map(|file| snapshot(file)).collect::<Vec<_>>());
        assert_eq!(before_children, children(&home));
    }

    #[test]
    fn contract_resolved_sqlite_home_selection_never_reads_global_environment() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let configured = home.join("effective-config");
        let environment = home.join("normalized-env");
        assert_eq!(sqlite_home_from_resolved(&home, Some(&configured), Some(&environment)).unwrap(), configured);
        assert_eq!(sqlite_home_from_resolved(&home, None, Some(&environment)).unwrap(), environment);
        assert_eq!(sqlite_home_from_resolved(&home, None, None).unwrap(), home);
        // Only the selected input matters; unselected overrides are not opened.
        assert_eq!(sqlite_home_from_resolved(&home, Some(&configured), Some(Path::new("relative"))).unwrap(), configured);
        assert!(sqlite_home_from_resolved(&home, None, Some(Path::new("relative"))).is_err());
        assert!(sqlite_home_from_resolved(&home, Some(Path::new("")), None).is_err());
        assert!(!configured.exists());
        assert!(!environment.exists());
    }
}
