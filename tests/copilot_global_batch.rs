//! admin: public product batch contract in the controller's owned global home.
#![cfg(unix)]
use std::{fs, path::PathBuf, os::unix::fs::MetadataExt};
use kitter::{SkillLibrary, InstallTarget, project};

#[test]
#[ignore = "requires the controller's isolated Copilot GUI fixture and literal HOME"]
fn owned_global_conflict_only_failed_retry_preserves_success_and_sources() {
    let root = PathBuf::from(std::env::var_os("PLEAT_COPILOT_GLOBAL_FIXTURE").unwrap())
        .canonicalize().unwrap();
    assert!(root.to_string_lossy().starts_with("/private/var/tmp/pleat-copilot-ui-"));
    let home = root.join("home");
    assert_eq!(std::env::var_os("HOME").unwrap(), home.as_os_str());
    let native_root = home.join(".copilot");
    assert_eq!(std::env::var_os("COPILOT_HOME").unwrap(), native_root.as_os_str());
    let names = ["pleat-copilot-ui-first", "pleat-copilot-ui-second"];
    let library = SkillLibrary::open_in(root.join("manager")).unwrap();
    let skills = names.into_iter().map(|name| {
        let source = library.skill_path(name).unwrap().canonicalize().unwrap();
        assert!(source.starts_with(root.join("sources")));
        (name.to_owned(), source)
    }).collect::<Vec<_>>();
    fs::create_dir_all(native_root.join("skills")).unwrap();
    let instructions = native_root.join("copilot-instructions.md");
    if instructions.exists() { assert_eq!(fs::read(&instructions).unwrap(), b"OWNED_GLOBAL_USER_SENTINEL\r\n"); }
    else { fs::write(&instructions, b"OWNED_GLOBAL_USER_SENTINEL\r\n").unwrap(); }
    let conflict = native_root.join("skills").join(names[1]);
    let mut blocker = fs::OpenOptions::new().write(true).create_new(true).open(&conflict).unwrap();
    std::io::Write::write_all(&mut blocker, b"OWNED_GLOBAL_CONFLICT\n").unwrap();
    drop(blocker);
    let before = fs::metadata(&conflict).unwrap();
    let result = project::install_batch(&[home.clone()], &skills, &[InstallTarget::Copilot]);
    assert_eq!((result.installed, result.failures.len()), (1, 1), "{:?}", result.failures);
    let successful = native_root.join("skills").join(names[0]);
    let identity = fs::symlink_metadata(&successful).unwrap();
    let link = fs::read_link(&successful).unwrap();
    assert_eq!(fs::read(&conflict).unwrap(), b"OWNED_GLOBAL_CONFLICT\n");
    let retained = root.join("retained-global-conflict");
    assert!(!retained.exists());
    fs::rename(&conflict, &retained).unwrap();
    let retry = project::install_batch(&[home.clone()], &skills[1..2], &[InstallTarget::Copilot]);
    assert_eq!((retry.installed, retry.failures.len()), (1, 0), "{:?}", retry.failures);
    let after = fs::symlink_metadata(&successful).unwrap();
    assert_eq!((after.dev(), after.ino(), after.mtime(), after.mtime_nsec()),
        (identity.dev(), identity.ino(), identity.mtime(), identity.mtime_nsec()));
    assert_eq!(fs::read_link(&successful).unwrap(), link);
    assert_eq!(fs::metadata(&retained).unwrap().ino(), before.ino());
    assert_eq!(fs::read(&retained).unwrap(), b"OWNED_GLOBAL_CONFLICT\n");
    for name in names { project::uninstall(&home, &library.config.library_dir, name, &[InstallTarget::Copilot]).unwrap(); }
    for name in names { assert!(native_root.join("skills").join(name).symlink_metadata().is_err()); }
    assert_eq!(fs::read(&instructions).unwrap(), b"OWNED_GLOBAL_USER_SENTINEL\r\n");
    let report = serde_json::json!({"author":"admin","last_modified_by":"admin",
        "status":"PASS_PUBLIC_PRODUCT_GLOBAL_CONFLICT_RETRY_REMOVE","scope":"public product API; no native SDK",
        "initial_success":1,"initial_failed":1,"retry_items":1,"final_success":2,"final_failed":0,
        "successful_link_identity_preserved":true,"conflict_bytes_inode_preserved":true,
        "uninstalled":2,"user_instruction_bytes_preserved":true,"native_SDK_started":false,
        "source_trigger_writer_executed":false});
    fs::write(root.join("public-global-conflict-proof.json"), serde_json::to_vec_pretty(&report).unwrap()).unwrap();
}
