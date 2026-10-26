//! Factory SDK 0.9.1 native metadata; see droid-storage.json for pinned evidence.
//! Register `mod droid;` and `(AgentKind::Droid, super::droid::droid_projects(home))`
//! in the parent/native catalogs. On Err retain the previous Droid source.
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Read},
    path::Path,
    time::SystemTime,
};

use anyhow::{Result, anyhow, bail};
use serde_json::Value;

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
    anyhow!("Droid 原生会话元数据暂不可读或格式无效")
}
fn charge(used: &mut usize, amount: usize, limit: usize) -> Result<()> {
    *used = used.checked_add(amount).ok_or_else(failure)?;
    if *used > limit {
        bail!("Droid 原生会话元数据超过读取上限");
    }
    Ok(())
}

struct Budget {
    entries: usize,
    files: usize,
    bytes: usize,
    limits: Limits,
}
impl Budget {
    fn open(&mut self, path: &Path) -> Result<File> {
        charge(&mut self.files, 1, self.limits.files)?;
        if !fs::symlink_metadata(path).map_err(|_| failure())?.is_file() {
            return Err(failure());
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(path).map_err(|_| failure())?;
        if !file.metadata().map_err(|_| failure())?.is_file() {
            return Err(failure());
        }
        Ok(file)
    }
    fn read(&mut self, input: &mut impl Read, header_only: bool) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        // Deliberately unbuffered: even a buffered read_until can consume chat
        // bytes beyond LF. No second line is read, including on malformed headers.
        loop {
            let mut byte = [0];
            match input.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return Err(failure()),
            }
            charge(&mut self.bytes, 1, self.limits.total_bytes)?;
            if bytes.len() >= self.limits.file_bytes {
                bail!("Droid 原生会话元数据超过读取上限");
            }
            if header_only && byte[0] == b'\n' {
                break;
            }
            bytes.push(byte[0]);
        }
        Ok(bytes)
    }
}

fn directory(path: &Path) -> Result<()> {
    if !fs::symlink_metadata(path).map_err(|_| failure())?.is_dir() {
        return Err(failure());
    }
    Ok(())
}

/// Reads only the official default storage; never consults environment/config,
/// invokes the SDK/CLI, reads favorites, or decodes lossy bucket names into cwd.
pub(super) fn droid_projects(home: &Path) -> Result<Vec<ProjectEntry>> {
    read_projects(home, LIMITS)
}

fn read_projects(home: &Path, limits: Limits) -> Result<Vec<ProjectEntry>> {
    if !home.is_absolute() {
        return Err(failure());
    }
    let factory = home.join(".factory");
    directory(&factory)?;
    let root = factory.join("sessions");
    directory(&root)?;
    let mut budget = Budget {
        entries: 0,
        files: 0,
        bytes: 0,
        limits,
    };
    // Same session ID can exist in legacy and project layouts. Resolve copies
    // by mtime before combining projects, including archived copies.
    let mut sessions = BTreeMap::new();
    scan(&root, true, &mut budget, &mut sessions)?;
    let mut projects: BTreeMap<_, ProjectEntry> = BTreeMap::new();
    for (_, (_, entry)) in sessions {
        if let Some(entry) = entry {
            projects
                .entry(entry.path.clone())
                .and_modify(|old| old.merge_archival(&entry))
                .or_insert(entry);
        }
    }
    Ok(projects.into_values().collect())
}

type Sessions = BTreeMap<String, (SystemTime, Option<ProjectEntry>)>;
fn scan(dir: &Path, root: bool, budget: &mut Budget, sessions: &mut Sessions) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(|_| failure())? {
        charge(&mut budget.entries, 1, budget.limits.entries)?;
        let entry = entry.map_err(|_| failure())?;
        let kind = entry.file_type().map_err(|_| failure())?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if root && kind.is_dir() && name.starts_with('-') {
            scan(&entry.path(), false, budget, sessions)?;
            continue;
        }
        let Some(id) = name.strip_suffix(".jsonl") else {
            continue;
        };
        if kind.is_dir() {
            continue;
        }
        if id.is_empty() {
            return Err(failure());
        }
        let mut file = budget.open(&entry.path())?;
        let mtime = file
            .metadata()
            .and_then(|m| m.modified())
            .map_err(|_| failure())?;
        let bytes = budget.read(&mut file, true)?;
        let header: Value = serde_json::from_slice(&bytes).map_err(|_| failure())?;
        let cwd = header_cwd(&header)?;
        let archived = read_archived(&dir.join(format!("{id}.settings.json")), budget)?;
        let project = match cwd {
            Some(cwd) if Path::new(cwd).is_absolute() && !cwd.contains('\0') => {
                let identity = match fs::canonicalize(cwd) {
                    Ok(path) => Some(path),
                    Err(e) if e.kind() == ErrorKind::NotFound => None,
                    Err(_) => return Err(failure()),
                };
                if let Some(identity) = identity {
                    let mut entry =
                        project(identity.to_str().ok_or_else(failure)?, AgentKind::Droid);
                    // Check errors rather than converting unreadable paths to absence.
                    if fs::metadata(&identity).map_err(|_| failure())?.is_dir() {
                        if let Some(entry) = &mut entry {
                            entry.record_archive(AgentKind::Droid, archived);
                        }
                    }
                    entry
                } else {
                    None
                }
            }
            _ => None,
        };
        if sessions
            .get(id)
            .is_some_and(|(old_time, old)| *old_time == mtime && old != &project)
        {
            // Equal-time conflicting copies have no authoritative winner.
            return Err(failure());
        }
        if sessions.get(id).is_none_or(|(old, _)| mtime > *old) {
            sessions.insert(id.to_owned(), (mtime, project));
        }
    }
    Ok(())
}

fn header_cwd(header: &Value) -> Result<Option<&str>> {
    if !header.is_object() || header.get("type").and_then(Value::as_str) != Some("session_start") {
        return Err(failure());
    }
    // SDK normalizeSessionStartEvent replaces title with a nonempty legacy
    // sessionTitle; otherwise title itself must still be a string.
    let legacy_title = header
        .get("sessionTitle")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty());
    if legacy_title
        .or_else(|| header.get("title").and_then(Value::as_str))
        .is_none()
    {
        return Err(failure());
    }
    for field in ["sessionId", "owner", "decompMissionId", "cwd"] {
        if header.get(field).is_some_and(|v| !v.is_string()) {
            return Err(failure());
        }
    }
    Ok(header.get("cwd").and_then(Value::as_str))
}

fn read_archived(path: &Path, budget: &mut Budget) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(failure()),
        Ok(_) => {}
    }
    let mut file = budget.open(path)?;
    let bytes = budget.read(&mut file, false)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| failure())?;
    if !value.is_object() {
        return Err(failure());
    }
    match value.get("archivedAt") {
        None => Ok(false),
        Some(Value::String(value)) => Ok(!value.is_empty()),
        _ => Err(failure()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Cursor, path::PathBuf};

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let sessions = home.join(".factory/sessions");
        fs::create_dir_all(&sessions).unwrap();
        (temp, home, sessions)
    }
    fn session(dir: &Path, id: &str, cwd: &Path) {
        let header = serde_json::json!({"type":"session_start","title":"fixture", "cwd":cwd});
        fs::write(
            dir.join(format!("{id}.jsonl")),
            format!("{header}\nTHIS IS NOT JSON: transcript must not be read"),
        )
        .unwrap();
    }
    fn budget(limits: Limits) -> Budget {
        Budget {
            entries: 0,
            files: 0,
            bytes: 0,
            limits,
        }
    }

    #[test]
    fn discovers_legacy_and_one_level_buckets_without_guessing_cwd() {
        let (_temp, home, root) = fixture();
        let cwd = home.join("中文 + percent% project");
        fs::create_dir(&cwd).unwrap();
        session(&root, "legacy", &cwd);
        let bucket = root.join("-deliberately-lossy");
        fs::create_dir(&bucket).unwrap();
        session(&bucket, "current", &cwd);
        let ignored = root.join("not-a-bucket");
        fs::create_dir(&ignored).unwrap();
        session(&ignored, "ignored", &home);
        let deeper = bucket.join("-deeper");
        fs::create_dir(&deeper).unwrap();
        session(&deeper, "deep", &home);
        fs::write(root.join("config.json"), "invalid unrelated metadata").unwrap();
        fs::write(root.join(".favorites"), "not read").unwrap();
        let projects = droid_projects(&home).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, cwd);
        assert_eq!(projects[0].agents, [AgentKind::Droid]);
        assert!(!projects[0].archived);
    }

    #[test]
    fn only_header_cwd_counts_and_legacy_title_is_supported() {
        let (_temp, home, root) = fixture();
        for (id, cwd) in [
            ("relative", "relative".to_owned()),
            ("stale", home.join("missing").to_string_lossy().into_owned()),
            ("nul", "\0invalid".to_owned()),
        ] {
            fs::write(
                root.join(format!("{id}.jsonl")),
                format!(
                    "{}\n",
                    serde_json::json!({"type":"session_start","title":"x","cwd":cwd})
                ),
            )
            .unwrap();
        }
        fs::write(
            root.join("no-cwd.jsonl"),
            format!(
                "{}\n{}\n",
                serde_json::json!({"type":"session_start","title":"x","message":{"cwd":home}}),
                serde_json::json!({"cwd":home})
            ),
        )
        .unwrap();
        assert!(droid_projects(&home).unwrap().is_empty());
        fs::write(
            root.join("old.jsonl"),
            format!(
                "{}",
                serde_json::json!({"type":"session_start","sessionTitle":"legacy","cwd":home})
            ),
        )
        .unwrap();
        assert_eq!(droid_projects(&home).unwrap().len(), 1);
        fs::write(
            root.join("file-cwd.jsonl"),
            format!(
                "{}\n",
                serde_json::json!({"type":"session_start","title":"x","cwd":root.join("old.jsonl")})
            ),
        )
        .unwrap();
        assert_eq!(droid_projects(&home).unwrap().len(), 1);
    }

    #[test]
    fn never_reads_beyond_first_lf_and_enforces_header_and_total_budgets() {
        let mut input = Cursor::new(b"header\r\nCHAT SECRET".to_vec());
        assert_eq!(budget(LIMITS).read(&mut input, true).unwrap(), b"header\r");
        assert_eq!(input.position(), 8);
        let mut b = budget(Limits {
            file_bytes: 3,
            ..LIMITS
        });
        assert!(b.read(&mut Cursor::new(b"long header\n"), true).is_err());
        let mut b = budget(Limits {
            total_bytes: 3,
            ..LIMITS
        });
        assert!(b.read(&mut Cursor::new(b"long\n"), true).is_err());
        let mut b = budget(Limits {
            file_bytes: 3,
            ..LIMITS
        });
        assert!(b.read(&mut Cursor::new(b"four"), false).is_err());
    }

    #[test]
    fn archive_requires_all_sessions_and_latest_copy_wins() {
        let (_temp, home, root) = fixture();
        session(&root, "one", &home);
        fs::write(
            root.join("one.settings.json"),
            r#"{"archivedAt":"2026-10-19"}"#,
        )
        .unwrap();
        assert!(droid_projects(&home).unwrap()[0].archived);
        session(&root, "two", &home);
        assert!(!droid_projects(&home).unwrap()[0].archived);
        // Synthetic timestamps avoid sleeps and nondeterministic mtime ordering.
        let mut sessions = BTreeMap::new();
        let mut b = budget(LIMITS);
        let bucket = root.join("-copy");
        fs::create_dir(&bucket).unwrap();
        session(&bucket, "one", &home);
        File::options()
            .write(true)
            .open(root.join("one.jsonl"))
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(10))
            .unwrap();
        scan(&root, false, &mut b, &mut sessions).unwrap();
        scan(&bucket, false, &mut b, &mut sessions).unwrap();
        assert!(!sessions["one"].1.as_ref().unwrap().archived);
        let time = File::open(root.join("one.jsonl"))
            .unwrap()
            .metadata()
            .unwrap()
            .modified()
            .unwrap();
        File::options()
            .write(true)
            .open(bucket.join("one.jsonl"))
            .unwrap()
            .set_modified(time)
            .unwrap();
        let mut sessions = BTreeMap::new();
        let mut b = budget(LIMITS);
        scan(&root, false, &mut b, &mut sessions).unwrap();
        assert!(scan(&bucket, false, &mut b, &mut sessions).is_err());
    }

    #[test]
    fn malformed_metadata_and_budget_exhaustion_fail_without_partial_catalog() {
        let (_temp, home, root) = fixture();
        session(&root, "valid", &home);
        assert!(
            read_projects(
                &home,
                Limits {
                    entries: 0,
                    ..LIMITS
                }
            )
            .is_err()
        );
        assert!(read_projects(&home, Limits { files: 0, ..LIMITS }).is_err());
        for value in [
            "",
            "SECRET INVALID",
            r#"{"type":"user","cwd":"/fake"}"#,
            r#"{"type":"session_start","title":"x","cwd":2}"#,
        ] {
            fs::write(root.join("bad.jsonl"), value).unwrap();
            let error = droid_projects(&home).unwrap_err().to_string();
            assert!(!error.contains("SECRET"));
        }
        fs::write(root.join("bad.jsonl"), [0xff, b'\n']).unwrap();
        assert!(droid_projects(&home).is_err());
        fs::remove_file(root.join("bad.jsonl")).unwrap();
        for value in ["SECRET", "[]", r#"{"archivedAt":true}"#] {
            fs::write(root.join("valid.settings.json"), value).unwrap();
            assert!(droid_projects(&home).is_err());
        }
        assert!(droid_projects(&home.join("missing-home")).is_err());
        assert!(droid_projects(Path::new("relative")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_nonfiles_are_not_opened() {
        use std::os::unix::fs::symlink;
        let (_temp, home, root) = fixture();
        session(&root, "valid", &home);
        symlink(home.join("missing"), root.join("bad.jsonl")).unwrap();
        assert!(droid_projects(&home).is_err());
        fs::remove_file(root.join("bad.jsonl")).unwrap();
        symlink(home.join("missing"), root.join("valid.settings.json")).unwrap();
        assert!(droid_projects(&home).is_err());
        fs::remove_file(root.join("valid.settings.json")).unwrap();
        fs::create_dir(root.join("valid.settings.json")).unwrap();
        assert!(droid_projects(&home).is_err());
        fs::remove_dir(root.join("valid.settings.json")).unwrap();
        symlink(home.join("missing"), root.join("-symlink-bucket")).unwrap();
        assert_eq!(droid_projects(&home).unwrap().len(), 1);
        if unsafe { libc::geteuid() } != 0 {
            use std::os::unix::fs::PermissionsExt;
            let path = root.join("valid.jsonl");
            fs::set_permissions(&path, fs::Permissions::from_mode(0)).unwrap();
            assert!(droid_projects(&home).is_err());
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let alias = home.join("alias");
        fs::create_dir(&alias).unwrap();
        symlink(home.join(".factory"), alias.join(".factory")).unwrap();
        assert!(droid_projects(&alias).is_err());
    }
}
