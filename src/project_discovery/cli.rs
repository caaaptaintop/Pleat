//! Bounded, metadata-only project catalogs for Pi, Oh My Pi, and OpenCode.
//! Session transcripts are never inspected past the native session header.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow, bail};
use serde_json::Value;

use crate::{agents::omp_agent_dir, effective_skills::AgentKind};

use super::{ProjectEntry, native::project, path_identity, workspaces::opencode_project_rows};

const MAX_FILES: usize = 10_000;
const MAX_TOTAL_HEADER_BYTES: usize = 64 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 64 * 1024;

pub(super) const OPENCODE_PROJECTS_SQL: &str = "SELECT worktree FROM project LIMIT 10001";

#[derive(Clone, Copy)]
struct Limits {
    files: usize,
    entries: usize,
    total_bytes: usize,
    line_bytes: usize,
}

const LIMITS: Limits = Limits {
    files: MAX_FILES,
    entries: MAX_FILES,
    total_bytes: MAX_TOTAL_HEADER_BYTES,
    line_bytes: MAX_LINE_BYTES,
};

/// Returns independent source results so the caller can preserve the previous
/// entries for an unavailable or malformed catalog without discarding others.
pub(super) fn catalogs(home: &Path) -> Vec<(AgentKind, Result<Vec<ProjectEntry>>)> {
    vec![
        (AgentKind::Pi, pi_catalog(home)),
        (AgentKind::Omp, omp_catalog(home)),
        (AgentKind::OpenCode, opencode_catalog(home)),
    ]
}

fn pi_catalog(home: &Path) -> Result<Vec<ProjectEntry>> {
    let agent_dir =
        configured_agent_dir(home, "PI_CODING_AGENT_DIR").unwrap_or_else(|| home.join(".pi/agent"));
    read_session_catalog(&agent_dir.join("sessions"), AgentKind::Pi, false, LIMITS)
}

fn omp_catalog(home: &Path) -> Result<Vec<ProjectEntry>> {
    read_session_catalog(
        &omp_agent_dir(home).join("sessions"),
        AgentKind::Omp,
        true,
        LIMITS,
    )
}

/// Mirrors Pi's documented PI_CODING_AGENT_DIR expansion: `~/...` is relative
/// to the supplied home; other values are used as paths (including relative
/// values), matching the existing installation-root resolver.
fn configured_agent_dir(home: &Path, key: &str) -> Option<PathBuf> {
    let value = std::env::var_os(key)?;
    let value = value.to_string_lossy();
    Some(
        value
            .strip_prefix("~/")
            .map_or_else(|| PathBuf::from(value.as_ref()), |suffix| home.join(suffix)),
    )
}

fn opencode_catalog(home: &Path) -> Result<Vec<ProjectEntry>> {
    let db = opencode_db_path(home)?;
    opencode_projects_at(&db)
}

fn opencode_projects_at(db: &Path) -> Result<Vec<ProjectEntry>> {
    let projects = opencode_project_rows(db)?;
    // An empty project table is authoritative. Only a real global-root project
    // unlocks its associated session-directory metadata in the same reader.
    let mut entries: BTreeMap<PathBuf, ProjectEntry> = BTreeMap::new();
    for row in projects {
        // Legacy ID.global + / is a shared bucket, not a session's cwd.
        // The reader returns its actual session directories separately, unknown
        // archival unless another native project explicitly marks that path.
        if row.is_global_root { continue; }
        let Some(mut entry) = project(&row.path, AgentKind::OpenCode) else { continue; };
        if let Some(archived) = row.archived {
            entry.record_archive(AgentKind::OpenCode, archived);
        }
        entries.entry(path_identity(&entry.path))
            .and_modify(|old| old.merge_archival(&entry))
            .or_insert(entry);
    }
    Ok(entries.into_values().collect())
}

fn opencode_db_path(home: &Path) -> Result<PathBuf> {
    if let Some(value) = std::env::var_os("OPENCODE_DB") {
        if value.is_empty() {
            bail!("OpenCode 项目索引路径无效");
        }
        let path = PathBuf::from(value);
        return Ok(if path.is_absolute() {
            path
        } else {
            opencode_data_dir(home)?.join(path)
        });
    }
    Ok(opencode_data_dir(home)?.join("opencode.db"))
}

fn opencode_data_dir(home: &Path) -> Result<PathBuf> {
    if let Some(value) = std::env::var_os("XDG_DATA_HOME") {
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            bail!("OpenCode 项目索引路径无效");
        }
        return Ok(path.join("opencode"));
    }
    Ok(home.join(".local/share/opencode"))
}

fn read_session_catalog(
    root: &Path,
    agent: AgentKind,
    omp: bool,
    limits: Limits,
) -> Result<Vec<ProjectEntry>> {
    let root_entries = fs::read_dir(root).map_err(|_| anyhow!("Agent 会话索引暂不可读"))?;
    let mut files_seen = 0usize;
    let mut entries_seen = 0usize;
    let mut bytes_seen = 0usize;
    let mut projects = BTreeSet::new();

    for bucket in root_entries {
        count_directory_entry(&mut entries_seen, limits.entries)?;
        let bucket = bucket.map_err(|_| anyhow!("Agent 会话索引暂不可读"))?;
        let bucket_type = bucket
            .file_type()
            .map_err(|_| anyhow!("Agent 会话索引暂不可读"))?;
        if !bucket_type.is_dir() {
            continue;
        }
        let sessions =
            fs::read_dir(bucket.path()).map_err(|_| anyhow!("Agent 会话索引暂不可读"))?;
        for session in sessions {
            count_directory_entry(&mut entries_seen, limits.entries)?;
            let session = session.map_err(|_| anyhow!("Agent 会话索引暂不可读"))?;
            let file_type = session
                .file_type()
                .map_err(|_| anyhow!("Agent 会话索引暂不可读"))?;
            if !file_type.is_file()
                || session.path().extension().and_then(|value| value.to_str()) != Some("jsonl")
            {
                continue;
            }
            files_seen = files_seen
                .checked_add(1)
                .ok_or_else(|| anyhow!("Agent 会话索引超过读取上限"))?;
            if files_seen > limits.files {
                bail!("Agent 会话索引超过读取上限");
            }
            let mut file =
                File::open(session.path()).map_err(|_| anyhow!("Agent 会话索引暂不可读"))?;
            let cwd = read_session_header(
                &mut file,
                omp,
                limits.line_bytes,
                &mut bytes_seen,
                limits.total_bytes,
            )?;
            let path = Path::new(&cwd);
            if !path.is_absolute() {
                bail!("Agent 会话索引头格式无效");
            }
            if let Some(entry) = project(&cwd, agent) {
                projects.insert(path_identity(&entry.path));
            }
        }
    }

    Ok(projects
        .into_iter()
        .filter_map(|path| project(path.to_string_lossy().as_ref(), agent))
        .collect())
}

fn count_directory_entry(seen: &mut usize, limit: usize) -> Result<()> {
    *seen = seen
        .checked_add(1)
        .ok_or_else(|| anyhow!("Agent 会话索引超过读取上限"))?;
    if *seen > limit {
        bail!("Agent 会话索引超过读取上限");
    }
    Ok(())
}

fn read_session_header<R: Read>(
    reader: &mut R,
    omp: bool,
    max_line: usize,
    bytes_seen: &mut usize,
    max_total: usize,
) -> Result<String> {
    let first =
        read_line_bounded(reader, max_line)?.ok_or_else(|| anyhow!("Agent 会话索引头缺失"))?;
    count_header_bytes(bytes_seen, first.len(), max_total)?;
    let first_value: Value = serde_json::from_slice(trim_line(&first))
        .map_err(|_| anyhow!("Agent 会话索引头格式无效"))?;

    let header = if omp {
        // OMP's native fixed slot is 256 UTF-8 bytes INCLUDING its newline.
        if first.len() != 256 || first.last() != Some(&b'\n')
            || first_value.get("type").and_then(Value::as_str) != Some("title")
            || first_value.get("v").and_then(Value::as_u64) != Some(1)
            || first_value.get("pad").and_then(Value::as_str).is_none()
        {
            bail!("Agent 会话索引头格式无效");
        }
        let second =
            read_line_bounded(reader, max_line)?.ok_or_else(|| anyhow!("Agent 会话索引头缺失"))?;
        count_header_bytes(bytes_seen, second.len(), max_total)?;
        serde_json::from_slice::<Value>(trim_line(&second))
            .map_err(|_| anyhow!("Agent 会话索引头格式无效"))?
    } else {
        first_value
    };

    if header.get("type").and_then(Value::as_str) != Some("session") {
        bail!("Agent 会话索引头格式无效");
    }
    if header.get("id").and_then(Value::as_str).is_none() {
        bail!("Agent 会话索引头格式无效");
    }
    header
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("Agent 会话索引头格式无效"))
}

fn read_line_bounded<R: Read>(reader: &mut R, max_line: usize) -> Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let read = reader
            .read(&mut byte)
            .map_err(|_| anyhow!("Agent 会话索引暂不可读"))?;
        if read == 0 {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(line))
            };
        }
        line.push(byte[0]);
        if line.len() > max_line {
            bail!("Agent 会话索引超过读取上限");
        }
        if byte[0] == b'\n' {
            return Ok(Some(line));
        }
    }
}

fn trim_line(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn count_header_bytes(seen: &mut usize, count: usize, max: usize) -> Result<()> {
    *seen = seen
        .checked_add(count)
        .ok_or_else(|| anyhow!("Agent 会话索引超过读取上限"))?;
    if *seen > max {
        bail!("Agent 会话索引超过读取上限");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, Cursor};

    fn entry(cwd: &Path, omp: bool, suffix: &str) -> String {
        let header = serde_json::json!({"type":"session","version":3,"id":suffix,
            "timestamp":"2026-01-01T00:00:00Z","cwd":cwd});
        if omp {
            let prefix = "{\"type\":\"title\",\"v\":1,\"title\":\"t\",\"pad\":\"";
            let suffix_text = "\"}";
            let pad = " ".repeat(255 - prefix.len() - suffix_text.len());
            format!(
                "{prefix}{pad}{suffix_text}\n{header}\n{{\"type\":\"message\",\"message\":\"CHAT_SENTINEL\"}}\n"
            )
        } else {
            format!(
                "{}\n{{\"type\":\"message\",\"message\":\"CHAT_SENTINEL\"}}\n",
                header
            )
        }
    }

    fn write_session(root: &Path, bucket: &str, name: &str, contents: &str) {
        let dir = root.join(bucket);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn reads_native_pi_and_omp_headers_and_coalesces_sessions_by_directory() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let cwd = root.join("project");
        fs::create_dir(&cwd).unwrap();
        let pi = root.join("pi-sessions");
        write_session(&pi, "encoded-cwd", "one.jsonl", &entry(&cwd, false, "1"));
        write_session(
            &pi,
            "other-encoded-cwd",
            "two.jsonl",
            &entry(&cwd, false, "2"),
        );
        let found = read_session_catalog(&pi, AgentKind::Pi, false, LIMITS).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, cwd);

        let omp = root.join("omp-sessions");
        write_session(&omp, "encoded-cwd", "one.jsonl", &entry(&cwd, true, "3"));
        let found = read_session_catalog(&omp, AgentKind::Omp, true, LIMITS).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, cwd);
    }

    #[test]
    fn header_reader_stops_before_chat_sentinel() {
        let cwd = "/project";
        let session = serde_json::json!({"type":"session","id":"test","cwd":cwd});
        let mut bytes =
            Cursor::new(format!("{session}\nCHAT_SENTINEL_MUST_NOT_BE_READ\n").into_bytes());
        let mut total = 0;
        assert_eq!(
            read_session_header(
                &mut bytes,
                false,
                MAX_LINE_BYTES,
                &mut total,
                MAX_TOTAL_HEADER_BYTES
            )
            .unwrap(),
            cwd
        );
        assert!(bytes.fill_buf().unwrap().starts_with(b"CHAT_SENTINEL"));

        let title_prefix = "{\"type\":\"title\",\"v\":1,\"title\":\"\",\"pad\":\"";
        let title_suffix = "\"}";
        let title = format!(
            "{title_prefix}{}{title_suffix}",
            " ".repeat(255 - title_prefix.len() - title_suffix.len())
        );
        let mut bytes = Cursor::new(format!("{title}\n{session}\nCHAT_SENTINEL\n").into_bytes());
        let mut total = 0;
        assert_eq!(
            read_session_header(
                &mut bytes,
                true,
                MAX_LINE_BYTES,
                &mut total,
                MAX_TOTAL_HEADER_BYTES
            )
            .unwrap(),
            cwd
        );
        assert!(bytes.fill_buf().unwrap().starts_with(b"CHAT_SENTINEL"));
    }

    #[test]
    fn omp_native_slot_counts_utf8_bytes_with_its_newline_and_stops_at_header() {
        let cwd = "/private/tmp/omp-native-metadata-only";
        let mut title = serde_json::json!({"type":"title","v":1,"title":"中文 🧪",
            "updatedAt":"2026-10-19T12:25:15.045Z","pad":""});
        let width = serde_json::to_vec(&title).unwrap().len();
        title["pad"] = " ".repeat(255 - width).into();
        let line = format!("{title}\n");
        assert_eq!(line.len(), 256);
        let header = serde_json::json!({"type":"session","version":3,"id":"owned-native-slot","cwd":cwd});
        for slot in [line.clone(), format!("{} \n", line.trim_end_matches('\n'))] {
            let expected_valid = slot.len() == 256;
            let mut input = Cursor::new(format!("{slot}{header}\nCHAT_SENTINEL\n").into_bytes());
            let mut consumed = 0;
            let result = read_session_header(&mut input, true, MAX_LINE_BYTES, &mut consumed, MAX_TOTAL_HEADER_BYTES);
            assert_eq!(result.is_ok(), expected_valid);
            if expected_valid {
                assert_eq!(result.unwrap(), cwd);
                assert!(input.fill_buf().unwrap().starts_with(b"CHAT_SENTINEL"));
            }
        }
    }

    #[test]
    fn malformed_headers_and_line_or_file_limits_fail_closed() {
        let mut total = 0;
        let mut malformed = Cursor::new(b"{not-json}\nsecret chat\n".to_vec());
        let error = read_session_header(
            &mut malformed,
            false,
            MAX_LINE_BYTES,
            &mut total,
            MAX_TOTAL_HEADER_BYTES,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("格式无效"));
        assert!(!error.contains("secret"));

        let mut long_line = Cursor::new(vec![b'x'; MAX_LINE_BYTES + 2]);
        assert!(read_line_bounded(&mut long_line, MAX_LINE_BYTES).is_err());

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let cwd = root.join("project");
        fs::create_dir(&cwd).unwrap();
        let sessions = root.join("sessions");
        write_session(&sessions, "bucket", "one.jsonl", &entry(&cwd, false, "1"));
        let limits = Limits {
            files: 0,
            entries: MAX_FILES,
            total_bytes: MAX_TOTAL_HEADER_BYTES,
            line_bytes: MAX_LINE_BYTES,
        };
        assert!(
            read_session_catalog(&sessions, AgentKind::Pi, false, limits)
                .unwrap_err()
                .to_string()
                .contains("读取上限")
        );
        let limits = Limits {
            files: MAX_FILES,
            entries: MAX_FILES,
            total_bytes: 0,
            line_bytes: MAX_LINE_BYTES,
        };
        assert!(
            read_session_catalog(&sessions, AgentKind::Pi, false, limits)
                .unwrap_err()
                .to_string()
                .contains("读取上限")
        );
        let empty_buckets = root.join("empty-buckets");
        fs::create_dir_all(empty_buckets.join("a")).unwrap();
        fs::create_dir_all(empty_buckets.join("b")).unwrap();
        let limits = Limits { files: MAX_FILES, entries: 1, total_bytes: MAX_TOTAL_HEADER_BYTES, line_bytes: MAX_LINE_BYTES };
        assert!(read_session_catalog(&empty_buckets, AgentKind::Pi, false, limits)
            .unwrap_err().to_string().contains("读取上限"));

        let non_json = root.join("non-json");
        write_session(&non_json, "bucket", "one.txt", "ignored");
        write_session(&non_json, "bucket", "two.bin", "ignored");
        let limits = Limits { files: MAX_FILES, entries: 2, total_bytes: MAX_TOTAL_HEADER_BYTES, line_bytes: MAX_LINE_BYTES };
        assert!(read_session_catalog(&non_json, AgentKind::Pi, false, limits)
            .unwrap_err().to_string().contains("读取上限"));
        assert!(
            read_session_catalog(&root.join("missing"), AgentKind::Pi, false, LIMITS)
                .unwrap_err()
                .to_string()
                .contains("暂不可读")
        );
    }

    #[test]
    fn opencode_native_archive_states_merge_without_losing_active_projects() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for name in ["active", "archived", "zero", "shared"] {
            fs::create_dir(root.join(name)).unwrap();
        }
        let db = root.join("opencode.db");
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection.execute_batch("CREATE TABLE project(id TEXT, worktree TEXT NOT NULL, time_archived INTEGER);").unwrap();
        for (id, name, stamp) in [
            ("active", "active", None),
            ("archived", "archived", Some(123_i64)),
            ("zero", "zero", Some(0)),
            ("shared-old", "shared", Some(456)),
            ("shared-new", "shared/.", None),
        ] {
            connection.execute("INSERT INTO project VALUES(?1, ?2, ?3)",
                rusqlite::params![id, root.join(name).to_str().unwrap(), stamp]).unwrap();
        }
        drop(connection);
        let before = fs::read(&db).unwrap();
        let projects = opencode_projects_at(&db).unwrap();
        assert_eq!(projects.len(), 4);
        for (name, archived) in [("active", false), ("archived", true), ("zero", true), ("shared", false)] {
            let entry = projects.iter().find(|entry| entry.path == root.join(name)).unwrap();
            assert_eq!(entry.archived, archived);
            assert_eq!(entry.archive_states.len(), 1);
            assert_eq!(entry.archive_states[0].agent, AgentKind::OpenCode);
            assert_eq!(entry.archive_states[0].archived, Some(archived));
        }
        assert_eq!(fs::read(&db).unwrap(), before);
    }

    #[test]
    fn opencode_legacy_missing_archive_column_stays_unknown() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let db = root.join("opencode.db");
        let connection = rusqlite::Connection::open(&db).unwrap();
        // The legacy worktree-only contract does not require id or archive.
        connection.execute_batch("CREATE TABLE project(worktree TEXT NOT NULL);").unwrap();
        connection.execute("INSERT INTO project VALUES(?1)", [root.to_str().unwrap()]).unwrap();
        drop(connection);
        let rows = opencode_project_rows(&db).unwrap();
        assert_eq!(rows[0].archived, None);
        let projects = opencode_projects_at(&db).unwrap();
        assert_eq!(projects.len(), 1);
        assert!(!projects[0].archived);
        assert!(projects[0].archive_states.iter().all(|state| state.archived.is_none()));
        assert_eq!(super::super::workspaces::sqlite_paths(&db, &[OPENCODE_PROJECTS_SQL]).unwrap(),
            [root.to_str().unwrap()]);
    }

    #[test]
    fn opencode_metadata_only_catalog_never_falls_back_to_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("opencode.db");
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection.execute_batch(
            "CREATE TABLE project(worktree TEXT NOT NULL, time_archived INTEGER);
             CREATE VIEW session_v2 AS SELECT unavailable_session_reader() AS directory;"
        ).unwrap();
        connection.execute("INSERT INTO project VALUES(?1, NULL)", [temp.path().to_str().unwrap()]).unwrap();
        drop(connection);
        assert_eq!(opencode_projects_at(&db).unwrap().len(), 1);
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection.execute("DELETE FROM project", []).unwrap();
        drop(connection);
        assert!(opencode_projects_at(&db).unwrap().is_empty());
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection.execute_batch("DROP TABLE project;").unwrap();
        drop(connection);
        assert!(opencode_projects_at(&db).is_err());
    }

    #[test]
    fn opencode_global_root_bucket_is_not_a_session_cwd_or_a_blanket_root_filter() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let db = root.join("opencode.db");
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection.execute_batch(
            "CREATE TABLE project(id TEXT, worktree TEXT, time_archived INTEGER);
             INSERT INTO project VALUES('global', '/', NULL);
             CREATE TABLE session_v2(project_id TEXT, directory TEXT);"
        ).unwrap();
        connection.execute("INSERT INTO session_v2 VALUES('global', ?1)", [root.to_str().unwrap()]).unwrap();
        drop(connection);
        // Recover the recorded cwd with unknown archival; / remains a bucket.
        let projects = opencode_projects_at(&db).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, root);
        assert!(projects[0].archive_states.iter().all(|state| state.archived.is_none()));
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection.execute("INSERT INTO project VALUES('real-root', '/', NULL)", []).unwrap();
        connection.execute("INSERT INTO project VALUES('real-project', ?1, NULL)", [root.to_str().unwrap()]).unwrap();
        drop(connection);
        let projects = opencode_projects_at(&db).unwrap();
        assert_eq!(projects.len(), 2);
        assert!(projects.iter().any(|entry| entry.path == path_identity(Path::new("/"))));
        assert!(projects.iter().any(|entry| entry.path == root));
    }

    #[test]
    fn opencode_archive_schema_and_values_fail_closed_without_leaking() {
        for schema in [
            "CREATE VIEW project AS SELECT '/secret-path' AS worktree, 0 AS time_archived",
            "CREATE TABLE project(worktree TEXT, time_archived TEXT)",
            "CREATE TABLE project(worktree TEXT, time_archived INTEGER GENERATED ALWAYS AS (0) VIRTUAL)",
            "CREATE TABLE project(worktree TEXT, TIME_ARCHIVED TEXT)",
            "CREATE TABLE project(worktree TEXT, id BLOB, time_archived INTEGER)",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let db = temp.path().join("private.db");
            let connection = rusqlite::Connection::open(&db).unwrap();
            connection.execute_batch(schema).unwrap();
            drop(connection);
            let error = opencode_projects_at(&db).unwrap_err().to_string();
            assert!(error.contains("结构无效"));
            assert!(!error.contains("secret") && !error.contains(temp.path().to_str().unwrap()));
        }
        for value in ["'secret-archive'", "-1", "1.5", "x'ffff'"] {
            let temp = tempfile::tempdir().unwrap();
            let db = temp.path().join("private.db");
            let connection = rusqlite::Connection::open(&db).unwrap();
            connection.execute_batch(&format!(
                "CREATE TABLE project(worktree TEXT, time_archived INTEGER);
                 INSERT INTO project VALUES('/secret-path', {value});"
            )).unwrap();
            drop(connection);
            let error = opencode_projects_at(&db).unwrap_err().to_string();
            assert!(error.contains("归档格式无效"));
            assert!(!error.contains("secret") && !error.contains(temp.path().to_str().unwrap()));
        }
        for id in ["NULL", "x'ffff'", "CAST(x'ff' AS TEXT)"] {
            let temp = tempfile::tempdir().unwrap();
            let db = temp.path().join("private.db");
            let connection = rusqlite::Connection::open(&db).unwrap();
            connection.execute_batch(&format!(
                "CREATE TABLE project(id TEXT, worktree TEXT, time_archived INTEGER);
                 INSERT INTO project VALUES({id}, '/secret-path', NULL);"
            )).unwrap();
            drop(connection);
            let error = opencode_projects_at(&db).unwrap_err().to_string();
            assert!(error.contains("标识格式无效"));
            assert!(!error.contains("secret") && !error.contains(temp.path().to_str().unwrap()));
        }
    }

    #[test]
    fn opencode_typed_projection_keeps_row_and_borrowed_path_limits() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("opencode.db");
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection.execute_batch(
            "CREATE TABLE project(worktree TEXT, time_archived INTEGER);
             WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000)
             INSERT INTO project SELECT '/' || x, NULL FROM n;"
        ).unwrap();
        drop(connection);
        assert_eq!(opencode_project_rows(&db).unwrap().len(), MAX_FILES);
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection.execute("INSERT INTO project VALUES('/overflow', 0)", []).unwrap();
        drop(connection);
        assert!(opencode_projects_at(&db).unwrap_err().to_string().contains("读取上限"));
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection.execute_batch("DELETE FROM project;").unwrap();
        connection.execute("INSERT INTO project VALUES(?1, NULL)", ["x".repeat(16 * 1024 + 1)]).unwrap();
        drop(connection);
        assert!(opencode_projects_at(&db).unwrap_err().to_string().contains("路径超过"));
    }

    #[test]
    fn opencode_global_native_v1_v2_and_coexisting_indexes_recover_non_git_cwds() {
        for tables in [&["session"][..], &["session_v2"][..], &["session", "session_v2"][..]] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let db = native_global_fixture(&root, tables);
            let connection = rusqlite::Connection::open(&db).unwrap();
            // Keep native foreign keys valid for the unrelated-session controls.
            // These parents name absent directories and add no discovered cwd.
            connection.execute_batch("INSERT INTO project(id,worktree,time_created,time_updated,sandboxes) VALUES('other-project','/missing-pleat-other-project',1,1,'[]'),('GLOBAL','/missing-pleat-uppercase-project',1,1,'[]');").unwrap();
            for (i, table) in tables.iter().enumerate() {
                let cwd = root.join(format!("non-git-{i}"));
                fs::create_dir(&cwd).unwrap();
                assert!(!cwd.join(".git").exists());
                connection.execute(&format!(
                    "INSERT INTO {table}(id,project_id,slug,directory,title,version,time_created,time_updated,time_archived)
                     VALUES(?1,'global','fixture',?2,'fixture','fixture',1,1,123)"
                ), rusqlite::params![format!("s{i}"), cwd.to_str().unwrap()]).unwrap();
                // Unrelated malformed directory must never be projected/parsed.
                connection.execute(&format!(
                    "INSERT INTO {table}(id,project_id,slug,directory,title,version,time_created,time_updated)
                     VALUES('other','other-project','fixture',x'ffff','fixture','fixture',1,1)"
                ), []).unwrap();
                connection.execute(&format!(
                    "INSERT INTO {table}(id,project_id,slug,directory,title,version,time_created,time_updated)
                     VALUES('uppercase','GLOBAL','fixture',x'ffff','fixture','fixture',1,1)"
                ), []).unwrap();
            }
            drop(connection);
            let before = fs::read(&db).unwrap();
            let projects = opencode_projects_at(&db).unwrap();
            assert_eq!(projects.len(), tables.len());
            for (i, entry) in projects.iter().enumerate() {
                assert_eq!(entry.path, root.join(format!("non-git-{i}")));
                assert!(!entry.archived);
                assert!(entry.archive_states.iter().all(|state| state.archived.is_none()));
            }
            assert_eq!(fs::read(&db).unwrap(), before);
        }
    }

    // Native table shapes from v1.19.3 and v2.0.32; only metadata columns are
    // populated. Neither session archive nor title is selected by discovery.
    fn native_global_fixture(root: &Path, tables: &[&str]) -> PathBuf {
        let db = root.join("opencode.db");
        let connection = rusqlite::Connection::open(&db).unwrap();
        let v2 = tables.contains(&"session_v2");
        connection.execute_batch(&format!(
            "CREATE TABLE project(
               id TEXT PRIMARY KEY, worktree TEXT NOT NULL, vcs TEXT, name TEXT,
               icon_url TEXT, icon_url_override TEXT, icon_color TEXT,
               time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
               time_initialized INTEGER, sandboxes TEXT NOT NULL, commands TEXT {}
             );
             INSERT INTO project(id,worktree,time_created,time_updated,sandboxes)
               VALUES('global','/',1,1,'[]');",
            if v2 { ", time_active INTEGER NOT NULL DEFAULT 0, time_archived INTEGER" } else { "" }
        )).unwrap();
        if v2 {
            connection.execute("UPDATE project SET time_archived=456", []).unwrap();
        }
        for table in tables {
            assert!(["session", "session_v2"].contains(table));
            connection.execute_batch(&format!(
                "CREATE TABLE {table}(
                   id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES project(id),
                   workspace_id TEXT, parent_id TEXT, slug TEXT NOT NULL,
                   directory TEXT NOT NULL, path TEXT, title TEXT {}, version TEXT NOT NULL,
                   share_url TEXT, summary_additions INTEGER, summary_deletions INTEGER,
                   summary_files INTEGER, summary_diffs TEXT, metadata TEXT,
                   cost REAL NOT NULL DEFAULT 0,
                   tokens_input INTEGER NOT NULL DEFAULT 0, tokens_output INTEGER NOT NULL DEFAULT 0,
                   tokens_reasoning INTEGER NOT NULL DEFAULT 0, tokens_cache_read INTEGER NOT NULL DEFAULT 0,
                   tokens_cache_write INTEGER NOT NULL DEFAULT 0, revert TEXT, permission TEXT,
                   agent TEXT, model TEXT, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
                   time_compacting INTEGER, time_archived INTEGER {}
                 );
                 CREATE INDEX {table}_project_idx ON {table}(project_id);",
                if *table == "session" { "NOT NULL" } else { "" },
                if *table == "session_v2" { ", fork_session_id TEXT, fork_boundary TEXT, time_idle INTEGER, time_viewed INTEGER, idle_outcome TEXT, time_suspended INTEGER, resume_attempts INTEGER NOT NULL DEFAULT 0" } else { "" }
            )).unwrap();
        }
        drop(connection);
        db
    }

    #[test]
    fn opencode_global_gate_never_resurrects_stale_or_unrelated_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let db = native_global_fixture(&root, &["session", "session_v2"]);
        let connection = rusqlite::Connection::open(&db).unwrap();
        for table in ["session", "session_v2"] {
            connection.execute(&format!(
                "INSERT INTO {table}(id,project_id,slug,directory,title,version,time_created,time_updated)
                 VALUES('stale','global','fixture',?1,'fixture','fixture',1,1)"
            ), [root.to_str().unwrap()]).unwrap();
        }
        // Deliberately synthesize stale session rows left behind an empty
        // authoritative index. This is confined to this fixture writer.
        connection.pragma_update(None, "foreign_keys", false).unwrap();
        connection.execute("DELETE FROM project", []).unwrap();
        drop(connection);
        assert!(opencode_projects_at(&db).unwrap().is_empty());
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection.execute("INSERT INTO project(id,worktree,time_created,time_updated,sandboxes) VALUES('real-root','/',1,1,'[]')", []).unwrap();
        // A bad session index is irrelevant until the exact native gate opens.
        connection.execute_batch("DROP TABLE session_v2; CREATE VIEW session_v2 AS SELECT unavailable_session_reader() AS directory;").unwrap();
        drop(connection);
        let projects = opencode_projects_at(&db).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, path_identity(Path::new("/")));
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection.execute("UPDATE project SET id='global',worktree=?1", [root.to_str().unwrap()]).unwrap();
        drop(connection);
        assert_eq!(opencode_projects_at(&db).unwrap()[0].path, root);
    }

    #[test]
    fn opencode_global_directory_dedup_uses_only_explicit_project_archive_votes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let db = native_global_fixture(&root, &["session", "session_v2"]);
        let connection = rusqlite::Connection::open(&db).unwrap();
        for (i, table) in ["session", "session_v2"].iter().enumerate() {
            connection.execute(&format!(
                "INSERT INTO {table}(id,project_id,slug,directory,title,version,time_created,time_updated,time_archived)
                 VALUES('same','global','fixture',?1,'fixture','fixture',1,1,999)"
            ), [root.join(if i==0 { "." } else { "" }).to_str().unwrap()]).unwrap();
        }
        drop(connection);
        let projects = opencode_projects_at(&db).unwrap();
        assert_eq!(projects.len(), 1);
        assert!(projects[0].archive_states.iter().all(|state| state.archived.is_none()));
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection.execute("INSERT INTO project(id,worktree,time_created,time_updated,sandboxes,time_archived) VALUES('explicit',?1,1,1,'[]',0)", [root.to_str().unwrap()]).unwrap();
        drop(connection);
        let projects = opencode_projects_at(&db).unwrap();
        assert_eq!(projects.len(), 1);
        assert!(projects[0].archived);
        assert_eq!(projects[0].archive_states[0].archived, Some(true));
    }

    #[test]
    fn opencode_global_missing_corrupt_or_untrusted_indexes_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing.db");
        assert!(opencode_projects_at(&missing).is_err());
        assert!(!missing.exists());
        fs::write(&missing, "secret-corrupt-db").unwrap();
        let error = opencode_projects_at(&missing).unwrap_err().to_string();
        assert!(!error.contains("secret") && !error.contains(temp.path().to_str().unwrap()));
        for schema in [
            "",
            "CREATE TABLE session(project_id TEXT, directory BLOB)",
            "CREATE TABLE session(project_id BLOB, directory TEXT)",
            "CREATE TABLE session(project_id TEXT, directory TEXT GENERATED ALWAYS AS ('/secret') VIRTUAL)",
            "CREATE VIEW session AS SELECT 'global' AS project_id, '/secret' AS directory",
            // A malformed present v1 index must not silently fall back to v2.
            "CREATE VIEW session AS SELECT 'global' AS project_id, '/secret' AS directory; CREATE TABLE session_v2(project_id TEXT, directory TEXT)",
        ] {
            let fixture = tempfile::tempdir().unwrap();
            let db = native_global_fixture(fixture.path(), &[]);
            let connection = rusqlite::Connection::open(&db).unwrap();
            connection.execute_batch(schema).unwrap();
            drop(connection);
            let error = opencode_projects_at(&db).unwrap_err().to_string();
            assert!(error.contains("结构无效"));
            assert!(!error.contains("secret") && !error.contains(fixture.path().to_str().unwrap()));
        }
    }

    #[test]
    fn opencode_global_indexes_share_row_and_path_limits_with_projects() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let db = native_global_fixture(&root, &["session", "session_v2"]);
        let connection = rusqlite::Connection::open(&db).unwrap();
        // Legacy empty directory is readable but must still spend row budget.
        connection.execute_batch(
            "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<9999)
             INSERT INTO session(id,project_id,slug,directory,title,version,time_created,time_updated)
               SELECT 's'||x,'global','fixture','','fixture','fixture',1,1 FROM n;"
        ).unwrap();
        drop(connection);
        assert!(opencode_projects_at(&db).unwrap().is_empty());
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection.execute_batch("INSERT INTO session_v2(id,project_id,slug,directory,title,version,time_created,time_updated) VALUES('overflow','global','fixture','','fixture','fixture',1,1)").unwrap();
        drop(connection);
        assert!(opencode_projects_at(&db).unwrap_err().to_string().contains("读取上限"));
        for value in [rusqlite::types::Value::Text("x".repeat(16 * 1024 + 1)), rusqlite::types::Value::Blob(vec![255])] {
            let connection = rusqlite::Connection::open(&db).unwrap();
            connection.execute_batch("DELETE FROM session; DELETE FROM session_v2").unwrap();
            connection.execute("INSERT INTO session_v2(id,project_id,slug,directory,title,version,time_created,time_updated) VALUES('invalid','global','fixture',?1,'fixture','fixture',1,1)", [value]).unwrap();
            drop(connection);
            let error = opencode_projects_at(&db).unwrap_err().to_string();
            assert!(error.contains("路径"));
            assert!(!error.contains(root.to_str().unwrap()));
        }
    }

    #[test]
    fn opencode_project_query_matches_current_sqlite_schema() {
        assert_eq!(
            OPENCODE_PROJECTS_SQL,
            "SELECT worktree FROM project LIMIT 10001"
        );
        assert!(
            !OPENCODE_PROJECTS_SQL
                .to_ascii_lowercase()
                .contains("message")
        );

        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE project(
               id TEXT PRIMARY KEY, worktree TEXT NOT NULL, vcs TEXT, name TEXT,
               icon_url TEXT, icon_url_override TEXT, icon_color TEXT,
               time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
               time_initialized INTEGER, time_active INTEGER NOT NULL DEFAULT 0,
               time_archived INTEGER, sandboxes TEXT NOT NULL, commands TEXT
             );
             CREATE TABLE session_v2(
               id TEXT PRIMARY KEY, project_id TEXT NOT NULL, workspace_id TEXT,
               parent_id TEXT, fork_session_id TEXT, fork_boundary TEXT, slug TEXT NOT NULL,
               directory TEXT NOT NULL, path TEXT, title TEXT, version TEXT NOT NULL,
               share_url TEXT, summary_additions INTEGER, summary_deletions INTEGER,
               summary_files INTEGER, summary_diffs TEXT, metadata TEXT, cost REAL NOT NULL DEFAULT 0
             );
             INSERT INTO project(id,worktree,time_created,time_updated,sandboxes)
               VALUES('project-1','/project-one',1,1,'[]');
             INSERT INTO session_v2(id,project_id,slug,directory,version,cost)
               VALUES('session-1','project-2','slug','/project-two','v2',0);",
        ).unwrap();
        let registered = db
            .prepare(OPENCODE_PROJECTS_SQL)
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(registered, ["/project-one"]);
    }
}
