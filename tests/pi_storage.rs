//! Controller-only library operations for exclusive native host fixtures.
#![cfg(unix)]
use kitter::{SkillLibrary, SkillOrigin, SkillRecord, library::StorageAction};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

fn id(path: &Path) -> String {
    let meta = path.metadata().unwrap();
    format!("{}:{}", meta.dev(), meta.ino())
}

#[test]
#[ignore = "controller serializes the exclusive Pi native fixture and its host"]
fn operate_isolated_native_pi_managed_library() {
    operate("pi");
}

#[test]
#[ignore = "controller serializes the exclusive OMP native fixture and its host"]
fn operate_isolated_native_omp_managed_library() {
    operate("omp");
}

// Keep the Pi/OMP entry names and their legacy environment protocol unchanged.
#[test]
#[ignore = "controller supplies the exclusive native fixture and serializes its host"]
fn operate_isolated_native_managed_library() {
    let agent = std::env::var("PLEAT_NATIVE_TARGET").expect("explicit native target required");
    assert!(matches!(agent.as_str(), "pi" | "omp" | "hermes" | "openclaw" | "copilot"));
    let base = explicit_directory("PLEAT_NATIVE_FIXTURE");
    assert!(
        base.to_string_lossy()
            .starts_with(&format!("/private/tmp/pleat-native-{agent}-"))
    );
    let home = explicit_directory("PLEAT_NATIVE_LIBRARY_HOME");
    assert!(home.starts_with(&base) && home != base);
    let next = PathBuf::from(
        std::env::var_os("PLEAT_NATIVE_MOVE_TO").expect("explicit move destination required"),
    );
    assert!(next.is_absolute() && next.file_name().is_some());
    // Resolve the existing parent, not the destination: move requires it absent.
    // Reject traversal and aliases so every mutation stays in the owned fixture.
    assert!(next.components().all(|c| matches!(
        c,
        std::path::Component::RootDir | std::path::Component::Normal(_)
    )));
    let parent = next.parent().unwrap().canonicalize().unwrap();
    assert_eq!(next.parent().unwrap(), parent);
    assert!(parent.starts_with(&base));
    assert!(!home.starts_with(&next) && !next.starts_with(&home));
    let operation =
        std::env::var("PLEAT_NATIVE_PRODUCT_OP").expect("explicit native operation required");
    operate_library(&agent, &base, &home, &next, &operation, true);
}

fn explicit_directory(key: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var_os(key).expect("explicit fixture directory required"));
    assert!(path.is_absolute());
    let canonical = path.canonicalize().unwrap();
    assert!(canonical.is_dir());
    canonical
}

fn operate(agent: &str) {
    let upper = agent.to_ascii_uppercase();
    let base = PathBuf::from(
        std::env::var_os(format!("PLEAT_{upper}_FIXTURE")).expect("exclusive fixture required"),
    )
    .canonicalize()
    .unwrap();
    assert!(
        base.to_string_lossy()
            .starts_with(&format!("/private/tmp/pleat-native-{agent}-"))
    );
    let operation = std::env::var(format!("PLEAT_{upper}_PRODUCT_OP")).unwrap();
    operate_library(
        agent,
        &base,
        &base.join("pleat"),
        &base.join(format!("{agent}-library-moved-native-proof")),
        &operation,
        false,
    );
}

fn operate_library(
    agent: &str,
    base: &Path,
    home: &Path,
    next: &Path,
    operation: &str,
    native: bool,
) {
    assert!(matches!(
        operation,
        "prepare" | "update" | "reset-v1" | "move"
    ));
    let upper = agent.to_ascii_uppercase();
    let name = format!("pleat-{agent}-managed-probe");
    let mut library = SkillLibrary::open_in(home).unwrap();
    if native {
        // The configured library must remain in this task's owned namespace.
        assert!(
            library.config.library_dir == home.join("skills") || library.config.library_dir == next
        );
        assert!(
            library
                .config
                .library_dir
                .canonicalize()
                .unwrap()
                .starts_with(base)
        );
        if operation != "prepare" {
            let skill = library.resolve_skill(&name).unwrap();
            assert!(!skill.path.is_symlink());
            assert!(skill.path.canonicalize().unwrap().starts_with(base));
            assert!(
                skill
                    .path
                    .join("local-data")
                    .canonicalize()
                    .unwrap()
                    .starts_with(base)
            );
        }
    }
    let mut report = serde_json::json!({"operation":operation,"status":"started","source_trigger_writer_executed":false});
    if operation == "prepare" {
        assert!(library.resolve_skill(&name).is_err());
        let incoming = base.join("managed-native-incoming-v1");
        fs::create_dir(&incoming).unwrap();
        fs::write(incoming.join("SKILL.md"), format!("---\nname: {name}\ndescription: Managed owned native version one\n---\nPLEAT_{upper}_MANAGED_BODY_V1\n")).unwrap();
        let record = SkillRecord {
            name: name.clone(),
            storage_name: String::new(),
            description: "Managed owned native version one".into(),
            origin: SkillOrigin::Unknown,
            update_available: false,
            group_id: None,
            last_operated_at: 0,
        };
        library.import(&incoming, record).unwrap();
        let skill = library.resolve_skill(&name).unwrap();
        assert!(!skill.path.is_symlink());
        fs::create_dir(skill.path.join("local-data")).unwrap();
        fs::write(
            skill.path.join("local-data/ADAPTATION.md"),
            format!("PLEAT_{upper}_MANAGED_ADAPTATION_V1\n"),
        )
        .unwrap();
        fs::write(skill.path.join("local-data/state"), "OWNED-MANAGED-STATE\n").unwrap();
        report["source"] = serde_json::json!(skill.path);
        report["source_identity"] = id(&skill.path).into();
    } else if operation == "update" || operation == "reset-v1" {
        let skill = library.resolve_skill(&name).unwrap();
        let root_id = id(&skill.path);
        let local_id = id(&skill.path.join("local-data"));
        let local_state = fs::read(skill.path.join("local-data/state")).unwrap();
        let adaptation = fs::read(skill.path.join("local-data/ADAPTATION.md")).unwrap();
        let (version, word) = if operation == "update" {
            (2, "two")
        } else {
            (1, "one")
        };
        let incoming = base.join(format!("managed-native-incoming-v{version}"));
        let body = format!(
            "---\nname: {name}\ndescription: Managed owned native version {word}\n---\nPLEAT_{upper}_MANAGED_BODY_V{version}\n"
        );
        if incoming.exists() {
            assert!(!incoming.is_symlink());
            assert_eq!(fs::read_to_string(incoming.join("SKILL.md")).unwrap(), body);
        } else {
            fs::create_dir(&incoming).unwrap();
            fs::write(incoming.join("SKILL.md"), body).unwrap();
        }
        let mut record = skill.record.clone();
        record.description = format!("Managed owned native version {word}");
        library
            .replace_by_storage(&incoming, record.storage_name.clone(), record)
            .unwrap();
        assert_eq!(id(&skill.path), root_id);
        assert_eq!(id(&skill.path.join("local-data")), local_id);
        assert_eq!(
            fs::read(skill.path.join("local-data/state")).unwrap(),
            local_state
        );
        assert_eq!(
            fs::read(skill.path.join("local-data/ADAPTATION.md")).unwrap(),
            adaptation
        );
        report["source_root_identity_preserved"] = true.into();
        report["local_data_identity_and_bytes_preserved"] = true.into();
    } else if operation == "move" {
        let old = library.config.library_dir.clone();
        assert_eq!(old, home.join("skills"));
        let skill = library.resolve_skill(&name).unwrap();
        let source_id = id(&skill.path);
        let state = fs::read(skill.path.join("local-data/state")).unwrap();
        let plan = library
            .plan_storage_change(StorageAction::MoveLibrary, next)
            .unwrap();
        assert!(!next.exists() && old.is_dir());
        library.apply_storage_change(&plan).unwrap();
        let reopened = SkillLibrary::open_in(home).unwrap();
        let moved = reopened.resolve_skill(&name).unwrap();
        assert_eq!(reopened.config.library_dir, next);
        assert!(old.is_symlink());
        assert_eq!(id(&moved.path), source_id);
        assert_eq!(
            fs::read(moved.path.join("local-data/state")).unwrap(),
            state
        );
        report["from"] = serde_json::json!(old);
        report["to"] = serde_json::json!(next);
        report["source_root_identity_preserved"] = true.into();
        report["compatibility_alias"] = true.into();
    } else {
        panic!("unsupported exclusive fixture operation");
    }
    report["status"] = "pass".into();
    fs::write(
        base.join(format!("native-managed-library-{operation}.json")),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
}
