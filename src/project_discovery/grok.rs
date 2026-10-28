//! Grok native session metadata only; never opens transcripts or search SQLite.
//! Integration: register `mod grok;` in the parent, then add
//! `(AgentKind::Grok, super::grok::grok_projects(home))` to native catalogs.
//! Caller must preserve its previous Grok source on Err (including budgets).

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
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

const MAX_CWD_BYTES: usize = 16 * 1024;

const LIMITS: Limits = Limits {
    entries: 10_000,
    files: 10_000,
    file_bytes: 64 * 1024,
    total_bytes: 64 * 1024 * 1024,
};

/// Only reads <GROK_HOME>/sessions or <home>/.grok/sessions.
/// No config, credentials, skills, commands, or SQLite are consulted.
pub(super) fn grok_projects(home: &Path) -> Result<Vec<ProjectEntry>> {
    let grok_home = crate::agents::grok::grok_config_root(home)?;
    read_sessions(&grok_home.join("sessions"), LIMITS)
}

fn unreadable() -> anyhow::Error {
    anyhow!("Grok 会话索引暂不可读")
}

fn count(seen: &mut usize, amount: usize, limit: usize) -> Result<()> {
    *seen = seen
        .checked_add(amount)
        .ok_or_else(|| anyhow!("Grok 会话索引超过读取上限"))?;
    if *seen > limit {
        bail!("Grok 会话索引超过读取上限");
    }
    Ok(())
}

/// Strict UTF-8 percent decoding, once only. A plus is a literal plus, never a
/// form-encoded space. Malformed escapes/NUL are rejected rather than repaired.
fn decode_cwd(name: &str) -> Option<String> {
    fn hex(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    let mut input = name.as_bytes().iter().copied();
    let mut bytes = Vec::with_capacity(name.len());
    while let Some(byte) = input.next() {
        bytes.push(if byte == b'%' {
            hex(input.next()?)? * 16 + hex(input.next()?)?
        } else {
            byte
        });
    }
    if bytes.contains(&0) {
        return None;
    }
    let cwd = String::from_utf8(bytes).ok()?;
    Path::new(&cwd).is_absolute().then_some(cwd)
}

fn read_sessions(root: &Path, limits: Limits) -> Result<Vec<ProjectEntry>> {
    // Do not follow metadata symlinks into arbitrary user files.
    if !fs::symlink_metadata(root)
        .map_err(|_| unreadable())?
        .is_dir()
    {
        return Err(unreadable());
    }
    let buckets = fs::read_dir(root).map_err(|_| unreadable())?;
    let (mut entries_seen, mut files_seen, mut bytes_seen) = (0, 0, 0);
    let mut projects = BTreeMap::new();
    for bucket in buckets {
        count(&mut entries_seen, 1, limits.entries)?;
        let bucket = bucket.map_err(|_| unreadable())?;
        if !bucket.file_type().map_err(|_| unreadable())?.is_dir() {
            continue;
        }
        let name = bucket.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        // Match the official decoder: URL-decoded absolute path first, then
        // trimmed UTF-8 .cwd. The decoder does not recompute the BLAKE3 hash.
        let decoded = decode_cwd(name);
        let sidecar_used = decoded.is_none();
        let cwd = if let Some(cwd) = decoded {
            cwd
        } else {
            count(&mut entries_seen, 1, limits.entries)?;
            let Some(bytes) = metadata_file(
                &bucket.path().join(".cwd"),
                limits,
                MAX_CWD_BYTES,
                &mut files_seen,
                &mut bytes_seen,
            )?
            else {
                // A recognized hash bucket without its necessary marker is an
                // incomplete source; unrelated invalid buckets remain ignored.
                if name.rsplit_once('-').is_some_and(|(_, hash)| {
                    hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit())
                }) {
                    return Err(unreadable());
                }
                continue;
            };
            let cwd = std::str::from_utf8(&bytes)
                .map_err(|_| anyhow!("Grok 会话索引格式无效"))?
                .trim();
            if cwd.contains('\0') || !Path::new(cwd).is_absolute() {
                bail!("Grok 会话索引格式无效");
            }
            cwd.to_owned()
        };
        let identity = match fs::canonicalize(&cwd) {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(unreadable()),
        };
        if !fs::metadata(&identity).map_err(|_| unreadable())?.is_dir() {
            continue;
        }
        let sessions = fs::read_dir(bucket.path()).map_err(|_| unreadable())?;
        for session in sessions {
            let session = session.map_err(|_| unreadable())?;
            // The sidecar was already charged before the fixed-name probe.
            if sidecar_used && session.file_name() == ".cwd" {
                continue;
            }
            count(&mut entries_seen, 1, limits.entries)?;
            if !session.file_type().map_err(|_| unreadable())?.is_dir() {
                continue;
            }
            let summary = session.path().join("summary.json");
            // Missing summary denotes a native not-yet-persisted session stub.
            let Some(bytes) = metadata_file(
                &summary,
                limits,
                limits.file_bytes,
                &mut files_seen,
                &mut bytes_seen,
            )?
            else {
                continue;
            };
            let value: Value =
                serde_json::from_slice(&bytes).map_err(|_| anyhow!("Grok 会话索引格式无效"))?;
            // Official Summary.info: Info { id: SessionId, cwd: String }.
            // Directory identity is authoritative; never mine other JSON paths.
            let id = session.file_name();
            let id = id
                .to_str()
                .ok_or_else(|| anyhow!("Grok 会话索引格式无效"))?;
            if value.pointer("/info/id").and_then(Value::as_str) != Some(id)
                || value.pointer("/info/cwd").and_then(Value::as_str) != Some(cwd.as_str())
            {
                bail!("Grok 会话索引格式无效");
            }
            if let Some(entry) = project(&cwd, AgentKind::Grok) {
                projects.insert(identity.clone(), entry);
            }
        }
    }
    Ok(projects.into_values().collect())
}

/// Both necessary metadata files share the same nofollow, file-count and byte
/// budgets. Only NotFound is distinguishable from a read failure.
fn metadata_file(
    path: &Path,
    limits: Limits,
    file_limit: usize,
    files_seen: &mut usize,
    bytes_seen: &mut usize,
) -> Result<Option<Vec<u8>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(unreadable()),
    };
    if !metadata.is_file() {
        return Err(unreadable());
    }
    count(files_seen, 1, limits.files)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| unreadable())?;
    let metadata = file.metadata().map_err(|_| unreadable())?;
    if !metadata.is_file() {
        return Err(unreadable());
    }
    let remaining = limits.total_bytes.saturating_sub(*bytes_seen);
    let max = file_limit.min(limits.file_bytes).min(remaining);
    if metadata.len() > max as u64 {
        bail!("Grok 会话索引超过读取上限");
    }
    let mut bytes = Vec::new();
    file.take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unreadable())?;
    if bytes.len() > max {
        bail!("Grok 会话索引超过读取上限");
    }
    count(bytes_seen, bytes.len(), limits.total_bytes)?;
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(path: &Path) -> String {
        path.to_str()
            .unwrap()
            .as_bytes()
            .iter()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() || b"-_.~".contains(byte) {
                    (*byte as char).to_string()
                } else {
                    format!("%{byte:02X}")
                }
            })
            .collect()
    }

    fn summary(root: &Path, cwd: &Path, bucket: &str, id: &str) -> PathBuf {
        let dir = root.join(bucket).join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("summary.json"),
            serde_json::to_vec(&serde_json::json!({
                "info": {"id": id, "cwd": cwd}, "session_summary": "METADATA_ONLY",
                "created_at":"2026-10-19T00:00:00Z", "updated_at":"2026-10-19T00:00:00Z",
                "current_model_id":"synthetic", "num_messages":0
            }))
            .unwrap(),
        )
        .unwrap();
        dir
    }

    #[test]
    fn decodes_utf8_spaces_plus_and_percent_exactly_once() {
        let (encoded, expected) = if cfg!(windows) {
            ("C%3A%2f%E4%B8%AD%E6%96%87%20a+b%252F", "C:/中文 a+b%2F")
        } else { ("%2f%E4%B8%AD%E6%96%87%20a+b%252F", "/中文 a+b%2F") };
        assert_eq!(decode_cwd(encoded).as_deref(), Some(expected));
        for name in [
            "%",
            "%2",
            "%GG",
            "%FF",
            "%00",
            "%252Ftmp",
            "relative",
            "file%3A%2Ftmp",
            "file%3A%2F%2F%2FC%3A%2Fproject",
            "",
        ] {
            assert_eq!(decode_cwd(name), None, "{name}");
        }
    }

    #[test]
    fn native_only_deduplicates_valid_paths_and_skips_invalid_or_stale_directories() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let cwd = root.join("中文 空格+%2F");
        fs::create_dir(&cwd).unwrap();
        let sessions = root.join("sessions");
        summary(&sessions, &cwd, &encode(&cwd), "one");
        summary(&sessions, &cwd, &encode(&cwd), "two");
        summary(
            &sessions,
            &cwd,
            &encode(&cwd).replace("%2F", "%2f"),
            "three",
        );
        let missing = root.join("missing");
        summary(&sessions, &missing, &encode(&missing), "stale");
        for invalid in ["%GG", "relative", "%FF", "%00"] {
            summary(&sessions, &cwd, invalid, "ignored");
        }
        fs::create_dir_all(sessions.join(encode(&cwd)).join("stub")).unwrap();
        let file = root.join("not-directory");
        fs::write(&file, "").unwrap();
        summary(&sessions, &file, &encode(&file), "ignored");
        // A skill directory alone never contributes a project.
        fs::create_dir_all(root.join(".grok/skills/fake-project")).unwrap();
        let found = read_sessions(&sessions, LIMITS).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, cwd);
        assert_eq!(found[0].agents, [AgentKind::Grok]);
    }

    #[test]
    fn unreadable_malformed_and_mismatched_metadata_return_sanitized_errors() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let sessions = cwd.join("sessions");
        assert!(read_sessions(&sessions, LIMITS).is_err());
        let dir = summary(&sessions, &cwd, &encode(&cwd), "one");
        for content in [
            "SECRET_SENTINEL",
            "{}",
            r#"{"info":{"id":"wrong","cwd":"/"}}"#,
        ] {
            fs::write(dir.join("summary.json"), content).unwrap();
            let error = read_sessions(&sessions, LIMITS).unwrap_err().to_string();
            assert!(error.contains("格式无效"));
            assert!(!error.contains("SECRET_SENTINEL"));
        }
        fs::remove_file(dir.join("summary.json")).unwrap();
        fs::create_dir(dir.join("summary.json")).unwrap();
        assert!(read_sessions(&sessions, LIMITS).is_err());
    }

    #[test]
    fn budgets_fail_instead_of_returning_a_partial_catalog() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let sessions = cwd.join("sessions");
        let dir = summary(&sessions, &cwd, &encode(&cwd), "one");
        for limits in [
            Limits {
                entries: 1,
                ..LIMITS
            },
            Limits { files: 0, ..LIMITS },
            Limits {
                file_bytes: 8,
                ..LIMITS
            },
            Limits {
                total_bytes: 8,
                ..LIMITS
            },
        ] {
            assert!(
                read_sessions(&sessions, limits)
                    .unwrap_err()
                    .to_string()
                    .contains("读取上限")
            );
        }
        let size = fs::metadata(dir.join("summary.json")).unwrap().len() as usize;
        summary(&sessions, &cwd, &encode(&cwd), "two");
        assert!(
            read_sessions(
                &sessions,
                Limits {
                    total_bytes: size,
                    ..LIMITS
                }
            )
            .is_err()
        );
        let junk = cwd.join("junk");
        fs::create_dir(&junk).unwrap();
        for name in ["a", "b"] {
            fs::write(junk.join(name), "ignored").unwrap();
        }
        assert!(
            read_sessions(
                &junk,
                Limits {
                    entries: 1,
                    ..LIMITS
                }
            )
            .is_err()
        );
        // Empty/irrelevant cwd buckets consume the same global entry budget.
        let empty = cwd.join("empty");
        for name in ["relative-a", "relative-b"] {
            fs::create_dir_all(empty.join(name)).unwrap();
        }
        assert!(
            read_sessions(
                &empty,
                Limits {
                    entries: 1,
                    ..LIMITS
                }
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn never_opens_chat_files_or_follows_metadata_symlinks() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let sessions = cwd.join("sessions");
        let dir = summary(&sessions, &cwd, &encode(&cwd), "one");
        // Dangling transcript symlinks would fail if the adapter opened them.
        for name in [
            "updates.jsonl",
            "chat_history.jsonl",
            "session_search.sqlite",
        ] {
            symlink(cwd.join("CHAT_MUST_NOT_BE_READ"), dir.join(name)).unwrap();
        }
        assert_eq!(read_sessions(&sessions, LIMITS).unwrap().len(), 1);
        fs::remove_file(dir.join("summary.json")).unwrap();
        symlink(cwd.join("DO_NOT_READ"), dir.join("summary.json")).unwrap();
        assert!(read_sessions(&sessions, LIMITS).is_err());
        fs::remove_file(dir.join("summary.json")).unwrap();
        // Session/bucket links are never traversed.
        symlink(&dir, sessions.join(encode(&cwd)).join("alias")).unwrap();
        symlink(sessions.join(encode(&cwd)), sessions.join("alias")).unwrap();
        assert!(read_sessions(&sessions, LIMITS).unwrap().is_empty());
    }
    fn long_fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let cwd = root.join("中文长路径".repeat(12)).join("Project Name");
        fs::create_dir_all(&cwd).unwrap();
        assert!(encode(&cwd).len() > 255);
        let sessions = root.join("sessions");
        // Like upstream decode_reads_cwd_file_for_hash_dirs: decode does not
        // recompute/validate the hash, only reads the .cwd sidecar.
        let bucket = "project-name-abcdef0123456789";
        let dir = summary(&sessions, &cwd, bucket, "one");
        let marker = sessions.join(bucket).join(".cwd");
        fs::write(&marker, cwd.to_str().unwrap()).unwrap();
        (temp, cwd, dir, marker)
    }

    #[test]
    fn long_hash_bucket_uses_trimmed_sidecar_and_agrees_with_summary() {
        let (_temp, cwd, dir, marker) = long_fixture();
        let sessions = dir.parent().unwrap().parent().unwrap();
        fs::write(&marker, format!(" \n{}\n", cwd.display())).unwrap();
        assert_eq!(read_sessions(sessions, LIMITS).unwrap()[0].path, cwd);
        // URL-encoded absolute names take precedence: even a bad .cwd is not read.
        let temp = tempfile::tempdir().unwrap();
        let short = temp.path().canonicalize().unwrap();
        let root = short.join("sessions");
        let dir = summary(&root, &short, &encode(&short), "short");
        fs::create_dir(dir.parent().unwrap().join(".cwd")).unwrap();
        assert_eq!(read_sessions(&root, LIMITS).unwrap().len(), 1);
    }

    #[test]
    fn forged_missing_invalid_or_mismatched_sidecars_never_create_projects() {
        let (_temp, cwd, dir, marker) = long_fixture();
        let sessions = dir.parent().unwrap().parent().unwrap();
        for bytes in [b"relative".as_slice(), b"\xff", b"/bad\0path", b"", b"  "] {
            fs::write(&marker, bytes).unwrap();
            assert!(read_sessions(sessions, LIMITS).is_err());
        }
        // A forged valid absolute cwd pointing elsewhere must disagree with info.cwd.
        fs::write(&marker, cwd.parent().unwrap().to_str().unwrap()).unwrap();
        assert!(read_sessions(sessions, LIMITS).is_err());
        fs::write(&marker, cwd.to_str().unwrap()).unwrap();
        fs::write(
            dir.join("summary.json"),
            serde_json::to_vec(&serde_json::json!({
                "info":{"id":"forged-id", "cwd":cwd}
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(read_sessions(sessions, LIMITS).is_err());
        fs::remove_file(&marker).unwrap();
        assert!(read_sessions(sessions, LIMITS).is_err());
    }

    #[test]
    fn sidecar_size_file_total_and_directory_entry_budgets_are_enforced() {
        let (_temp, _cwd, dir, marker) = long_fixture();
        let sessions = dir.parent().unwrap().parent().unwrap();
        let marker_size = fs::metadata(&marker).unwrap().len() as usize;
        for limits in [
            Limits {
                entries: 2,
                ..LIMITS
            },
            Limits { files: 1, ..LIMITS },
            Limits {
                total_bytes: marker_size,
                ..LIMITS
            },
        ] {
            assert!(
                read_sessions(sessions, limits)
                    .unwrap_err()
                    .to_string()
                    .contains("读取上限")
            );
        }
        // Exactly root bucket + .cwd + one session = three directory entries.
        assert_eq!(
            read_sessions(
                sessions,
                Limits {
                    entries: 3,
                    ..LIMITS
                }
            )
            .unwrap()
            .len(),
            1
        );
        OpenOptions::new()
            .write(true)
            .open(&marker)
            .unwrap()
            .set_len(MAX_CWD_BYTES as u64 + 1)
            .unwrap();
        assert!(
            read_sessions(sessions, LIMITS)
                .unwrap_err()
                .to_string()
                .contains("读取上限")
        );
    }

    #[cfg(unix)]
    #[test]
    fn sidecar_links_nonfiles_and_permission_errors_fail_closed() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let (_temp, _cwd, dir, marker) = long_fixture();
        let sessions = dir.parent().unwrap().parent().unwrap();
        let original = fs::read(&marker).unwrap();
        fs::remove_file(&marker).unwrap();
        symlink(dir.join("DO_NOT_OPEN"), &marker).unwrap();
        assert!(read_sessions(sessions, LIMITS).is_err());
        fs::remove_file(&marker).unwrap();
        fs::create_dir(&marker).unwrap();
        assert!(read_sessions(sessions, LIMITS).is_err());
        fs::remove_dir(&marker).unwrap();
        fs::write(&marker, original).unwrap();
        fs::set_permissions(&marker, fs::Permissions::from_mode(0o000)).unwrap();
        let result = read_sessions(sessions, LIMITS);
        fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();
        if unsafe { libc::geteuid() } != 0 {
            assert!(result.is_err());
        }
    }
}
