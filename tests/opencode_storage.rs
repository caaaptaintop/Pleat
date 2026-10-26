//! Controller-only product operation; does not start or talk to an Agent.
#![cfg(unix)]
use kitter::{SkillLibrary, library::StorageAction};
use std::{fs, path::PathBuf};

#[test]
#[ignore = "controller's isolated OpenCode fixture and live host are required"]
fn move_isolated_native_library_keeps_installation_uris() {
    let base = PathBuf::from(
        std::env::var_os("PLEAT_OPENCODE_FIXTURE").expect("exclusive fixture required"),
    )
    .canonicalize()
    .unwrap();
    assert!(
        base.to_string_lossy()
            .starts_with("/private/tmp/pleat-native-opencode-")
    );
    let data = base.join("pleat");
    let mut library = SkillLibrary::open_in(&data).unwrap();
    let before = library.config.library_dir.clone();
    assert_eq!(before, data.join("skills"));
    let target = base.join("library-moved-native-proof");
    assert!(!target.exists());
    let ids = [
        "pleat-opencode-global-probe",
        "pleat-opencode-project-probe",
    ];
    let installations = [
        base.join("home/.config/opencode/skills").join(ids[0]),
        base.join("home/project/.opencode/skills").join(ids[1]),
    ];
    let links = installations
        .iter()
        .map(|p| fs::read_link(p).unwrap())
        .collect::<Vec<_>>();
    let source_roots = installations
        .iter()
        .map(|p| p.canonicalize().unwrap())
        .collect::<Vec<_>>();
    let plan = library
        .plan_storage_change(StorageAction::MoveLibrary, &target)
        .unwrap();
    assert_eq!(library.config.library_dir, before);
    assert!(before.is_dir() && !target.exists());
    let mut report = serde_json::json!({"status":"previewed", "from":plan.from,"to":plan.to,
        "skill_count":plan.skill_count,"compatibility_alias":plan.compatibility_alias,
        "preview_did_not_move_files":true,"source_trigger_writer_executed":false});
    let output = base.join("candidate-native-library-migration.json");
    fs::write(&output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    library.apply_storage_change(&plan).unwrap();
    assert_eq!(library.config.library_dir, target);
    assert!(before.is_symlink());
    let reopened = SkillLibrary::open_in(&data).unwrap();
    assert_eq!(reopened.config.library_dir, target);
    for (index, id) in ids.iter().enumerate() {
        let skill = reopened.resolve_skill(id).unwrap();
        assert_eq!(skill.path.canonicalize().unwrap(), source_roots[index]);
        assert_eq!(fs::read_link(&installations[index]).unwrap(), links[index]);
        assert_eq!(
            installations[index].canonicalize().unwrap(),
            source_roots[index]
        );
    }
    report["status"] = "pass".into();
    report["reopened_library_matches_destination"] = true.into();
    report["native_installation_links_unchanged"] = true.into();
    report["native_source_identities_unchanged"] = true.into();
    fs::write(&output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
}
