//! Author: admin. Controller-only public storage API; never starts an Agent.
#![cfg(unix)]

use kitter::{library::StorageAction, SkillLibrary};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{fs::{MetadataExt, OpenOptionsExt}, io::AsRawFd},
    path::{Component, Path, PathBuf},
};

type Checked<T> = Result<T, &'static str>;
fn stage<T, E>(value: Result<T, E>, name: &'static str) -> Checked<T> {
    value.map_err(|_| name)
}
fn require(value: bool, name: &'static str) -> Checked<()> {
    if value { Ok(()) } else { Err(name) }
}
fn sha(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    root: PathBuf,
    data: PathBuf,
    library: PathBuf,
    target: PathBuf,
    action: String,
    sources: Vec<PathBuf>,
    installations: Vec<PathBuf>,
}

fn lexical(path: &Path) -> bool {
    path.is_absolute() && !path.components().any(|c| matches!(c, Component::CurDir | Component::ParentDir))
}
fn real_owned_dir(path: &Path, root: &Path) -> Checked<()> {
    require(lexical(path) && path.starts_with(root), "directory_scope")?;
    let info = stage(fs::symlink_metadata(path), "directory_metadata")?;
    require(info.is_dir() && info.uid() == unsafe { libc::geteuid() }
        && stage(path.canonicalize(), "directory_canonical")? == path, "directory_identity")
}
fn bounded_owned(path: &Path, root: &Path) -> Checked<Vec<u8>> {
    bounded_owned_file(path, root, false)
}
fn bounded_owned_file(path: &Path, root: &Path, single_link: bool) -> Checked<Vec<u8>> {
    require(lexical(path) && path.starts_with(root), "file_scope")?;
    let parent = path.parent().ok_or("file_parent")?;
    real_owned_dir(parent, root)?;
    let mut file = stage(OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK).open(path), "file_open")?;
    let info = stage(file.metadata(), "file_metadata")?;
    require(info.is_file() && info.uid() == unsafe { libc::geteuid() }
        && info.len() <= 4 * 1024 * 1024
        && (!single_link || info.nlink() == 1), "file_bound_identity")?;
    let mut bytes = Vec::new();
    stage((&mut file).take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes), "file_read")?;
    require(bytes.len() <= 4 * 1024 * 1024, "file_read_bound")?;
    Ok(bytes)
}
fn builtin_guard(library: &Path, root: &Path) -> Checked<()> {
    real_owned_dir(library, root)?;
    let builtin = library.join(kitter::library::KITTER_SKILL_STORAGE);
    // These are the complete fixed create_dir_all/fs::write footprint of the
    // public opener. Missing nodes are allowed only under the verified chain.
    for directory in [&builtin, &builtin.join("agents"), &builtin.join("references")] {
        if !absent(directory)? {
            real_owned_dir(directory, root)?;
        }
    }
    for leaf in [builtin.join("SKILL.md"), builtin.join("agents/openai.yaml"),
                 builtin.join("references/install-cli.md")] {
        if !absent(&leaf)? {
            bounded_owned_file(&leaf, root, true)?;
        }
    }
    Ok(())
}
fn absent(path: &Path) -> Checked<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(_) => Err("absence_unknown"),
    }
}
fn config_guard(data: &Path, library: &Path, root: &Path) -> Checked<(Value, Value)> {
    real_owned_dir(data, root)?;
    real_owned_dir(library, root)?;
    require(absent(&data.join(".pleat-storage-pending.json"))?, "pending_storage_transaction")?;
    let config: Value = stage(serde_json::from_slice(&bounded_owned(&data.join("config.json"), root)?), "config_json")?;
    let registry: Value = stage(serde_json::from_slice(&bounded_owned(&data.join("registry.json"), root)?), "registry_json")?;
    require(config.get("library_dir").and_then(Value::as_str) == library.to_str(), "configured_library_exact")?;
    require(registry.get("trigger_overrides").and_then(Value::as_object)
        .is_some_and(|v| v.is_empty()), "overrides_must_be_explicit_empty")?;
    Ok((config, registry))
}

#[derive(Clone, Serialize, PartialEq, Eq)]
struct Node {
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    size: u64,
    digest: String,
}
fn tree(path: &Path, root: &Path) -> Checked<BTreeMap<PathBuf, Node>> {
    require(lexical(path) && path.starts_with(root), "source_input_scope")?;
    let canonical = stage(path.canonicalize(), "source_canonical")?;
    real_owned_dir(&canonical, root)?;
    let mut pending = vec![canonical.clone()];
    let mut nodes = BTreeMap::new();
    let mut bytes = 0u64;
    while let Some(current) = pending.pop() {
        require(nodes.len() < 4096, "snapshot_entry_bound")?;
        let info = stage(fs::symlink_metadata(&current), "snapshot_metadata")?;
        require(info.uid() == unsafe { libc::geteuid() }, "snapshot_owner")?;
        let (size, digest) = if info.is_dir() {
            for item in stage(fs::read_dir(&current), "snapshot_directory")? {
                pending.push(stage(item, "snapshot_entry")?.path());
            }
            (0, String::new())
        } else if info.file_type().is_symlink() {
            require(stage(current.canonicalize(), "snapshot_link_resolve")?.starts_with(root), "snapshot_external_link")?;
            let link = stage(fs::read_link(&current), "snapshot_link")?;
            (0, sha(link.as_os_str().as_encoded_bytes()))
        } else {
            require(info.is_file(), "snapshot_file_type")?;
            bytes += info.len();
            require(bytes <= 64 * 1024 * 1024, "snapshot_byte_bound")?;
            let content = bounded_owned(&current, root)?;
            (content.len() as u64, sha(&content))
        };
        nodes.insert(stage(current.strip_prefix(&canonical), "snapshot_relative")?.to_path_buf(),
            Node { device: info.dev(), inode: info.ino(), mode: info.mode(), uid: info.uid(), size, digest });
    }
    Ok(nodes)
}
fn links(paths: &[PathBuf], root: &Path) -> Checked<Vec<Node>> {
    paths.iter().map(|path| {
        require(lexical(path) && path.starts_with(root), "installation_scope")?;
        let info = stage(fs::symlink_metadata(path), "installation_metadata")?;
        require(info.file_type().is_symlink() && info.uid() == unsafe { libc::geteuid() }, "installation_link_owner")?;
        require(stage(path.canonicalize(), "installation_canonical")?.starts_with(root), "installation_external")?;
        let target = stage(fs::read_link(path), "installation_readlink")?;
        Ok(Node { device: info.dev(), inode: info.ino(), mode: info.mode(), uid: info.uid(), size: 0,
                  digest: sha(target.as_os_str().as_encoded_bytes()) })
    }).collect()
}

fn operate() -> Checked<()> {
    let root = PathBuf::from(std::env::var_os("PLEAT_GROK_STORAGE_ROOT").ok_or("root_required")?);
    require(root.parent() == Some(Path::new("/private/tmp"))
        && root.file_name().and_then(|s| s.to_str()).is_some_and(|s|
            s.starts_with("pleat-grok-body-") || s.starts_with("pleat-grok-managed-")), "root_prefix")?;
    real_owned_dir(&root, &root)?;
    let root_info = stage(fs::metadata(&root), "root_metadata")?;
    require(root_info.mode() & 0o077 == 0, "root_private")?;
    let input_path = PathBuf::from(std::env::var_os("PLEAT_GROK_STORAGE_MANIFEST").ok_or("manifest_required")?);
    let expected_sha = std::env::var("PLEAT_GROK_STORAGE_MANIFEST_SHA256").map_err(|_| "manifest_sha_required")?;
    require(expected_sha.len() == 64 && expected_sha.bytes().all(|c| c.is_ascii_hexdigit()), "manifest_sha_shape")?;
    let raw = bounded_owned(&input_path, &root)?;
    require(sha(&raw) == expected_sha, "manifest_sha_mismatch")?;
    let input: Input = stage(serde_json::from_slice(&raw), "manifest_json")?;
    require(input.root == root && !input.sources.is_empty() && input.sources.len() <= 4
        && !input.installations.is_empty() && input.installations.len() <= 8, "manifest_scope")?;
    require(lexical(&input.target) && input.target.starts_with(&root), "target_scope")?;
    real_owned_dir(input.target.parent().ok_or("target_parent")?, &root)?;
    require(absent(&input.target)?, "target_must_be_absent")?;
    let action = match input.action.as_str() {
        "move-library" => StorageAction::MoveLibrary,
        "move-app-data" => StorageAction::MoveAppData,
        _ => return Err("operation_not_allowed"),
    };
    require(input.installations.iter().all(|p| !p.starts_with(&input.data) && !p.starts_with(&input.library)),
            "installation_uri_must_be_external_to_moved_trees")?;
    let lock_path = root.join(".grok-storage-driver.lock");
    let lock = stage(OpenOptions::new().read(true).write(true).create(true)
        .mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(lock_path), "driver_lock_open")?;
    let lock_info = stage(lock.metadata(), "driver_lock_metadata")?;
    require(lock_info.is_file() && lock_info.uid() == unsafe { libc::geteuid() }
        && lock_info.mode() & 0o077 == 0, "driver_lock_identity")?;
    require(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0, "driver_lock_busy")?;
    // The task controller holds the native slot and owns this unshared fixture.
    // This gate runs BEFORE the public opener can perform any startup writes.
    config_guard(&input.data, &input.library, &root)?;
    builtin_guard(&input.library, &root)?;
    let before_sources = input.sources.iter().map(|p| tree(p, &root)).collect::<Checked<Vec<_>>>()?;
    let before_links = links(&input.installations, &root)?;
    let from = if action == StorageAction::MoveLibrary { &input.library } else { &input.data };
    let source_identity = stage(fs::metadata(from), "moving_root_identity")?;
    let mut library = stage(SkillLibrary::open_in(&input.data), "public_open")?;
    require(library.config.library_dir == input.library, "opened_library_mismatch")?;
    let (before_config, before_registry) = config_guard(&input.data, &input.library, &root)?;
    let plan = stage(library.plan_storage_change(action, &input.target), "public_plan")?;
    require(plan.from == *from && plan.to == input.target && plan.compatibility_alias, "plan_identity")?;
    let (preview_config, preview_registry) = config_guard(&input.data, &input.library, &root)?;
    require(before_config == preview_config && before_registry == preview_registry && absent(&input.target)?, "preview_changed_state")?;
    require(input.sources.iter().map(|p| tree(p, &root)).collect::<Checked<Vec<_>>>()? == before_sources
        && links(&input.installations, &root)? == before_links, "preview_changed_payload")?;
    stage(library.apply_storage_change(&plan), "public_apply")?;
    let current_data = if action == StorageAction::MoveAppData { input.target.clone() } else { input.data.clone() };
    let current_library = library.config.library_dir.clone();
    let moved = stage(fs::metadata(&input.target), "moved_root_metadata")?;
    require(moved.dev() == source_identity.dev() && moved.ino() == source_identity.ino()
        && stage(fs::symlink_metadata(from), "compatibility_alias_metadata")?.file_type().is_symlink()
        && stage(from.canonicalize(), "compatibility_alias_canonical")? == input.target, "move_alias_identity")?;
    let (_, after_registry) = config_guard(&current_data, &current_library, &root)?;
    require(after_registry == before_registry, "registry_semantic_changed")?;
    builtin_guard(&current_library, &root)?;
    let reopened = stage(SkillLibrary::open_in(&current_data), "public_reopen")?;
    require(reopened.data_dir() == current_data && reopened.config.library_dir == current_library, "reopen_context")?;
    require(input.sources.iter().map(|p| tree(p, &root)).collect::<Checked<Vec<_>>>()? == before_sources
        && links(&input.installations, &root)? == before_links, "source_local_data_or_installation_changed")?;
    config_guard(&current_data, &current_library, &root)?;
    let report = json!({"author":"admin", "last_modified_by":"admin", "status":"PASS_PUBLIC_STORAGE_API_FS_ONLY",
        "action":input.action, "from":from, "to":input.target, "current_data":current_data,
        "current_library":current_library, "preview_did_not_change_state":true,
        "source_payload_and_local_data_identity_preserved":true, "installation_uri_and_inode_preserved":true,
        "compatibility_alias_identity_verified":true, "registry_semantics_preserved":true,
        "trigger_overrides_explicit_empty_preserved":true, "source_trigger_writer_called":false,
        "native_agent_run":false, "native_body_or_ADP_verified":false, "full_17_complete":false,
        "source_tree_snapshots":before_sources, "installation_snapshots":before_links,
        "recovery_materials_retained":true});
    let output = root.join(format!("grok-storage-{}.json", input.action));
    let mut file: File = stage(OpenOptions::new().write(true).create_new(true).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(output), "report_create_new")?;
    stage(file.write_all(&stage(serde_json::to_vec_pretty(&report), "report_json")?), "report_write")?;
    Ok(())
}

#[test]
#[ignore = "main controller's private fixture and reviewed explicit manifest are required"]
fn operate_owned_grok_storage() {
    if let Err(fixed_stage) = operate() {
        panic!("controlled_grok_storage_failed: {fixed_stage}");
    }
    println!("GROK_STORAGE_DRIVER_PASS_FS_ONLY");
}

fn guard_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temporary = tempfile::Builder::new().prefix("pleat-grok-storage-guard-")
        .tempdir().expect("owned fixture creation");
    let root = temporary.path().canonicalize().expect("owned fixture canonical");
    let library = root.join("library");
    fs::create_dir(&library).expect("owned library creation");
    (temporary, root, library)
}

#[test]
fn storage_guard_rejects_builtin_directory_link() {
    let (_temporary, root, library) = guard_fixture();
    let external = tempfile::Builder::new().prefix("pleat-grok-storage-sentinel-")
        .tempdir().expect("owned external fixture");
    let sentinel = external.path().join("sentinel");
    fs::write(&sentinel, b"owned-sentinel").expect("owned sentinel");
    std::os::unix::fs::symlink(external.path(), library.join(kitter::library::KITTER_SKILL_STORAGE))
        .expect("fixture link");
    assert!(builtin_guard(&library, &root).is_err());
    assert!(fs::read(&sentinel).expect("sentinel read") == b"owned-sentinel");
    assert!(!external.path().join("SKILL.md").exists());
}

#[test]
fn storage_guard_rejects_builtin_leaf_link() {
    let (_temporary, root, library) = guard_fixture();
    let builtin = library.join(kitter::library::KITTER_SKILL_STORAGE);
    fs::create_dir(&builtin).expect("fixture builtin");
    let external = tempfile::Builder::new().prefix("pleat-grok-storage-sentinel-")
        .tempdir().expect("owned external fixture");
    let sentinel = external.path().join("sentinel");
    fs::write(&sentinel, b"owned-sentinel").expect("owned sentinel");
    std::os::unix::fs::symlink(&sentinel, builtin.join("SKILL.md")).expect("fixture link");
    assert!(builtin_guard(&library, &root).is_err());
    assert!(fs::read(&sentinel).expect("sentinel read") == b"owned-sentinel");
}

#[test]
fn storage_guard_rejects_builtin_hard_link() {
    let (_temporary, root, library) = guard_fixture();
    let builtin = library.join(kitter::library::KITTER_SKILL_STORAGE);
    fs::create_dir(&builtin).expect("fixture builtin");
    let external = tempfile::Builder::new().prefix("pleat-grok-storage-sentinel-")
        .tempdir().expect("owned external fixture");
    let sentinel = external.path().join("sentinel");
    fs::write(&sentinel, b"owned-sentinel").expect("owned sentinel");
    fs::hard_link(&sentinel, builtin.join("SKILL.md")).expect("fixture hard link");
    assert!(builtin_guard(&library, &root).is_err());
    assert!(fs::read(&sentinel).expect("sentinel read") == b"owned-sentinel");
}

#[test]
fn storage_guard_accepts_owned_builtin_tree() {
    let (_temporary, root, library) = guard_fixture();
    assert!(builtin_guard(&library, &root).is_ok());
    let builtin = library.join(kitter::library::KITTER_SKILL_STORAGE);
    fs::create_dir_all(builtin.join("agents")).expect("fixture agents");
    fs::create_dir_all(builtin.join("references")).expect("fixture references");
    for file in [builtin.join("SKILL.md"), builtin.join("agents/openai.yaml"),
                 builtin.join("references/install-cli.md")] {
        fs::write(file, b"owned-fixture-file").expect("fixture file");
    }
    assert!(builtin_guard(&library, &root).is_ok());
}

#[test]
fn storage_guard_rejects_fifo_without_waiting_for_writer() {
    use std::os::unix::ffi::OsStrExt;
    let (_temporary, root, _library) = guard_fixture();
    let fifo = root.join("unopened-fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("fixture fifo path");
    assert!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) } == 0);
    let started = std::time::Instant::now();
    assert!(bounded_owned(&fifo, &root).is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    let regular = root.join("regular");
    fs::write(&regular, b"owned-regular").expect("fixture regular");
    assert!(bounded_owned(&regular, &root).expect("bounded regular") == b"owned-regular");
}

#[test]
fn storage_guard_rejects_outside_source_input_before_snapshot() {
    let (_temporary, root, _library) = guard_fixture();
    assert!(matches!(tree(Path::new("/Users/imac"), &root), Err("source_input_scope")));
}
