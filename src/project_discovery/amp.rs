//! Amp 0.0.1792368068-g91990d local runner registrations, never threads.
//! Native evidence: docs/agent-compatibility/native-guides/amp-native-runner-adapter.json.
//! Register in the parent/native catalogs there; every Err must retain the old
//! Amp source. Runner absence or stopped execution is not an archive signal.
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow};
use serde::Deserialize;

use super::{AgentKind, ProjectEntry};

const MAX_PATH_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy)]
struct Limits {
    entries: usize,
    files: usize,
    file_bytes: usize,
    total_bytes: usize,
}
const LIMITS: Limits = Limits {
    entries: 10_000,
    files: 10_000,
    file_bytes: 64 * 1024,
    total_bytes: 8 * 1024 * 1024,
};

fn failure() -> anyhow::Error {
    anyhow!("Amp 原生工作区来源不可读或状态未知")
}
fn charge(used: &mut usize, amount: usize, limit: usize) -> Result<()> {
    *used = used.checked_add(amount).ok_or_else(failure)?;
    if *used > limit {
        return Err(failure());
    }
    Ok(())
}

pub(super) fn projects(home: &Path) -> Result<Vec<ProjectEntry>> {
    // Override names occur in the binary, but their storage semantics have not
    // been closed. Do not silently read a default profile in their presence.
    let root = storage_root(
        home,
        std::env::consts::OS,
        std::env::var_os("XDG_DATA_HOME").as_deref(),
        std::env::var_os("AMP_STORAGE_BASE").is_some(),
        std::env::var_os("AMP_HOME").is_some(),
    )?;
    projects_at(&root.join("runner"))
}

fn storage_root(
    home: &Path,
    os: &str,
    xdg: Option<&OsStr>,
    storage_override: bool,
    home_override: bool,
) -> Result<PathBuf> {
    if storage_override || home_override || !home.is_absolute() {
        return Err(failure());
    }
    // Pinned initializer: win32/darwin ignore XDG_DATA_HOME; Linux uses
    // XDG_DATA_HOME || HOME/.local/share. Never expand relative roots or '~'.
    let base = match os {
        "macos" | "windows" => home.join(".local/share"),
        "linux" => xdg
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share")),
        _ => return Err(failure()),
    };
    let root = base.join("amp");
    if !root.is_absolute()
        || root.as_os_str().as_encoded_bytes().len() > MAX_PATH_BYTES
        || root.as_os_str().as_encoded_bytes().contains(&0)
    {
        return Err(failure());
    }
    Ok(root)
}

// These fields are the observed persisted shape, not runner list --json's
// object-valued directories. Ignore thread container contents without guessing
// their element/value types or extracting any cwd from them.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Registration {
    runner_id: String,
    working_directory: String,
    directories: Vec<String>,
    #[serde(rename = "servedThreadIds")]
    _served_thread_ids: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "servedThreadWorkingDirectories")]
    _served_thread_working_directories: BTreeMap<String, serde::de::IgnoredAny>,
    #[serde(rename = "sleepingThreadIds")]
    _sleeping_thread_ids: Vec<serde::de::IgnoredAny>,
    #[serde(rename = "controlSocketPath", deserialize_with = "opaque_socket")]
    _control_socket_path: (),
}

// The pinned writer uses null for TUI registrations and a string for no-TUI.
// Validate either shape without retaining or following the socket.
fn opaque_socket<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<(), D::Error> {
    struct Opaque;
    impl<'de> serde::de::Visitor<'de> for Opaque {
        type Value = ();
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a string or null")
        }
        fn visit_str<E: serde::de::Error>(self, _: &str) -> std::result::Result<(), E> {
            Ok(())
        }
        fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<(), E> {
            Ok(())
        }
    }
    deserializer.deserialize_any(Opaque)
}

fn projects_at(root: &Path) -> Result<Vec<ProjectEntry>> {
    read_projects(root, LIMITS)
}

#[cfg(not(unix))]
fn read_projects(_: &Path, _: Limits) -> Result<Vec<ProjectEntry>> {
    // Directory's anchored NOFOLLOW operations are Unix-only. Fail closed on
    // other platforms until an equivalent reparse-safe reader is available.
    Err(failure())
}

#[cfg(unix)]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.is_file()
        && b.is_file()
        && a.nlink() == 1
        && b.nlink() == 1
        && a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}

#[cfg(unix)]
fn names(directory: &crate::package_fs::Directory, limit: usize) -> Result<Vec<PathBuf>> {
    let mut stream = rustix::fs::Dir::read_from(directory).map_err(|_| failure())?;
    let mut seen = 0;
    let mut names = Vec::new();
    while let Some(entry) = stream.read() {
        let entry = entry.map_err(|_| failure())?;
        let bytes = entry.file_name().to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        // Charge every entry, including sockets, subdirectories and junk. Never
        // descend, inspect file contents by title, or open non-JSON entries.
        charge(&mut seen, 1, limit)?;
        if !bytes.ends_with(b".json") {
            continue;
        }
        let name = std::str::from_utf8(bytes).map_err(|_| failure())?;
        if name == ".json" || name.len() > MAX_PATH_BYTES {
            return Err(failure());
        }
        names.push(PathBuf::from(name));
    }
    names.sort();
    Ok(names)
}

#[cfg(unix)]
fn read_projects(root: &Path, limits: Limits) -> Result<Vec<ProjectEntry>> {
    use crate::package_fs::Directory;
    use std::{io::Read, os::unix::fs::MetadataExt, sync::Arc};

    if !root.is_absolute() || root.as_os_str().as_encoded_bytes().len() > MAX_PATH_BYTES {
        return Err(failure());
    }
    let directory = Directory::open_anchored(root).map_err(|_| failure())?;
    #[cfg(test)]
    run_hook(&AFTER_ROOT_OPEN);
    directory.verify_location(root).map_err(|_| failure())?;
    let registrations = names(&directory, limits.entries)?;
    if registrations.is_empty() || registrations.len() > limits.files {
        return Err(failure());
    }
    // Canonicalize only the already verified metadata directory. All payload
    // opens below remain relative to its pinned descriptor.
    let identity_root = std::fs::canonicalize(root).map_err(|_| failure())?;
    directory
        .verify_location(&identity_root)
        .map_err(|_| failure())?;
    let mut total_bytes = 0;
    let mut snapshots = Vec::new();
    let mut projects: BTreeMap<PathBuf, ProjectEntry> = BTreeMap::new();
    for name in &registrations {
        if identity_root
            .join(name)
            .as_os_str()
            .as_encoded_bytes()
            .len()
            > MAX_PATH_BYTES
        {
            return Err(failure());
        }
        directory.verify_location(root).map_err(|_| failure())?;
        let mut file = directory.read(name).map_err(|_| failure())?;
        let before = file.metadata().map_err(|_| failure())?;
        if !before.is_file() || before.nlink() != 1 {
            return Err(failure());
        }
        #[cfg(test)]
        run_hook(&AFTER_FILE_OPEN);
        // Reject a replacement before reading as well as after reading.
        directory.verify_location(root).map_err(|_| failure())?;
        let visible = directory
            .read(name)
            .and_then(|file| Ok(file.metadata()?))
            .map_err(|_| failure())?;
        if !same_file(&before, &visible) {
            return Err(failure());
        }
        let max = limits
            .file_bytes
            .min(limits.total_bytes.saturating_sub(total_bytes));
        if before.len() > max as u64 {
            return Err(failure());
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(max as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| failure())?;
        if bytes.len() > max || bytes.len() as u64 != before.len() {
            return Err(failure());
        }
        charge(&mut total_bytes, bytes.len(), limits.total_bytes)?;
        if !same_file(&before, &file.metadata().map_err(|_| failure())?) {
            return Err(failure());
        }
        let registration: Registration = serde_json::from_slice(&bytes).map_err(|_| failure())?;
        if registration.runner_id.trim().is_empty()
            || registration.runner_id.len() > MAX_PATH_BYTES
            || registration.runner_id.chars().any(char::is_control)
        {
            return Err(failure());
        }
        let mut members = BTreeMap::new();
        for cwd in std::iter::once(&registration.working_directory).chain(&registration.directories)
        {
            if cwd.len() > MAX_PATH_BYTES || cwd.contains('\0') {
                return Err(failure());
            }
            // UNC/network paths and repository-relative or remote URI values
            // cannot establish a local project. Empty cwd is unknown, not empty.
            if cwd.is_empty() {
                return Err(failure());
            }
            if cwd.starts_with("//") || cwd.starts_with("\\\\") || !Path::new(cwd).is_absolute() {
                continue;
            }
            let path = match std::fs::canonicalize(cwd) {
                Ok(path) => path,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(failure()),
            };
            if !std::fs::metadata(&path).map_err(|_| failure())?.is_dir() {
                continue;
            }
            if path.as_os_str().as_encoded_bytes().len() > MAX_PATH_BYTES {
                return Err(failure());
            }
            let entry = super::native::project(path.to_str().ok_or_else(failure)?, AgentKind::Amp)
                .ok_or_else(failure)?;
            members.entry(entry.path.clone()).or_insert(entry);
        }
        // No valid roots cannot authoritatively clear a prior runner's projects.
        if members.is_empty() {
            return Err(failure());
        }
        let workspace = super::NativeWorkspace {
            agent: AgentKind::Amp,
            source_id: Some(registration.runner_id),
            display_name: None,
            archived: None,
            path: identity_root.join(name),
            project_roots: Arc::new(members.keys().cloned().collect()),
        };
        for (_, mut entry) in members {
            if let Some(old) = projects.get_mut(&entry.path) {
                old.native_workspaces.push(workspace.clone());
            } else {
                entry.native_workspaces.push(workspace.clone());
                projects.insert(entry.path.clone(), entry);
            }
        }
        snapshots.push((name, before));
    }
    // Revalidate every file, not just the last one, and the whole visible parent
    // chain. No partial catalog escapes if a source changes during the scan.
    directory.verify_location(root).map_err(|_| failure())?;
    if names(&directory, limits.entries)? != registrations {
        return Err(failure());
    }
    for (name, before) in snapshots {
        let after = directory
            .read(name)
            .and_then(|file| Ok(file.metadata()?))
            .map_err(|_| failure())?;
        if !same_file(&before, &after) {
            return Err(failure());
        }
    }
    directory.verify_location(root).map_err(|_| failure())?;
    Ok(projects.into_values().collect())
}

#[cfg(all(test, unix))]
thread_local! {
    static AFTER_ROOT_OPEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
    static AFTER_FILE_OPEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
}
#[cfg(all(test, unix))]
fn run_hook(hook: &'static std::thread::LocalKey<std::cell::RefCell<Option<Box<dyn FnOnce()>>>>) {
    hook.with(|slot| {
        if let Some(callback) = slot.borrow_mut().take() {
            callback();
        }
    });
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::{fs, os::unix::fs::symlink, sync::Arc};

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let runner = root.join("amp/runner");
        let a = root.join("project-A");
        let b = root.join("项目-B");
        for dir in [&runner, &a, &b] {
            fs::create_dir_all(dir).unwrap();
        }
        (temp, runner, a, b)
    }
    fn metadata(id: &str, a: &Path, b: &Path) -> Value {
        json!({"runnerId": id, "workingDirectory": a, "directories": [b],
            "servedThreadIds": [], "servedThreadWorkingDirectories": {},
            "sleepingThreadIds": [], "controlSocketPath": "opaque-not-to-open"})
    }
    fn write(root: &Path, name: &str, value: &Value) {
        fs::write(root.join(name), serde_json::to_vec(value).unwrap()).unwrap();
    }
    fn unknown(result: Result<Vec<ProjectEntry>>) {
        assert_eq!(
            result.unwrap_err().to_string(),
            "Amp 原生工作区来源不可读或状态未知"
        );
    }

    #[test]
    fn profile_selection_is_only_the_proven_initializer() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let xdg = home.join("xdg");
        for os in ["macos", "windows", "linux"] {
            assert_eq!(
                storage_root(home, os, None, false, false).unwrap(),
                home.join(".local/share/amp")
            );
            let expected = if os == "linux" {
                xdg.join("amp")
            } else {
                home.join(".local/share/amp")
            };
            assert_eq!(
                storage_root(home, os, Some(xdg.as_os_str()), false, false).unwrap(),
                expected
            );
            assert!(storage_root(home, os, None, true, false).is_err());
            assert!(storage_root(home, os, None, false, true).is_err());
        }
        assert_eq!(
            storage_root(home, "linux", Some(OsStr::new("")), false, false).unwrap(),
            home.join(".local/share/amp")
        );
        assert!(storage_root(home, "linux", Some(OsStr::new("relative")), false, false).is_err());
        assert!(storage_root(home, "unknown", None, false, false).is_err());
        assert!(storage_root(Path::new("relative"), "macos", None, false, false).is_err());
    }

    #[test]
    fn real_shape_two_roots_alias_dedup_shared_members_and_read_only() {
        let (_temp, root, a, b) = fixture();
        let alias = a.with_file_name("alias");
        symlink(&a, &alias).unwrap();
        let mut value = metadata("native-runner-one", &a, &b);
        value["directories"] = json!([b, a, alias, b]);
        write(&root, "116f523ca6ce35ef.json", &value);
        let source = root.join("116f523ca6ce35ef.json");
        let before = fs::read(&source).unwrap();
        let found = projects_at(&root).unwrap();
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|p| p.path == a));
        assert!(found.iter().any(|p| p.path == b));
        let first = &found[0].native_workspaces[0];
        assert_eq!(first.source_id.as_deref(), Some("native-runner-one"));
        assert_eq!(first.path, source);
        assert_eq!(first.project_roots.len(), 2);
        assert!(Arc::ptr_eq(
            &first.project_roots,
            &found[1].native_workspaces[0].project_roots
        ));
        assert!(found.iter().all(|p| p.agents == [AgentKind::Amp]
            && !p.archived
            && p.archive_states.is_empty()
            && p.native_workspaces.iter().all(|w| w.archived.is_none())));
        assert_eq!(fs::read(&source).unwrap(), before);
        assert_eq!(
            first.id(),
            projects_at(&root).unwrap()[0].native_workspaces[0].id()
        );
        let mut changed = first.clone();
        changed.source_id = Some("another-native-runner".into());
        assert_ne!(first.id(), changed.id());
        changed = first.clone();
        changed.path = root.join("other.json");
        assert_ne!(first.id(), changed.id());
        write(&root, "other.json", &metadata("native-runner-two", &a, &a));
        let merged = projects_at(&root).unwrap();
        assert_eq!(merged.len(), 2);
        assert_eq!(
            merged
                .iter()
                .find(|p| p.path == a)
                .unwrap()
                .native_workspaces
                .len(),
            2
        );
    }

    #[test]
    fn only_cwd_registration_fields_can_create_projects() {
        let (_temp, root, a, b) = fixture();
        let mut value = metadata("native-id", &a, &b);
        value["workingDirectory"] = json!("relative/project");
        value["directories"] = json!([
            "https://remote/project",
            "file:///tmp/remote",
            "//server/share",
            "\\\\server\\share",
            a.join("missing"),
            a
        ]);
        // Keep the only natively witnessed empty map; no populated map fixture
        // or inferred value/cwd contract is introduced here.
        write(&root, "one.json", &value);
        let found = projects_at(&root).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, a);
        value["directories"] = json!([]);
        write(&root, "one.json", &value);
        unknown(projects_at(&root));
        value["directories"] = json!([root.join("one.json")]);
        write(&root, "one.json", &value);
        unknown(projects_at(&root));
        value["title"] = json!(b);
        value["chat"] = json!({"cwd": b});
        write(&root, "one.json", &value);
        unknown(projects_at(&root));
        value = metadata("native-id", &a, &b);
        value["workingDirectory"] = json!("/invalid\0path");
        write(&root, "one.json", &value);
        unknown(projects_at(&root));
    }

    #[test]
    fn tui_registration_with_null_control_socket_keeps_real_cwd_read_only() {
        let (_temp, root, a, b) = fixture();
        let mut value = metadata("native-tui-id", &a, &b);
        value["directories"] = json!([]);
        value["controlSocketPath"] = Value::Null;
        write(&root, "116f523ca6ce35ef.json", &value);
        let source = root.join("116f523ca6ce35ef.json");
        let before = fs::read(&source).unwrap();
        let found = projects_at(&root).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, a);
        assert_eq!(found[0].agents, [AgentKind::Amp]);
        assert_eq!(
            found[0].native_workspaces[0].source_id.as_deref(),
            Some("native-tui-id")
        );
        assert_eq!(fs::read(source).unwrap(), before);
    }

    #[test]
    fn absent_corrupt_empty_unknown_and_wrong_types_fail_without_partial_catalog() {
        let (_temp, root, a, b) = fixture();
        unknown(projects_at(&root.join("missing")));
        unknown(projects_at(&root));
        fs::write(root.join("irrelevant"), "ignored").unwrap();
        unknown(projects_at(&root));
        write(&root, "good.json", &metadata("native-id", &a, &b));
        for bad in [json!({}), json!([]), json!(null)] {
            write(&root, "bad.json", &bad);
            unknown(projects_at(&root));
        }
        for (key, bad) in [
            ("runnerId", json!(null)),
            ("runnerId", json!("")),
            ("workingDirectory", json!(null)),
            ("workingDirectory", json!("")),
            ("directories", json!({})),
            ("directories", json!([{"path": a}])),
            ("servedThreadIds", json!({})),
            ("servedThreadWorkingDirectories", json!([])),
            ("sleepingThreadIds", json!(null)),
            ("controlSocketPath", json!({})),
            ("controlSocketPath", json!([])),
            ("controlSocketPath", json!(true)),
            ("controlSocketPath", json!(1)),
            ("futureUnverifiedSchema", json!(true)),
        ] {
            let mut bad_value = metadata("native-id", &a, &b);
            bad_value[key] = bad;
            write(&root, "bad.json", &bad_value);
            unknown(projects_at(&root));
        }
        for key in [
            "runnerId",
            "workingDirectory",
            "directories",
            "controlSocketPath",
        ] {
            let mut value = metadata("native-id", &a, &b);
            value.as_object_mut().unwrap().remove(key);
            write(&root, "bad.json", &value);
            unknown(projects_at(&root));
        }
        for bytes in [
            b"not-json-private-content".as_slice(),
            b"",
            b"{\"runnerId\":\"one\",\"runnerId\":\"two\"}",
        ] {
            fs::write(root.join("bad.json"), bytes).unwrap();
            unknown(projects_at(&root));
        }
    }

    #[test]
    fn entry_file_byte_aggregate_path_and_depth_budgets() {
        let (_temp, root, a, b) = fixture();
        let value = metadata("native-id", &a, &b);
        write(&root, "one.json", &value);
        let size = fs::metadata(root.join("one.json")).unwrap().len() as usize;
        assert!(
            read_projects(
                &root,
                Limits {
                    entries: 1,
                    files: 1,
                    file_bytes: size,
                    total_bytes: size
                }
            )
            .is_ok()
        );
        for limits in [
            Limits {
                file_bytes: size - 1,
                ..LIMITS
            },
            Limits {
                total_bytes: size - 1,
                ..LIMITS
            },
            Limits { files: 0, ..LIMITS },
            Limits {
                entries: 0,
                ..LIMITS
            },
        ] {
            unknown(read_projects(&root, limits));
        }
        fs::write(root.join("junk"), "ignored").unwrap();
        unknown(read_projects(
            &root,
            Limits {
                entries: 1,
                ..LIMITS
            },
        ));
        fs::create_dir(root.join("nested")).unwrap();
        fs::write(root.join("nested/chat.json"), "must-not-read").unwrap();
        assert_eq!(projects_at(&root).unwrap().len(), 2);
        write(&root, "two.json", &value);
        unknown(read_projects(
            &root,
            Limits {
                total_bytes: size * 2 - 1,
                ..LIMITS
            },
        ));
        let mut long = value;
        long["workingDirectory"] = json!(format!("/{}", "a".repeat(MAX_PATH_BYTES)));
        write(&root, "two.json", &long);
        unknown(projects_at(&root));
        fs::write(root.join("two.json"), vec![b' '; LIMITS.file_bytes + 1]).unwrap();
        unknown(projects_at(&root));
    }

    #[test]
    fn nofollow_regular_single_link_and_unreadable_sources() {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, root, a, b) = fixture();
        let outside = a.join("metadata");
        fs::write(
            &outside,
            serde_json::to_vec(&metadata("native-id", &a, &b)).unwrap(),
        )
        .unwrap();
        let source = root.join("one.json");
        symlink(&outside, &source).unwrap();
        unknown(projects_at(&root));
        fs::remove_file(&source).unwrap();
        fs::hard_link(&outside, &source).unwrap();
        unknown(projects_at(&root));
        fs::remove_file(&source).unwrap();
        fs::create_dir(&source).unwrap();
        unknown(projects_at(&root));
        fs::remove_dir(&source).unwrap();
        let source_c = std::ffi::CString::new(source.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(source_c.as_ptr(), 0o600) }, 0);
        unknown(projects_at(&root));
        fs::remove_file(&source).unwrap();
        write(&root, "one.json", &metadata("native-id", &a, &b));
        fs::set_permissions(&source, fs::Permissions::from_mode(0)).unwrap();
        if unsafe { libc::geteuid() } != 0 {
            unknown(projects_at(&root));
        }
        fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
        let alias = a.join("alias");
        symlink(&root, &alias).unwrap();
        unknown(projects_at(&alias));
        let parent_alias = a.join("parent-alias");
        symlink(root.parent().unwrap(), &parent_alias).unwrap();
        unknown(projects_at(&parent_alias.join("runner")));
    }

    #[test]
    fn parent_and_file_replacements_fail_closed() {
        for (file_swap, link_swap) in [(false, true), (false, false), (true, true), (true, false)] {
            let (_temp, root, a, b) = fixture();
            write(&root, "one.json", &metadata("native-id", &a, &b));
            let replacement = a.join("replacement");
            fs::create_dir(&replacement).unwrap();
            write(
                &replacement,
                "one.json",
                &metadata("other-native-id", &b, &b),
            );
            let hook: Box<dyn FnOnce()> = if file_swap {
                let root = root.clone();
                let replacement = replacement.clone();
                Box::new(move || {
                    fs::rename(root.join("one.json"), root.join("old")).unwrap();
                    if link_swap {
                        symlink(replacement.join("one.json"), root.join("one.json")).unwrap();
                    } else {
                        fs::copy(replacement.join("one.json"), root.join("one.json")).unwrap();
                    }
                })
            } else {
                let parent = root.parent().unwrap().to_path_buf();
                Box::new(move || {
                    let moved = parent.with_extension("moved");
                    fs::rename(&parent, moved).unwrap();
                    if link_swap {
                        symlink(replacement.parent().unwrap(), &parent).unwrap();
                    } else {
                        fs::create_dir_all(parent.join("runner")).unwrap();
                        fs::copy(replacement.join("one.json"), parent.join("runner/one.json"))
                            .unwrap();
                    }
                })
            };
            if file_swap {
                AFTER_FILE_OPEN.with(|slot| *slot.borrow_mut() = Some(hook));
            } else {
                AFTER_ROOT_OPEN.with(|slot| *slot.borrow_mut() = Some(hook));
            }
            unknown(projects_at(&root));
        }
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn unsupported_anchored_reader_rejects_valid_registration_without_changes() {
        let temp = tempfile::tempdir().unwrap();
        let metadata = temp.path().join("native.json");
        let bytes = serde_json::to_vec(&serde_json::json!({
            "runnerId":"native", "workingDirectory":temp.path(), "directories":[temp.path()],
            "servedThreadIds":[], "servedThreadWorkingDirectories":{},
            "sleepingThreadIds":[], "controlSocketPath":"opaque-not-to-open"
        })).unwrap();
        std::fs::write(&metadata, &bytes).unwrap();
        assert!(projects_at(temp.path()).is_err());
        assert_eq!(std::fs::read(metadata).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }
}
