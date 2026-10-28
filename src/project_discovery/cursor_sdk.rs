//! Cursor SDK JsonlLocalAgentStore agent metadata only. Author/last modifier: admin.
//! The parent calls this only for an explicit KITTER_CURSOR_SDK_STORE; there is
//! no default directory or environment lookup here. IDE registration stays primary.
//! On Err the caller preserves previous Cursor entries and manual archive state.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{ErrorKind, Read},
    path::Path,
};

use anyhow::{Result, anyhow};
use serde::Deserialize;

use super::{AgentKind, ProjectEntry, native::project};

const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;
const MAX_ROW_BYTES: usize = 64 * 1024;
const MAX_ENTRIES: usize = 10_000;

#[derive(Clone, Copy)]
struct Limits {
    file_bytes: usize,
    row_bytes: usize,
    entries: usize,
}

const LIMITS: Limits = Limits {
    file_bytes: MAX_FILE_BYTES,
    row_bytes: MAX_ROW_BYTES,
    entries: MAX_ENTRIES,
};

fn failure() -> anyhow::Error {
    anyhow!("Cursor SDK 项目元数据暂不可读或格式无效")
}

// Validate only necessary identity/lifecycle fields. Unknown fields (including
// sdkMetadata) are ignored, never projected or included in errors.
#[derive(Deserialize)]
struct AgentMetadata {
    #[serde(rename = "agentId")]
    agent_id: String,
    cwd: String,
    #[serde(rename = "status")]
    _status: AgentStatus,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum AgentStatus {
    Idle,
    Running,
    Error,
    Archived,
}

/// Reads only the fixed agents.ndjson leaf of an explicitly selected store.
/// Session/agent lifecycle status is NOT evidence of project archival.
pub(super) fn projects_at(store: &Path) -> Result<Vec<ProjectEntry>> {
    read_projects(store, LIMITS)
}

fn read_projects(store: &Path, limits: Limits) -> Result<Vec<ProjectEntry>> {
    let mut file = open_metadata(store)?;
    let before = file.metadata().map_err(|_| failure())?;
    if !before.is_file() || before.len() > limits.file_bytes as u64 {
        return Err(failure());
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(limits.file_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure())?;
    if bytes.len() > limits.file_bytes || bytes.len() as u64 != before.len() {
        return Err(failure());
    }
    // Atomic replacement, mutation, or an unreadable/replaced parent chain is
    // unknown, not a successful empty catalog. Reopen through the same fd walk.
    let after = file.metadata().map_err(|_| failure())?;
    let current = open_metadata(store)?.metadata().map_err(|_| failure())?;
    if !same_snapshot(&before, &after) || !same_snapshot(&before, &current) {
        return Err(failure());
    }

    let mut rows = 0usize;
    let mut projects = BTreeMap::new();
    let mut agent_ids = BTreeSet::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        // LF is the delimiter; CR and other whitespace still count as row bytes.
        if line.len() > limits.row_bytes {
            return Err(failure());
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        rows = rows.checked_add(1).ok_or_else(failure)?;
        if rows > limits.entries {
            return Err(failure());
        }
        let row: AgentMetadata = serde_json::from_slice(line).map_err(|_| failure())?;
        if row.agent_id.trim().is_empty()
            || row.agent_id.contains('\0')
            || row.cwd.contains('\0')
            || !Path::new(&row.cwd).is_absolute()
        {
            return Err(failure());
        }
        // SDK 1.0.40 rewrites a full agent snapshot for create/update/delete;
        // create rejects an existing agentId, and delete filters rows out.
        // Duplicate IDs are not an append-log protocol: no guessed first/last
        // winner, even when one row's cwd is now missing.
        if !agent_ids.insert(row.agent_id) {
            return Err(failure());
        }
        // cwd is a literal native path: never percent-decode, replace '+', or
        // trim meaningful filename whitespace.
        let identity = match fs::canonicalize(&row.cwd) {
            Ok(path) => path,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(_) => return Err(failure()),
        };
        if !fs::metadata(&identity).map_err(|_| failure())?.is_dir() {
            continue;
        }
        // The shared constructor sets archive_states=[], archived/hidden/
        // retained=false. Do not let its Option suppress an intervening I/O error.
        let entry = project(identity.to_str().ok_or_else(failure)?, AgentKind::Cursor)
            .ok_or_else(failure)?;
        if entry.path != identity {
            return Err(failure());
        }
        projects.entry(identity).or_insert(entry);
    }
    Ok(projects.into_values().collect())
}

#[cfg(unix)]
fn open_metadata(store: &Path) -> Result<File> {
    use std::{
        ffi::{CString, OsStr},
        fs::OpenOptions,
        os::{fd::{AsRawFd, FromRawFd}, unix::{ffi::OsStrExt, fs::OpenOptionsExt}},
        path::Component,
    };

    fn open_at(directory: &File, name: &OsStr, is_directory: bool) -> Result<File> {
        let name = CString::new(name.as_bytes()).map_err(|_| failure())?;
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC
            | if is_directory { libc::O_DIRECTORY } else { 0 };
        // SAFETY: directory and the NUL-terminated name remain alive for openat.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(failure());
        }
        // SAFETY: successful openat transferred one fresh owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    if !store.is_absolute() {
        return Err(failure());
    }
    // Like the existing anchored SQLite reader, use directory descriptors and
    // O_NOFOLLOW. Walking from / also rejects symlinks in every ancestor.
    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")
        .map_err(|_| failure())?;
    for component in store.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => directory = open_at(&directory, name, true)?,
            _ => return Err(failure()),
        }
    }
    // O_NONBLOCK prevents a substituted FIFO from blocking before fstat. The
    // caller verifies regular-file metadata before any data read.
    open_at(&directory, OsStr::new("agents.ndjson"), false)
}

#[cfg(not(unix))]
fn open_metadata(_store: &Path) -> Result<File> {
    // No path-based fallback that could silently follow a replaced parent.
    Err(failure())
}

fn same_snapshot(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        after.is_file()
            && (before.dev(), before.ino(), before.len(), before.mtime(), before.mtime_nsec(),
                before.ctime(), before.ctime_nsec())
                == (after.dev(), after.ino(), after.len(), after.mtime(), after.mtime_nsec(),
                    after.ctime(), after.ctime_nsec())
    }
    #[cfg(not(unix))]
    {
        let _ = (before, after);
        false
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::path::PathBuf;

    struct Fixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        store: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let store = root.join("store");
            fs::create_dir(&store).unwrap();
            Self { _temp: temp, root, store }
        }

        fn project(&self, name: &str) -> PathBuf {
            let path = self.root.join(name);
            fs::create_dir(&path).unwrap();
            path
        }

        fn write(&self, rows: &[Value]) {
            let mut bytes = Vec::new();
            for row in rows {
                serde_json::to_writer(&mut bytes, row).unwrap();
                bytes.push(b'\n');
            }
            fs::write(self.store.join("agents.ndjson"), bytes).unwrap();
        }
    }

    // Synthetic contract rows in the observed SDK schema; not native evidence.
    fn row(id: &str, cwd: &Path, status: &str) -> Value {
        json!({"agentId":id, "cwd":cwd, "status":status, "activeRunId":null,
            "createdAt":1, "updatedAt":2, "name":"Synthetic contract fixture",
            "sdkMetadata":{"synthetic":true}})
    }

    fn assert_fixed_error(result: Result<Vec<ProjectEntry>>) {
        assert_eq!(format!("{:#}", result.unwrap_err()), failure().to_string());
    }

    #[test]
    fn synthetic_native2_schema_preserves_literal_cwds_and_unknown_archive() {
        let fixture = Fixture::new();
        let a = fixture.project("A");
        let b = fixture.project("B 中文+%");
        fixture.write(&[row("agent-a", &a, "idle"), row("agent-b", &b, "idle")]);
        let entries = projects_at(&fixture.store).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries.iter().map(|entry| &entry.path).collect::<Vec<_>>(), vec![&a, &b]);
        for entry in entries {
            assert_eq!(entry.agents, vec![AgentKind::Cursor]);
            assert!(entry.archive_states.is_empty());
            assert!(!entry.archived && !entry.hidden && !entry.retained);
        }
    }

    #[test]
    fn canonical_cwd_dedup_is_independent_of_agent_status() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let cwd = fixture.project("project");
        let alias = fixture.root.join("project-alias");
        symlink(&cwd, &alias).unwrap();
        fixture.write(&[row("agent-a", &cwd, "running"), row("agent-b", &alias, "archived")]);
        let entries = projects_at(&fixture.store).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, cwd);
        assert!(entries[0].archive_states.is_empty());
        assert!(!entries[0].archived);
    }

    #[test]
    fn archived_agent_never_archives_its_project() {
        let fixture = Fixture::new();
        let cwd = fixture.project("project");
        fixture.write(&[row("agent-a", &cwd, "archived")]);
        let entries = projects_at(&fixture.store).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].archived);
        assert!(entries[0].archive_states.is_empty());
    }

    #[test]
    fn malformed_identity_status_and_partial_catalog_have_fixed_errors() {
        let fixture = Fixture::new();
        let cwd = fixture.project("project");
        let valid = row("agent-a", &cwd, "idle");
        for field in ["agentId", "cwd", "status"] {
            let mut invalid = valid.clone();
            invalid.as_object_mut().unwrap().remove(field);
            fixture.write(&[valid.clone(), invalid]);
            assert_fixed_error(projects_at(&fixture.store));
        }
        for (field, value) in [("agentId", json!(" ")), ("agentId", json!(42)),
            ("cwd", json!("relative")), ("cwd", json!("/nul\0path")),
            ("status", json!("unknown")), ("status", json!("removed")), ("status", Value::Null)] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            fixture.write(&[invalid]);
            assert_fixed_error(projects_at(&fixture.store));
        }
        for raw in [&b"{private-malformed-value"[..], &b"\xff\n"[..], &b"[]\n"[..]] {
            fs::write(fixture.store.join("agents.ndjson"), raw).unwrap();
            assert_fixed_error(projects_at(&fixture.store));
        }
        let duplicate_id = format!("{{\"agentId\":\"one\",\"agentId\":\"two\",\"cwd\":{},\"status\":\"idle\"}}",
            serde_json::to_string(&cwd).unwrap());
        fs::write(fixture.store.join("agents.ndjson"), duplicate_id).unwrap();
        assert_fixed_error(projects_at(&fixture.store));
    }

    #[test]
    fn duplicate_ids_never_project_old_and_new_cwds_as_current() {
        let fixture = Fixture::new();
        let old = fixture.project("old");
        let new = fixture.project("new");
        let missing = fixture.root.join("missing-old");
        for previous in [&old, &missing] {
            fixture.write(&[row("agent-a", previous, "idle"), row("agent-a", &new, "idle")]);
            assert_fixed_error(projects_at(&fixture.store));
        }
        fixture.write(&[row("agent-a", &new, "idle"), row("agent-a", &new, "archived")]);
        assert_fixed_error(projects_at(&fixture.store));
        fixture.write(&[json!({"agentId":"agent-a", "removed":true})]);
        assert_fixed_error(projects_at(&fixture.store));
    }

    #[test]
    fn rewritten_update_and_delete_snapshots_do_not_resurrect_old_cwds() {
        let fixture = Fixture::new();
        let old = fixture.project("old");
        let current = fixture.project("current");
        let other = fixture.project("other");
        fixture.write(&[row("agent-a", &old, "idle"), row("agent-b", &other, "idle")]);
        assert_eq!(projects_at(&fixture.store).unwrap().len(), 2);
        fixture.write(&[row("agent-a", &current, "idle"), row("agent-b", &other, "idle")]);
        let entries = projects_at(&fixture.store).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(!entries.iter().any(|entry| entry.path == old));
        assert!(entries.iter().any(|entry| entry.path == current));
        fixture.write(&[row("agent-b", &other, "idle")]);
        let entries = projects_at(&fixture.store).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, other);
        fixture.write(&[]);
        assert!(projects_at(&fixture.store).unwrap().is_empty());
    }

    #[test]
    fn missing_metadata_is_unknown_but_missing_cwd_is_stale() {
        let fixture = Fixture::new();
        assert_fixed_error(projects_at(&fixture.store));
        fixture.write(&[row("agent-a", &fixture.root.join("missing-project"), "archived")]);
        assert!(projects_at(&fixture.store).unwrap().is_empty());
        fs::write(fixture.store.join("agents.ndjson"), b"\n \r\n").unwrap();
        assert!(projects_at(&fixture.store).unwrap().is_empty());
    }

    #[test]
    fn unknown_cwd_io_is_not_an_empty_or_partial_success() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let cwd = fixture.project("project");
        let looping = fixture.root.join("loop");
        symlink(&looping, &looping).unwrap();
        assert_ne!(fs::canonicalize(&looping).unwrap_err().kind(), ErrorKind::NotFound);
        fixture.write(&[row("agent-a", &cwd, "idle"), row("agent-b", &looping, "idle")]);
        assert_fixed_error(projects_at(&fixture.store));
    }

    #[test]
    fn entry_budget_counts_rows_before_dedup() {
        let fixture = Fixture::new();
        let cwd = fixture.project("project");
        fixture.write(&[row("agent-a", &cwd, "idle"), row("agent-b", &cwd, "idle")]);
        assert_eq!(read_projects(&fixture.store, Limits { entries: 2, ..LIMITS }).unwrap().len(), 1);
        assert_fixed_error(read_projects(&fixture.store, Limits { entries: 1, ..LIMITS }));
    }

    #[test]
    fn row_budget_accepts_the_boundary_and_rejects_overflow() {
        let fixture = Fixture::new();
        let cwd = fixture.project("project");
        let mut bytes = serde_json::to_vec(&row("agent-a", &cwd, "idle")).unwrap();
        bytes.resize(MAX_ROW_BYTES, b' ');
        bytes.push(b'\n');
        fs::write(fixture.store.join("agents.ndjson"), &bytes).unwrap();
        assert_eq!(projects_at(&fixture.store).unwrap().len(), 1);
        bytes.insert(MAX_ROW_BYTES, b' ');
        fs::write(fixture.store.join("agents.ndjson"), &bytes).unwrap();
        assert_fixed_error(projects_at(&fixture.store));
    }

    #[test]
    fn file_budget_is_checked_before_parsing() {
        let fixture = Fixture::new();
        let cwd = fixture.project("project");
        fixture.write(&[row("agent-a", &cwd, "idle")]);
        let path = fixture.store.join("agents.ndjson");
        let length = fs::metadata(&path).unwrap().len() as usize;
        assert_eq!(read_projects(&fixture.store, Limits { file_bytes: length, ..LIMITS }).unwrap().len(), 1);
        assert_fixed_error(read_projects(&fixture.store, Limits { file_bytes: length - 1, ..LIMITS }));
        File::options().write(true).open(&path).unwrap().set_len(MAX_FILE_BYTES as u64 + 1).unwrap();
        assert_fixed_error(projects_at(&fixture.store));
    }

    #[test]
    fn snapshot_identity_rejects_atomic_leaf_replacement() {
        let fixture = Fixture::new();
        fixture.write(&[]);
        let path = fixture.store.join("agents.ndjson");
        let before = fs::metadata(&path).unwrap();
        assert!(same_snapshot(&before, &before));
        let replacement = fixture.store.join("replacement");
        fs::write(&replacement, b"").unwrap();
        fs::rename(&replacement, &path).unwrap();
        assert!(!same_snapshot(&before, &fs::metadata(&path).unwrap()));
    }

    #[test]
    fn metadata_links_directories_and_fifos_are_rejected() {
        use std::{ffi::CString, os::unix::{ffi::OsStrExt, fs::symlink}};
        let fixture = Fixture::new();
        let other = fixture.root.join("outside.ndjson");
        fs::write(&other, b"\n").unwrap();
        let leaf = fixture.store.join("agents.ndjson");
        symlink(&other, &leaf).unwrap();
        assert_fixed_error(projects_at(&fixture.store));
        fs::remove_file(&leaf).unwrap();
        fs::create_dir(&leaf).unwrap();
        assert_fixed_error(projects_at(&fixture.store));
        fs::remove_dir(&leaf).unwrap();
        let name = CString::new(leaf.as_os_str().as_bytes()).unwrap();
        // SAFETY: valid NUL-terminated path, under this synthetic test fixture.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert_fixed_error(projects_at(&fixture.store));
    }

    #[test]
    fn metadata_parent_chain_links_and_relative_roots_are_rejected() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        fixture.write(&[]);
        let store_alias = fixture.root.join("store-alias");
        symlink(&fixture.store, &store_alias).unwrap();
        assert_fixed_error(projects_at(&store_alias));
        let parent_alias = fixture.root.join("parent-alias");
        symlink(&fixture.root, &parent_alias).unwrap();
        assert_fixed_error(projects_at(&parent_alias.join("store")));
        assert_fixed_error(projects_at(Path::new("relative/store")));
        assert_fixed_error(projects_at(&fixture.store.join("..")));
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn unsupported_anchored_reader_rejects_valid_metadata_without_changes() {
        let temp = tempfile::tempdir().unwrap();
        let metadata = temp.path().join("agents.ndjson");
        let bytes = serde_json::to_vec(&serde_json::json!({
            "agentId":"native", "cwd":temp.path(), "status":"idle"
        })).unwrap();
        fs::write(&metadata, &bytes).unwrap();
        assert!(projects_at(temp.path()).is_err());
        assert_eq!(fs::read(metadata).unwrap(), bytes);
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }
}
