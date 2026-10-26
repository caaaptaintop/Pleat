//! Copilot CLI 0.0.420 workspace metadata, pinned in copilot-storage.json.
//! The 1.0.98 native binary's storage implementation remains unverified.
//! Integration: parent `mod copilot;`, native catalog AgentKind::Copilot.
//! Caller must retain the previous Copilot source on Err, including budgets.
use std::{
    collections::BTreeMap,
    fs::{self, Metadata, OpenOptions},
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow, bail};
use serde_yaml::Value;

use super::{AgentKind, ProjectEntry, native::project};

#[derive(Clone, Copy)]
struct Limits {
    entries: usize,
    files: usize,
    file_bytes: usize,
    total_bytes: usize,
}
const LIMITS: Limits = Limits {
    entries: 20_000,
    files: 10_000,
    file_bytes: 64 * 1024,
    total_bytes: 16 * 1024 * 1024,
};

fn failure() -> anyhow::Error {
    anyhow!("Copilot 原生工作区元数据暂不可读或格式无效")
}
fn charge(used: &mut usize, amount: usize, limit: usize) -> Result<()> {
    *used = used.checked_add(amount).ok_or_else(failure)?;
    if *used > limit {
        bail!("Copilot 原生工作区元数据超过读取上限");
    }
    Ok(())
}
fn directory(path: &Path) -> Result<()> {
    if !fs::symlink_metadata(path).map_err(|_| failure())?.is_dir() {
        return Err(failure());
    }
    Ok(())
}
fn same_file(before: &Metadata, after: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        before.dev() == after.dev() && before.ino() == after.ino()
    }
    #[cfg(not(unix))]
    {
        before.file_type() == after.file_type()
            && before.len() == after.len()
            && before.modified().ok() == after.modified().ok()
    }
}

/// Default or official XDG state tree. No CLI/SDK, config, events,
/// session database, checkpoint, plan, or artifact contents are opened.
/// An explicit per-process SDK configDir is not a global project registry.
pub(super) fn copilot_projects(home: &Path) -> Result<Vec<ProjectEntry>> {
    let state = state_root(home, std::env::var_os("XDG_STATE_HOME").as_deref())?;
    read_state(&state, LIMITS)
}

fn state_root(home: &Path, xdg: Option<&std::ffi::OsStr>) -> Result<PathBuf> {
    let base = xdg.map(Path::new).unwrap_or(home);
    if !base.is_absolute() { return Err(failure()); }
    Ok(base.join(".copilot"))
}

#[cfg(test)]
fn read_projects(home: &Path, limits: Limits) -> Result<Vec<ProjectEntry>> {
    read_state(&state_root(home, None)?, limits)
}

fn read_state(state: &Path, limits: Limits) -> Result<Vec<ProjectEntry>> {
    if !state.is_absolute() {
        return Err(failure());
    }
    directory(state)?;
    let root = state.join("session-state");
    directory(&root)?;
    let (mut entries, mut files, mut total_bytes) = (0, 0, 0);
    let mut projects = BTreeMap::new();
    for session in fs::read_dir(&root).map_err(|_| failure())? {
        charge(&mut entries, 1, limits.entries)?;
        let session = session.map_err(|_| failure())?;
        let name = session.file_name();
        let Some(id) = name.to_str().filter(|s| session_id(s)) else {
            continue;
        };
        let kind = session.file_type().map_err(|_| failure())?;
        if kind.is_symlink() {
            return Err(failure());
        }
        if !kind.is_dir() {
            continue;
        }
        let dir = session.path();
        directory(&dir)?;
        let path = dir.join("workspace.yaml");
        let before = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            // A workspace may not yet be initialized (or be disabled).
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(_) => return Err(failure()),
        };
        if !before.is_file() {
            return Err(failure());
        }
        charge(&mut files, 1, limits.files)?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(&path).map_err(|_| failure())?;
        let opened = file.metadata().map_err(|_| failure())?;
        if !opened.is_file() || !same_file(&before, &opened) {
            return Err(failure());
        }
        let max = limits.file_bytes.min(limits.total_bytes.saturating_sub(total_bytes));
        if opened.len() > max as u64 {
            bail!("Copilot 原生工作区元数据超过读取上限");
        }
        let mut bytes = Vec::new();
        file.take(max as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| failure())?;
        if bytes.len() > max {
            bail!("Copilot 原生工作区元数据超过读取上限");
        }
        charge(&mut total_bytes, bytes.len(), limits.total_bytes)?;
        let after = fs::symlink_metadata(&path).map_err(|_| failure())?;
        if !after.is_file() || !same_file(&opened, &after) {
            return Err(failure());
        }
        let metadata: Value = serde_yaml::from_slice(&bytes).map_err(|_| failure())?;
        let Some(cwd) = metadata_cwd(&metadata, id)? else {
            continue;
        };
        if !Path::new(cwd).is_absolute() || cwd.contains('\0') {
            return Err(failure());
        }
        let identity = match fs::canonicalize(cwd) {
            Ok(path) => path,
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(_) => return Err(failure()),
        };
        if !fs::metadata(&identity).map_err(|_| failure())?.is_dir() {
            continue;
        }
        if let Some(entry) = project(identity.to_str().ok_or_else(failure)?, AgentKind::Copilot) {
            projects.insert(identity, entry);
        }
    }
    Ok(projects.into_values().collect())
}

fn session_id(id: &str) -> bool {
    id.len() == 36 && id.bytes().enumerate().all(|(i, b)| {
        if [8, 13, 18, 23].contains(&i) { b == b'-' } else { b.is_ascii_hexdigit() }
    })
}

fn metadata_cwd<'a>(metadata: &'a Value, id: &str) -> Result<Option<&'a str>> {
    if !metadata.is_mapping() || metadata.get("id").and_then(Value::as_str) != Some(id) {
        return Err(failure());
    }
    match metadata.get("cwd") {
        None => Ok(None),
        Some(Value::String(cwd)) => Ok(Some(cwd)),
        _ => Err(failure()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const ONE: &str = "11111111-1111-4111-8111-111111111111";
    const TWO: &str = "22222222-2222-4222-8222-222222222222";
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let root = home.join(".copilot/session-state");
        fs::create_dir_all(&root).unwrap();
        (temp, home, root)
    }
    fn workspace(root: &Path, id: &str, cwd: &Path) -> PathBuf {
        let dir = root.join(id);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("workspace.yaml");
        fs::write(&path, serde_yaml::to_string(&serde_json::json!({
            "id": id, "cwd": cwd, "summary_count": 0,
            "git_root": "/not-the-cwd", "summary": "never displayed"
        })).unwrap()).unwrap();
        path
    }

    #[test]
    fn official_xdg_root_is_absolute_and_fixture_reads_ignore_process_environment() {
        let (_temp, home, root) = fixture();
        assert_eq!(state_root(&home, None).unwrap(), home.join(".copilot"));
        let alternate = home.join("xdg-state");
        assert_eq!(state_root(&home, Some(alternate.as_os_str())).unwrap(), alternate.join(".copilot"));
        for invalid in ["", "relative"] { assert!(state_root(&home, Some(std::ffi::OsStr::new(invalid))).is_err()); }
        workspace(&root, ONE, &home);
        assert_eq!(read_projects(&home, LIMITS).unwrap().len(), 1);
    }

    #[test]
    fn native_yaml_uses_only_top_level_cwd_and_deduplicates() {
        let (_temp, home, root) = fixture();
        let cwd = home.join("中文 Project");
        fs::create_dir(&cwd).unwrap();
        workspace(&root, ONE, &cwd);
        workspace(&root, TWO, &cwd);
        fs::create_dir(root.join("33333333-3333-4333-8333-333333333333")).unwrap();
        let rows = read_projects(&home, LIMITS).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, cwd);
        assert_eq!(rows[0].agents, [AgentKind::Copilot]);
        assert!(!rows[0].archived && !rows[0].hidden && !rows[0].retained);
        workspace(&root, ONE, &home.join("missing"));
        workspace(&root, TWO, &home.join("missing"));
        assert!(read_projects(&home, LIMITS).unwrap().is_empty());
    }

    #[test]
    fn missing_cwd_is_not_mined_from_nested_or_other_fields() {
        let (_temp, home, root) = fixture();
        let path = workspace(&root, ONE, &home);
        fs::write(&path, format!("id: {ONE}\ncontext:\n  cwd: /tmp\ngit_root: /tmp\n")).unwrap();
        assert!(read_projects(&home, LIMITS).unwrap().is_empty());
        for value in ["relative", "", "/bad\0path"] {
            workspace(&root, ONE, Path::new(value));
            assert!(read_projects(&home, LIMITS).is_err());
        }
        for text in ["cwd: /tmp", "[]", "id: [", "id: wrong\ncwd: /tmp"] {
            fs::write(&path, text).unwrap();
            assert!(read_projects(&home, LIMITS).is_err());
        }
        fs::write(&path, format!("id: {ONE}\ncwd: 123\n")).unwrap();
        assert!(read_projects(&home, LIMITS).is_err());
        assert!(copilot_projects(Path::new("relative")).is_err());
        assert!(copilot_projects(&home.join("missing-home")).is_err());
    }

    #[test]
    fn budgets_fail_instead_of_returning_partial_rows() {
        let (_temp, home, root) = fixture();
        let path = workspace(&root, ONE, &home);
        for limits in [
            Limits { entries: 0, ..LIMITS },
            Limits { files: 0, ..LIMITS },
            Limits { file_bytes: 1, ..LIMITS },
            Limits { total_bytes: 1, ..LIMITS },
        ] {
            assert!(read_projects(&home, limits).unwrap_err().to_string().contains("读取上限"));
        }
        let size = fs::metadata(path).unwrap().len() as usize;
        workspace(&root, TWO, &home);
        assert!(read_projects(&home, Limits { total_bytes: size, ..LIMITS }).is_err());
        fs::remove_dir_all(&root).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(root.join("unrelated"), "ignored").unwrap();
        assert!(read_projects(&home, Limits { entries: 0, ..LIMITS }).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn ignores_events_and_rejects_metadata_links_nonfiles_and_unreadable_files() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let (_temp, home, root) = fixture();
        let path = workspace(&root, ONE, &home);
        let dir = path.parent().unwrap();
        for name in ["events.jsonl", "session.db", "plan.md", "checkpoints", "files"] {
            symlink(home.join("NEVER_OPEN"), dir.join(name)).unwrap();
        }
        assert_eq!(read_projects(&home, LIMITS).unwrap().len(), 1);
        let alias = home.join("alias");
        symlink(&home, &alias).unwrap();
        workspace(&root, TWO, &alias);
        assert_eq!(read_projects(&home, LIMITS).unwrap().len(), 1);
        let original = fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        symlink(home.join("NEVER_OPEN"), &path).unwrap();
        assert!(read_projects(&home, LIMITS).is_err());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(read_projects(&home, LIMITS).is_err());
        fs::remove_dir(&path).unwrap();
        // A FIFO must be rejected before open, rather than waiting for a writer.
        let fifo = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(read_projects(&home, LIMITS).is_err());
        fs::remove_file(&path).unwrap();
        fs::write(&path, original).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        let result = read_projects(&home, LIMITS);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        if unsafe { libc::geteuid() } != 0 { assert!(result.is_err()); }
        let old = fs::metadata(&path).unwrap();
        let replacement = dir.join("replacement");
        fs::write(&replacement, fs::read(&path).unwrap()).unwrap();
        fs::rename(&replacement, &path).unwrap();
        assert!(!same_file(&old, &fs::metadata(&path).unwrap()));
        symlink(root.join(ONE), root.join("44444444-4444-4444-8444-444444444444")).unwrap();
        assert!(read_projects(&home, LIMITS).is_err());
    }
}
