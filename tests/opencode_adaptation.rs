//! Cross-module installation contracts; these are not host runtime evidence.
#![cfg(all(unix, any(target_os = "macos", target_os = "linux")))]
use std::{collections::BTreeSet, fs, path::{Path, PathBuf}};
use kitter::{agents::AGENT_ICON_ORDER, cross_project::{Action, Catalog, Skill},
    effective_skills::AgentKind, project_discovery::ProjectEntry};

fn skill(root: &Path, name: &str) -> Skill {
    let source = root.join("sources").join(name);
    fs::create_dir_all(source.join("local-data")).unwrap();
    fs::write(source.join("SKILL.md"), format!("---\nname: {name}\ndescription: isolated contract\n---\nBody\n")).unwrap();
    fs::write(source.join("local-data/ADAPTATION.md"), "Owned fixture adaptation").unwrap();
    fs::write(source.join("local-data/state"), "Preserve this private fixture data").unwrap();
    Skill { key: format!("file:{}", source.join("SKILL.md").canonicalize().unwrap().display()),
        name: name.to_owned(), source, provider: "library", install_name: Some(name.to_owned()) }
}
fn project(root: &Path, name: &str) -> ProjectEntry {
    let path = root.join(name);
    fs::create_dir_all(&path).unwrap();
    ProjectEntry { path, name: name.to_owned(), agents: vec![AgentKind::OpenCode],
        proma_workspaces: vec![], native_workspaces: vec![], archive_states: vec![],
        archived: false, hidden: false, retained: false }
}
fn catalog(skills: &[Skill], projects: &[ProjectEntry], ids: &[&str]) -> Catalog {
    Catalog::scan(skills.to_vec(), projects.to_vec(), AGENT_ICON_ORDER.iter()
        .filter(|agent| ids.contains(&agent.id)).copied().collect(), None)
}
fn keys(skills: &[Skill]) -> BTreeSet<String> { skills.iter().map(|s| s.key.clone()).collect() }
fn selected(ids: &[&str]) -> BTreeSet<String> { ids.iter().map(|s| s.to_string()).collect() }
fn paths(projects: &[ProjectEntry]) -> BTreeSet<PathBuf> { projects.iter().map(|p| p.path.clone()).collect() }
fn intact(skills: &[Skill]) {
    for skill in skills {
        assert!(fs::read_to_string(skill.source.join("SKILL.md")).unwrap().ends_with("Body\n"));
        assert_eq!(fs::read_to_string(skill.source.join("local-data/ADAPTATION.md")).unwrap(), "Owned fixture adaptation");
        assert_eq!(fs::read_to_string(skill.source.join("local-data/state")).unwrap(), "Preserve this private fixture data");
    }
}

#[test]
fn two_skills_two_agents_two_projects_keep_shared_native_config_and_source_data() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let skills = vec![skill(&root, "first"), skill(&root, "second")];
    let projects = vec![project(&root, "project-one"), project(&root, "project-two")];
    for project in &projects {
        fs::create_dir(project.path.join(".opencode")).unwrap();
        fs::write(project.path.join(".opencode/opencode.jsonc"), "// user comment\n{\"instructions\":[\"user-kept.md\"],\"model\":\"unchanged/model\"}\n").unwrap();
    }
    let ids = ["opencode", "codex"];
    let c = catalog(&skills, &projects, &ids);
    let p = c.plan(Action::Install, &keys(&skills), &selected(&ids), &paths(&projects), &BTreeSet::new()).unwrap();
    let result = p.execute();
    assert_eq!(result.changed(), 8);
    assert!(result.outcomes.iter().all(|o| o.error.is_none()));
    for project in &projects {
        let config = fs::read_to_string(project.path.join(".opencode/opencode.jsonc")).unwrap();
        assert!(config.starts_with("// user comment"));
        assert!(config.contains("user-kept.md") && config.contains("unchanged/model"));
        for skill in &skills {
            assert!(config.contains(&skill.name));
            for directory in [".opencode/skills", ".codex/skills"] {
                assert_eq!(project.path.join(directory).join(&skill.name).canonicalize().unwrap(), skill.source);
            }
        }
    }
    intact(&skills);
    let c = catalog(&skills, &projects, &ids);
    let result = c.plan(Action::Remove, &keys(&skills), &selected(&ids), &paths(&projects), &BTreeSet::new()).unwrap().execute();
    assert_eq!(result.changed(), 8);
    assert!(result.outcomes.iter().all(|o| o.error.is_none()));
    for project in &projects {
        let config = fs::read_to_string(project.path.join(".opencode/opencode.jsonc")).unwrap();
        assert!(!config.contains("pleat-adaptation-v1:"));
        assert!(config.contains("user-kept.md") && config.contains("unchanged/model"));
        for skill in &skills {
            for directory in [".opencode/skills", ".codex/skills"] {
                assert!(project.path.join(directory).join(&skill.name).symlink_metadata().is_err());
            }
        }
    }
    intact(&skills);
}

#[test]
fn shared_claude_opencode_skill_directory_merges_operations_and_native_entries() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let skills = vec![skill(&root, "first"), skill(&root, "second")];
    let projects = vec![project(&root, "project")];
    let project = &projects[0].path;
    fs::create_dir_all(project.join(".claude/skills")).unwrap();
    fs::create_dir(project.join(".opencode")).unwrap();
    std::os::unix::fs::symlink(project.join(".claude/skills"), project.join(".opencode/skills")).unwrap();
    let ids = ["opencode", "claude-code"];
    let c = catalog(&skills, &projects, &ids);
    let p = c.plan(Action::Install, &keys(&skills), &selected(&ids), &paths(&projects), &BTreeSet::new()).unwrap();
    let result = p.execute();
    assert_eq!(result.changed(), 2);
    assert!(result.outcomes.iter().all(|o| o.error.is_none()));
    assert_eq!(fs::read_dir(project.join(".claude/rules")).unwrap().count(), 2);
    let native = fs::read_to_string(project.join(".opencode/opencode.jsonc")).unwrap();
    assert!(native.contains("first") && native.contains("second"));
    for skill in &skills {
        assert_eq!(project.join(".opencode/skills").join(&skill.name).canonicalize().unwrap(), skill.source);
    }
    let c = catalog(&skills, &projects, &ids);
    let result = c.plan(Action::Remove, &keys(&skills), &selected(&ids), &paths(&projects), &BTreeSet::new()).unwrap().execute();
    assert_eq!(result.changed(), 2);
    assert!(result.outcomes.iter().all(|o| o.error.is_none()));
    assert_eq!(fs::read_dir(project.join(".claude/rules")).unwrap().count(), 0);
    assert!(!fs::read_to_string(project.join(".opencode/opencode.jsonc")).unwrap().contains("pleat-adaptation-v1:"));
    intact(&skills);
}

/// Controller for the owned live-host probe; the parent owns the native slot.
#[test]
#[ignore = "requires the controller's two isolated native OpenCode projects"]
fn owned_native_two_project_batch_retry_and_remove() {
    use std::os::unix::fs::MetadataExt;
    let root = PathBuf::from(std::env::var_os("PLEAT_OPENCODE_BATCH_FIXTURE").unwrap())
        .canonicalize().unwrap();
    assert!(root.to_string_lossy().starts_with("/private/var/tmp/pleat-opencode-batch-"));
    let input: serde_json::Value = serde_json::from_slice(&fs::read(root.join("batch-input.json")).unwrap()).unwrap();
    let skills = input["names"].as_array().unwrap().iter().map(|name| {
        let name = name.as_str().unwrap().to_owned();
        assert!(!name.contains('/') && name.starts_with("pleat-opencode-batch-"));
        let source = root.join("sources").join(&name).canonicalize().unwrap();
        assert!(source.starts_with(root.join("sources")));
        Skill { key: format!("file:{}", source.join("SKILL.md").canonicalize().unwrap().display()),
            name: name.clone(), source, provider: "library", install_name: Some(name) }
    }).collect::<Vec<_>>();
    assert_eq!(skills.len(), 2);
    let projects = ["one", "two"].into_iter().map(|name| ProjectEntry {
        path: root.join("projects").join(name).canonicalize().unwrap(), name: name.into(),
        agents: vec![AgentKind::OpenCode], proma_workspaces: vec![], native_workspaces: vec![],
        archive_states: vec![], archived: false, hidden: false, retained: false,
    }).collect::<Vec<_>>();
    let c = catalog(&skills, &projects, &["opencode"]);
    let phase = std::env::var("PLEAT_OPENCODE_BATCH_PHASE").unwrap();
    let action = match phase.as_str() { "install" => Action::Install, "remove" => Action::Remove, _ => panic!("unknown owned phase") };
    let plan = c.plan(action, &keys(&skills), &selected(&["opencode"]), &paths(&projects), &BTreeSet::new()).unwrap();
    assert_eq!(plan.items.len(), 4);
    assert_eq!(plan.count(kitter::cross_project::Disposition::Change), 4);
    let preview = plan.items.iter().map(|item| serde_json::json!({
        "project":item.project,"path":item.path,"skill_key":item.skill.key,
        "instruction_paths":item.instruction_paths
    })).collect::<Vec<_>>();
    let mut result;
    if phase == "install" {
        // The foreign entry appears after preview, exercising the actual retry.
        let conflict = projects[1].path.join(".opencode/skills").join(&skills[0].name);
        assert!(!conflict.exists());
        fs::write(&conflict, b"OWNED_CONFLICT_PRESERVE\n").unwrap();
        result = plan.execute();
        assert_eq!((result.changed(), result.failed()), (3, 1), "{result:?}");
        assert_eq!(fs::read(&conflict).unwrap(), b"OWNED_CONFLICT_PRESERVE\n");
        let identities = result.outcomes.iter().filter(|outcome| outcome.changed).map(|outcome| {
            let m = fs::symlink_metadata(&outcome.item.path).unwrap();
            (outcome.item.path.clone(), (m.dev(), m.ino(), m.mtime(), m.mtime_nsec()), fs::read_link(&outcome.item.path).unwrap())
        }).collect::<Vec<_>>();
        let retained = root.join("retained-conflict");
        assert!(!retained.exists());
        fs::rename(&conflict, &retained).unwrap();
        let retry = result.retry_plan();
        assert_eq!(retry.items.len(), 1);
        assert_eq!(retry.items[0].path, conflict);
        result.merge_retry(retry.execute());
        assert_eq!((result.changed(), result.failed()), (4, 0), "{result:?}");
        for (path, identity, link) in identities {
            let m = fs::symlink_metadata(&path).unwrap();
            assert_eq!((m.dev(), m.ino(), m.mtime(), m.mtime_nsec()), identity);
            assert_eq!(fs::read_link(&path).unwrap(), link);
        }
        assert_eq!(fs::read(retained).unwrap(), b"OWNED_CONFLICT_PRESERVE\n");
    } else {
        result = plan.execute();
        assert_eq!((result.changed(), result.failed()), (4, 0), "{result:?}");
    }
    for project in &projects {
        for skill in &skills {
            let path = project.path.join(".opencode/skills").join(&skill.name);
            if phase == "install" { assert_eq!(fs::read_link(path).unwrap(), skill.source); }
            else { assert!(path.symlink_metadata().is_err()); }
        }
        let config = fs::read_to_string(project.path.join(".opencode/opencode.jsonc")).unwrap();
        assert!(config.starts_with("// owned user config\n") && config.contains("owned-existing.md"));
    }
    let report = serde_json::json!({"status":"pass","phase":phase,"preview":preview,
        "changed":result.changed(),"failed":result.failed(),"source_trigger_writer_executed":false,
        "initial_changed":if phase=="install" {3} else {4},"initial_failed":if phase=="install" {1} else {0},
        "retry_items":if phase=="install" {1} else {0},"successful_link_identities_preserved":phase=="install"});
    fs::write(root.join(format!("batch-proof-{phase}.json")), serde_json::to_vec_pretty(&report).unwrap()).unwrap();
}
