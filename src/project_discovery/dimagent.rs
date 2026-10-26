//! DimAgent CLI 0.5.19's real SQLite session cwd projection, never messages.
//! Bound to the official darwin-arm64 payload and ACP session/new producer in
//! docs/agent-compatibility/native-dimagent-runtime.json. Desktop is not inferred.
use std::{collections::BTreeMap, path::{Component, Path, PathBuf}};
use anyhow::{Result, anyhow, ensure};
use super::{AgentKind, ProjectEntry};

pub(super) const SESSIONS_SQL: &str = "SELECT cwd FROM sessions LIMIT 10001";

pub(super) fn projects(home: &Path) -> Result<Vec<ProjectEntry>> {
    let configured = std::env::var("DIMCODE_HOME").map(Some).or_else(|error| match error {
        std::env::VarError::NotPresent => Ok(None),
        _ => Err(anyhow!("DimAgent 项目来源目录编码无效")),
    })?;
    let cwd = std::env::current_dir().map_err(|_| anyhow!("DimAgent 路径基准不可读"))?;
    let profile = storage_root(home, configured.as_deref(), &cwd)?;
    projects_at(&profile.join("dimcode.sqlite"))
}

fn storage_root(home: &Path, configured: Option<&str>, cwd: &Path) -> Result<PathBuf> {
    // Official 0.5.19 resolveCliHomeDirFromEnv normalizes the override and adds
    // v2 unless its final segment is already v2. This differs from the website's
    // bare <DIMCODE_HOME>/dimcode.sqlite example. XDG is not a CLI storage input.
    let Some(raw) = configured.map(str::trim).filter(|raw| !raw.is_empty()) else {
        ensure!(home.is_absolute(), "DimAgent home 必须为绝对路径");
        return Ok(home.join(".dimcode/v2"));
    };
    ensure!(cwd.is_absolute() && !raw.contains('\0'), "DimAgent 项目来源目录无效");
    let raw = Path::new(raw);
    let absolute = if raw.is_absolute() { raw.to_path_buf() } else { cwd.join(raw) };
    let mut normalized = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::CurDir => {},
            Component::ParentDir => { normalized.pop(); },
            _ => normalized.push(part.as_os_str()),
        }
    }
    if normalized.file_name().is_none_or(|name| name != "v2") { normalized.push("v2"); }
    Ok(normalized)
}

fn projects_at(database: &Path) -> Result<Vec<ProjectEntry>> {
    let paths = super::workspaces::sqlite_paths(database, &[SESSIONS_SQL])?;
    let mut projects = BTreeMap::new();
    for path in paths {
        if let Some(entry) = super::native::project(&path, AgentKind::DimAgent) {
            // Native status is session execution state, not a project archive
            // signal. Keep manual hide/restore; missing sources remain unknown.
            projects.entry(entry.path.clone()).or_insert(entry);
        }
    }
    Ok(projects.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::fs;

    #[test]
    fn storage_selection_matches_the_versioned_cli_producer() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        assert_eq!(storage_root(home, None, home).unwrap(), home.join(".dimcode/v2"));
        assert_eq!(storage_root(home, Some("  "), home).unwrap(), home.join(".dimcode/v2"));
        assert_eq!(storage_root(home, Some("profile"), home).unwrap(), home.join("profile/v2"));
        assert_eq!(storage_root(home, Some("profile/v2/"), home).unwrap(), home.join("profile/v2"));
        assert_eq!(storage_root(home, Some("profile/v2/../other"), home).unwrap(), home.join("profile/other/v2"));
        assert_eq!(storage_root(home, Some("~/profile"), home).unwrap(), home.join("~/profile/v2"));
    }

    fn database(root: &Path) -> (PathBuf, Connection) {
        let path = root.join("dimcode.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TABLE sessions(sessionId TEXT PRIMARY KEY,cwd TEXT NOT NULL,status TEXT,title TEXT,version INTEGER NOT NULL);CREATE TABLE messages(secret TEXT);").unwrap();
        connection.execute("INSERT INTO messages VALUES(?1)", ["not-project-metadata"]).unwrap();
        (path, connection)
    }

    #[test]
    fn reads_real_cwd_only_and_deduplicates_without_inventing_archive() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("real-project");
        fs::create_dir(&project).unwrap();
        let (path, connection) = database(temp.path());
        for (id, cwd, status) in [("one", project.clone(), "active"), ("two", project.clone(), "completed"),
            ("relative", PathBuf::from("relative"), "active"), ("gone", temp.path().join("missing"), "active")] {
            connection.execute("INSERT INTO sessions VALUES(?1,?2,?3,'not-a-project',2)",
                rusqlite::params![id, cwd.to_string_lossy(), status]).unwrap();
        }
        drop(connection);
        let before = fs::read(&path).unwrap();
        let found = projects_at(&path).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, project.canonicalize().unwrap());
        assert_eq!(found[0].agents, [AgentKind::DimAgent]);
        assert!(!found[0].archived && found[0].archive_states.is_empty());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(super::super::workspaces::sqlite_paths(&path, &["SELECT secret FROM messages"]).is_err());
    }

    #[test]
    fn missing_invalid_and_empty_sources_are_distinct() {
        let temp = tempfile::tempdir().unwrap();
        let (path, connection) = database(temp.path());
        drop(connection);
        assert!(projects_at(&path).unwrap().is_empty());
        let missing = temp.path().join("missing.sqlite");
        assert!(projects_at(&missing).is_err());
        assert!(!missing.exists());
        fs::write(&path, "not-a-database-not-to-echo").unwrap();
        let error = projects_at(&path).unwrap_err().to_string();
        assert!(!error.contains("not-to-echo"));
    }

    #[cfg(unix)]
    #[test]
    fn directory_aliases_merge_and_nontext_cwd_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir(&project).unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&project, &alias).unwrap();
        let (path, connection) = database(temp.path());
        for (id, cwd) in [("one", &project), ("alias", &alias)] {
            connection.execute("INSERT INTO sessions VALUES(?1,?2,'active',NULL,2)",
                rusqlite::params![id, cwd.to_string_lossy()]).unwrap();
        }
        assert_eq!(projects_at(&path).unwrap().len(), 1);
        connection.execute_batch("INSERT INTO sessions VALUES('blob',X'2F746D70','active',NULL,2)").unwrap();
        assert!(projects_at(&path).is_err());
    }
}
