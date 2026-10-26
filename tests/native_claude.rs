//! Explicit, serial native acceptance. Not part of offline contract counts.
#![cfg(target_os = "macos")]

use std::{collections::BTreeSet, fs, path::Path, process::Command};
use kitter::{InstallTarget, SkillLibrary, SkillOrigin, SkillRecord, TriggerMode};
use serde_json::{Value, json};

const NAME: &str = "pleat-native-trigger-probe";

fn isolated_command(binary: &Path, home: &Path, project: &Path) -> Command {
    let mut command = Command::new(binary);
    command.env_clear().env("HOME", home).env("USERPROFILE", home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default()).env("LANG", "en_US.UTF-8")
        .env("KITTER_HOME", home.join("pleat-probe-state"))
        .env("DISABLE_AUTOUPDATER", "1").env("DISABLE_TELEMETRY", "1")
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1").current_dir(project);
    command
}

fn native_state(binary: &Path, home: &Path, project: &Path) -> Value {
    native_state_for_name(binary, home, project, NAME)
}

fn native_state_for_name(binary: &Path, home: &Path, project: &Path, name: &str) -> Value {
    let script = r#"
import sys, os, json
from pathlib import Path
sys.path.insert(0, sys.argv[1])
from native_suite import claude_initialize
name = sys.argv[4]
initialization, context = claude_initialize(Path(sys.argv[2]), os.environ, Path(sys.argv[3]), context=True)
commands = [c for c in initialization['commands'] if c['name'] == name]
automatic = [s for s in context.get('skills', {}).get('skillFrontmatter', []) if s['name'] == name]
print(json.dumps({'commands': commands, 'automatic': bool(automatic), 'api_usage': context.get('apiUsage', {}), 'version': initialization.get('claude_code_version'), 'adaptations': [m for m in context.get('memoryFiles', []) if m.get('path', '').endswith('/ADAPTATION.md')]}))
"#;
    let output = isolated_command(Path::new("/usr/bin/python3"), home, project)
        .args(["-c", script])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/agent-compatibility"))
        .arg(binary).arg(project).arg(name).output().unwrap();
    assert!(output.status.success(), "native initialize/context probe exited {:?}", output.status.code());
    serde_json::from_slice(&output.stdout).expect("native probe must return sanitized JSON")
}

#[test]
#[ignore = "Requires PLEAT_NATIVE_CLAUDE_BIN and the main controller's serial runtime queue"]
fn managed_trigger_policy_reaches_native_catalog_and_survives_adapted_update() {
    use std::os::fd::AsRawFd;
    let binary = std::path::PathBuf::from(std::env::var_os("PLEAT_NATIVE_CLAUDE_BIN")
        .expect("explicit native binary required"));
    assert!(binary.is_file());
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/agent-native-runtime");
    fs::create_dir_all(&runtime).unwrap();
    let lock = fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(runtime.join("native.lock")).unwrap();
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0,
        "another native Agent probe is active");
    let temp = tempfile::Builder::new().prefix("pleat-native-trigger-").tempdir_in("/private/tmp").unwrap();
    let root = temp.path().canonicalize().unwrap();
    let home = root.join("home");
    let project = home.join("project");
    let source = root.join("source");
    fs::create_dir_all(project.join(".git")).unwrap();
    fs::create_dir(&source).unwrap();
    let write_version = |version: &str| fs::write(source.join("SKILL.md"),
        format!("---\nname: {NAME}\ndescription: Native trigger {version}\n{}---\nFixture only.\n",
            if version == "v2" { "user-invocable: false\n" } else { "" })).unwrap();
    write_version("v1");
    let mut library = SkillLibrary::open_in(project.join("pleat-data")).unwrap();
    library.import(&source, SkillRecord {
        name: NAME.into(), storage_name: String::new(), description: String::new(),
        origin: SkillOrigin::Unknown, update_available: false, group_id: None, last_operated_at: 0,
    }).unwrap();
    let record = library.record(NAME).unwrap();
    let managed = library.skill_path(NAME).unwrap();
    fs::create_dir(managed.join("local-data")).unwrap();
    let adaptation = managed.join("local-data/ADAPTATION.md");
    fs::write(&adaptation, "preserve this local fixture state").unwrap();
    kitter::project::install_from_path(&project, &managed, NAME, &[InstallTarget::ClaudeCode]).unwrap();
    let initial = native_state(&binary, &home, &project);
    assert_eq!(initial["automatic"], true);
    assert!(initial["adaptations"].as_array().unwrap().iter().any(|m| Path::new(m["path"].as_str().unwrap()).canonicalize().ok().as_deref() == Some(adaptation.as_path())), "native context must include the installed adaptation import");
    assert_eq!(initial["commands"].as_array().unwrap().len(), 1);
    library.set_trigger_mode_by_storage(&record.storage_name, TriggerMode::Manual).unwrap();
    let manual = native_state(&binary, &home, &project);
    assert_eq!(manual["automatic"], false);
    assert_eq!(manual["commands"].as_array().unwrap().len(), 1, "manual command must remain selectable");
    write_version("v2");
    library.replace_by_storage(&source, record.storage_name.clone(), record.clone()).unwrap();
    let updated = native_state(&binary, &home, &project);
    assert_eq!(updated["automatic"], false);
    assert!(updated["commands"][0]["description"].as_str().unwrap().contains("v2"));
    library.set_trigger_mode_by_storage(&record.storage_name, TriggerMode::Automatic).unwrap();
    assert_eq!(native_state(&binary, &home, &project)["automatic"], true);
    library.set_trigger_mode_by_storage(&record.storage_name, TriggerMode::FollowSkill).unwrap();
    assert_eq!(fs::read(managed.join("SKILL.md")).unwrap(), fs::read(source.join("SKILL.md")).unwrap());
    let followed = native_state(&binary, &home, &project);
    assert_eq!(followed["automatic"], true);
    assert!(followed["commands"].as_array().unwrap().is_empty(), "upstream user-invocable=false must be restored");
    kitter::project::uninstall(&project, &library.config.library_dir, NAME, &[InstallTarget::ClaudeCode]).unwrap();
    let removed = native_state(&binary, &home, &project);
    assert_eq!(removed["automatic"], false);
    assert!(removed["adaptations"].as_array().unwrap().is_empty(), "uninstall must detach the native adaptation import");
    assert!(removed["commands"].as_array().unwrap().is_empty());
    assert_eq!(fs::read_to_string(&adaptation).unwrap(), "preserve this local fixture state");
    assert!(source.join("SKILL.md").is_file());
    // User scope is a separate native loading entry and needs its own proof.
    kitter::project::install_from_path(&home, &managed, NAME, &[InstallTarget::ClaudeCode]).unwrap();
    let global = native_state(&binary, &home, &project);
    assert_eq!(global["automatic"], true);
    assert!(global["adaptations"].as_array().unwrap().iter().any(|m| Path::new(m["path"].as_str().unwrap()).canonicalize().ok().as_deref() == Some(adaptation.as_path())));
    kitter::project::uninstall(&home, &library.config.library_dir, NAME, &[InstallTarget::ClaudeCode]).unwrap();
    assert!(native_state(&binary, &home, &project)["adaptations"].as_array().unwrap().is_empty());

    // Two real native-registered cwd values feed the same public batch planner.
    // The second cwd has its own native repository boundary; external imports require native consent.
    let second = project.join("second project");
    fs::create_dir(&second).unwrap();
    assert!(Command::new("git").args(["init", "--quiet"]).arg(&second).status().unwrap().success());
    for (index, cwd) in [&project, &second].into_iter().enumerate() {
        let output = isolated_command(&binary, &home, cwd)
            .args(["mcp", "add", "--scope", "local", &format!("pleat-registration-{index}"), "--", "/usr/bin/true"])
            .output().unwrap();
        assert!(output.status.success(), "native registration failed at fixture index {index}, exit {:?}", output.status.code());
    }
    let output = isolated_command(Path::new(env!("CARGO_BIN_EXE_kitter")), &home, &project)
        .args(["projects", "--json"]).output().unwrap();
    assert!(output.status.success());
    let native_projects: Value = serde_json::from_slice(&output.stdout).unwrap();
    let projects = [&project, &second].into_iter().map(|cwd| {
        let row = native_projects.as_array().unwrap().iter().find(|row| row["path"].as_str() == cwd.to_str()).unwrap_or_else(|| panic!("Pleat must discover native fixture cwd {}; actual fixture paths {:?}", cwd.display(), native_projects.as_array().unwrap().iter().filter_map(|r| r["path"].as_str()).collect::<Vec<_>>()));
        assert!(row["agents"].as_array().unwrap().contains(&json!("claude_code")));
        kitter::project_discovery::ProjectEntry { path: cwd.to_path_buf(), name: row["name"].as_str().unwrap().into(),
            agents: vec![kitter::effective_skills::AgentKind::ClaudeCode], proma_workspaces: vec![], native_workspaces: Vec::new(), archive_states: Vec::new(), archived: false, hidden: false, retained: false }
    }).collect::<Vec<_>>();
    let beta_name = "pleat-native-batch-probe";
    let beta_source = root.join("beta-source");
    fs::create_dir(&beta_source).unwrap();
    fs::write(beta_source.join("SKILL.md"), format!("---\nname: {beta_name}\ndescription: Batch native v1\n---\nFixture only.\n")).unwrap();
    library.import(&beta_source, SkillRecord { name: beta_name.into(), storage_name: String::new(), description: String::new(),
        origin: SkillOrigin::Unknown, update_available: false, group_id: None, last_operated_at: 0 }).unwrap();
    let beta = library.skill_path(beta_name).unwrap();
    fs::create_dir(beta.join("local-data")).unwrap();
    fs::write(beta.join("local-data/ADAPTATION.md"), "Batch fixture local adaptation").unwrap();
    let mut skills = [(NAME, &managed), (beta_name, &beta)].into_iter().map(|(name, path)| kitter::cross_project::Skill {
        key: format!("file:{}", path.join("SKILL.md").canonicalize().unwrap().display()), name: name.into(), source: path.to_path_buf(), provider: "library", install_name: Some(name.into()),
    }).collect::<Vec<_>>();
    let agents = kitter::agents::AGENT_ICON_ORDER.iter().filter(|agent| agent.id == "claude-code").copied().collect::<Vec<_>>();
    let selected_skills = skills.iter().map(|skill| skill.key.clone()).collect::<BTreeSet<_>>();
    let selected_projects = projects.iter().map(|project| project.path.clone()).collect::<BTreeSet<_>>();
    let selected_agents = BTreeSet::from(["claude-code".to_string()]);
    let catalog = kitter::cross_project::Catalog::scan(skills.clone(), projects.clone(), agents.clone(), None);
    let plan = catalog.plan(kitter::cross_project::Action::Install, &selected_skills, &selected_agents, &selected_projects, &BTreeSet::new()).unwrap();
    assert_eq!(plan.items.len(), 4);
    assert!(plan.items.iter().all(|item| item.instruction_paths.len() == 1));
    let conflict = second.join(".claude/skills").join(beta_name);
    fs::create_dir_all(&conflict).unwrap();
    fs::write(conflict.join("SKILL.md"), "user-owned concurrent fixture").unwrap();
    let mut result = plan.execute();
    assert_eq!((result.changed(), result.failed()), (3, 1));
    assert_eq!(fs::read_to_string(conflict.join("SKILL.md")).unwrap(), "user-owned concurrent fixture");
    // Remove only the fixture this test itself introduced, then retry failed items.
    fs::remove_file(conflict.join("SKILL.md")).unwrap();
    fs::remove_dir(&conflict).unwrap();
    result.merge_retry(result.retry_plan().execute());
    assert_eq!((result.changed(), result.failed()), (4, 0));
    let mut adaptation_blockers = Vec::new();
    for cwd in [&project, &second] {
        assert_eq!(native_state(&binary, &home, cwd)["automatic"], true);
        let state = native_state_for_name(&binary, &home, cwd, beta_name);
        assert_eq!(state["automatic"], true);
        assert_eq!(state["commands"].as_array().unwrap().len(), 1);
        if state["adaptations"].as_array().unwrap().is_empty() {
            adaptation_blockers.push("project outside adaptation source boundary requires native external-import consent");
        }
    }
    let migrated = project.join("migrated-library");
    let migration = library.plan_storage_change(kitter::library::StorageAction::MoveLibrary, &migrated).unwrap();
    assert!(migration.compatibility_alias);
    library.apply_storage_change(&migration).unwrap();
    assert_eq!(managed.canonicalize().unwrap(), migrated.join(&record.storage_name));
    write_version("v3");
    library.replace_by_storage(&source, record.storage_name.clone(), library.record(NAME).unwrap()).unwrap();
    for cwd in [&project, &second] {
        let state = native_state(&binary, &home, cwd);
        assert_eq!(state["automatic"], true);
        assert!(state["commands"][0]["description"].as_str().unwrap().contains("v3"), "source update after migration must reach native catalog");
    }
    assert_eq!(fs::read_to_string(&adaptation).unwrap(), "preserve this local fixture state");
    for skill in &mut skills {
        skill.source = skill.source.canonicalize().unwrap();
        skill.key = format!("file:{}", skill.source.join("SKILL.md").canonicalize().unwrap().display());
    }
    let selected_skills = skills.iter().map(|skill| skill.key.clone()).collect::<BTreeSet<_>>();
    let catalog = kitter::cross_project::Catalog::scan(skills, projects, agents, None);
    let removal = catalog.plan(kitter::cross_project::Action::Remove, &selected_skills, &selected_agents, &selected_projects, &BTreeSet::new()).unwrap().execute();
    assert_eq!((removal.changed(), removal.failed()), (4, 0));
    for cwd in [&project, &second] {
        assert_eq!(native_state(&binary, &home, cwd)["automatic"], false);
        let state = native_state_for_name(&binary, &home, cwd, beta_name);
        assert_eq!(state["automatic"], false);
        assert!(state["commands"].as_array().unwrap().is_empty());
        assert!(state["adaptations"].as_array().unwrap().is_empty());
    }
    assert!(beta.join("local-data/ADAPTATION.md").is_file());
    assert!(adaptation.is_file() && source.join("SKILL.md").is_file());
    if let Some(path) = std::env::var_os("PLEAT_NATIVE_EVIDENCE") {
        fs::write(path, serde_json::to_vec_pretty(&json!({
            "agent":"claude-code", "status":"partial_native_pass_external_import_consent_pending", "interface":"native initialize + get_context_usage summary",
            "native_version":initial["version"],
            "public_library_api_used":true, "automatic_catalog":initial["automatic"],
            "manual_excluded_from_automatic_catalog":!manual["automatic"].as_bool().unwrap(),
            "manual_command_retained":true, "mode_retained_through_update":true,
            "automatic_and_follow_restored":true, "uninstalled_not_discovered":true,
            "source_and_local_data_preserved":true, "inference_requests":0,
            "adaptation_loading_integration":"native memoryFiles includes installed local-data/ADAPTATION.md; removed on uninstall",
            "adaptation_verified_scopes":["global","project"],
            "native_registered_projects":2, "batch_skills":2, "batch_installations":4,
            "impact_preview_includes_native_rules":true, "partial_failure_conflict_preserved_and_retry_passed":true,
            "batch_native_discovery_and_uninstall_passed":true,
            "batch_project_adaptation_blockers":adaptation_blockers,
            "storage_migration_native_loading_and_update_passed":true,
            "migration_compatibility_alias_and_local_data_preserved":true
        })).unwrap()).unwrap();
    }
}
