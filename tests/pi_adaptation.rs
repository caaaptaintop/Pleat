//! Product scope/batch contracts. Native Pi evidence lives in native-pi.json.
#![cfg(all(unix, any(target_os = "macos", target_os = "linux")))]
use std::{collections::BTreeSet, fs, path::{Path, PathBuf}};
use kitter::{agents::AGENT_ICON_ORDER, cross_project::{Action, Catalog, Skill},
    effective_skills::AgentKind, project_discovery::ProjectEntry};

#[test]
fn two_skills_two_hosts_two_projects_remove_only_selected_pi_entries() {
    verify_batch("pi", AgentKind::Pi, ".pi");
}

#[test]
fn two_skills_two_hosts_two_projects_remove_only_selected_omp_entries() {
    verify_batch("omp", AgentKind::Omp, ".omp");
}

fn verify_batch(native: &str, kind: AgentKind, directory: &str) {
    // OMP reads Codex compatibility roots, so Pi is its independent neighbour.
    let (other, other_directory) = if native == "omp" { ("pi", ".pi") } else { ("codex", ".codex") };
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let skills = ["first", "second"].into_iter().map(|name| {
        let source = root.join("sources").join(name);
        fs::create_dir_all(source.join("local-data")).unwrap();
        fs::write(source.join("SKILL.md"), format!("---\nname: {name}\ndescription: owned batch fixture\n---\nBody\n")).unwrap();
        fs::write(source.join("local-data/ADAPTATION.md"), "Owned adaptation").unwrap();
        fs::write(source.join("local-data/state"), "Preserve local state").unwrap();
        Skill { key: format!("file:{}", source.join("SKILL.md").display()), name: name.into(),
            source, provider: "library", install_name: Some(name.into()) }
    }).collect::<Vec<_>>();
    let projects = ["one", "two"].into_iter().map(|name| {
        let path = root.join(name); fs::create_dir(&path).unwrap();
        fs::create_dir_all(path.join(directory).join("extensions")).unwrap();
        fs::write(path.join(directory).join("extensions/user-kept.js"), "// owned unrelated fixture\n").unwrap();
        ProjectEntry { path, name: name.into(), agents: vec![kind], proma_workspaces: vec![],
            native_workspaces: vec![], archive_states: vec![], archived: false, hidden: false, retained: false }
    }).collect::<Vec<_>>();
    let agents = AGENT_ICON_ORDER.iter().filter(|a| [native, other].contains(&a.id)).copied().collect::<Vec<_>>();
    let all_keys = skills.iter().map(|s| s.key.clone()).collect::<BTreeSet<_>>();
    let roots = projects.iter().map(|p| p.path.clone()).collect::<BTreeSet<PathBuf>>();
    let selected = [native, other].into_iter().map(String::from).collect::<BTreeSet<_>>();
    let scan = || Catalog::scan(skills.clone(), projects.clone(), agents.clone(), None);
    let plan = scan().plan(Action::Install, &all_keys, &selected, &roots, &BTreeSet::new()).unwrap();
    assert_eq!(plan.items.iter().filter(|item| item.instruction_paths.iter().any(|p|
        p.parent().is_some_and(|parent| parent.ends_with(format!("{directory}/extensions"))))).count(), 4);
    let applied = plan.execute();
    assert_eq!(applied.changed(), 8);
    assert!(applied.outcomes.iter().all(|o| o.error.is_none()));
    for project in &projects {
        assert_eq!(managed_entries(&project.path, directory).len(), 2);
        for skill in &skills {
            assert_eq!(project.path.join(directory).join("skills").join(&skill.name).canonicalize().unwrap(), skill.source);
        }
    }
    let selected_native = BTreeSet::from([native.to_owned()]);
    let first = BTreeSet::from([skills[0].key.clone()]);
    let result = scan().plan(Action::Remove, &first, &selected_native, &roots, &BTreeSet::new()).unwrap().execute();
    assert_eq!(result.changed(), 2);
    assert!(result.outcomes.iter().all(|o| o.error.is_none()));
    for project in &projects {
        assert_eq!(managed_entries(&project.path, directory).len(), 1);
        assert!(!project.path.join(directory).join("skills/first").exists());
        assert!(project.path.join(directory).join("skills/second").is_symlink());
        assert!(project.path.join(other_directory).join("skills/first").is_symlink());
        assert_eq!(fs::read_to_string(project.path.join(directory).join("extensions/user-kept.js")).unwrap(), "// owned unrelated fixture\n");
        assert!(fs::read_dir(project.path.join(directory)).unwrap().flatten().any(|e|
            e.file_name().to_string_lossy().starts_with(".pleat-removed-rule-") && e.path().join("instruction.md").is_file()));
    }
    for skill in &skills {
        assert!(fs::read_to_string(skill.source.join("SKILL.md")).unwrap().ends_with("Body\n"));
        assert_eq!(fs::read_to_string(skill.source.join("local-data/state")).unwrap(), "Preserve local state");
        assert_eq!(fs::read_to_string(skill.source.join("local-data/ADAPTATION.md")).unwrap(), "Owned adaptation");
    }
}

fn managed_entries(project: &Path, directory: &str) -> Vec<PathBuf> {
    fs::read_dir(project.join(directory).join("extensions")).unwrap().flatten().filter(|e|
        e.file_name().to_string_lossy().starts_with("pleat-adaptation-")).map(|e| e.path()).collect()
}

#[test]
fn pi_native_and_shared_installations_keep_actual_descriptors_and_remove_independently() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let source = root.join("source");
    fs::create_dir_all(source.join("local-data")).unwrap();
    fs::write(source.join("SKILL.md"), "---\nname: probe\ndescription: owned\n---\nBody\n").unwrap();
    fs::write(source.join("local-data/ADAPTATION.md"), "Owned adaptation").unwrap();
    let project = root.join("project");
    fs::create_dir(&project).unwrap();
    let skill = Skill { key: format!("file:{}", source.join("SKILL.md").display()), name: "probe".into(),
        source: source.clone(), provider: "library", install_name: Some("probe".into()) };
    let entry = ProjectEntry { path: project.clone(), name: "project".into(), agents: vec![AgentKind::Pi],
        proma_workspaces: vec![], native_workspaces: vec![], archive_states: vec![],
        archived: false, hidden: false, retained: false };
    // DimAgent's product install target is the shared .agents root, also read by Pi.
    let agents = AGENT_ICON_ORDER.iter().filter(|a| ["pi", "dimagent"].contains(&a.id)).copied().collect::<Vec<_>>();
    let keys = BTreeSet::from([skill.key.clone()]);
    let roots = BTreeSet::from([project.clone()]);
    let scan = || Catalog::scan(vec![skill.clone()], vec![entry.clone()], agents.clone(), None);
    let plan = scan().plan(Action::Install, &keys, &BTreeSet::from(["pi".into(), "dimagent".into()]), &roots, &BTreeSet::new()).unwrap();
    assert_eq!(plan.items.len(), 2);
    let pi_paths = plan.items.iter().flat_map(|item| item.instruction_paths.iter())
        .filter(|p| p.parent().is_some_and(|parent| parent.ends_with(".pi/extensions")))
        .cloned().collect::<BTreeSet<_>>();
    assert_eq!(pi_paths.len(), 2);
    let result = plan.execute();
    assert_eq!((result.changed(), result.failed()), (2, 0));
    let shared = project.join(".agents/skills/probe");
    let native = project.join(".pi/skills/probe");
    let descriptor_for = |installation: &Path| pi_paths.iter().find(|p| fs::read_to_string(p).unwrap()
        .contains(&serde_json::to_string(&installation.join("SKILL.md")).unwrap())).unwrap().clone();
    let shared_descriptor = descriptor_for(&shared);
    let native_descriptor = descriptor_for(&native);
    assert_ne!(shared_descriptor, native_descriptor);
    assert_eq!(shared.canonicalize().unwrap(), source);
    assert_eq!(native.canonicalize().unwrap(), source);
    let remove = scan().plan(Action::Remove, &keys, &BTreeSet::from(["dimagent".into()]), &roots, &BTreeSet::new()).unwrap().execute();
    assert_eq!((remove.changed(), remove.failed()), (1, 0));
    assert!(!shared_descriptor.exists());
    assert!(native_descriptor.is_file());
    assert!(!shared.exists());
    assert_eq!(native.canonicalize().unwrap(), source);
    assert_eq!(fs::read_to_string(source.join("local-data/ADAPTATION.md")).unwrap(), "Owned adaptation");
}

// Literal fixtures for the two historical serde_json producers. Do not build
// these with json! or Value's Display: that would inherit the feature under test.
const PI_CONFIG_ORDERS: [&str; 2] = [
    r#"{"manifest":{"schemaVersion":1,"skills":[{"canonicalSourceRoot":@source@,"installationSkillPath":@installation@,"nativeName":@name@,"sourceIdentity":@identity@}]},"readerExecutable":@reader@}"#,
    r#"{"manifest":{"schemaVersion":1,"skills":[{"installationSkillPath":@installation@,"canonicalSourceRoot":@source@,"sourceIdentity":@identity@,"nativeName":@name@}]},"readerExecutable":@reader@}"#,
];

fn pi_config_fixture(template: &str, value: &serde_json::Value) -> String {
    let entry = &value["manifest"]["skills"][0];
    [("@source@", &entry["canonicalSourceRoot"]),
        ("@installation@", &entry["installationSkillPath"]), ("@name@", &entry["nativeName"]),
        ("@identity@", &entry["sourceIdentity"]), ("@reader@", &value["readerExecutable"])]
        .into_iter().fold(template.to_owned(), |text, (token, value)|
            text.replace(token, &serde_json::to_string(value).unwrap()))
}

fn pi_json_fixture(native: &str, kind: AgentKind, run: impl FnOnce(&Path, &Path, &Path, &dyn Fn(Action) -> kitter::cross_project::Plan)) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let source = root.join("source with space-技能");
    fs::create_dir_all(source.join("local-data")).unwrap();
    fs::write(source.join("SKILL.md"), "---\nname: probe\ndescription: owned\n---\nBody\n").unwrap();
    fs::write(source.join("local-data/ADAPTATION.md"), "Keep adaptation").unwrap();
    let project = root.join("project");
    fs::create_dir(&project).unwrap();
    let skill = Skill { key: format!("file:{}", source.join("SKILL.md").display()), name: "probe".into(),
        source: source.clone(), provider: "library", install_name: Some("probe".into()) };
    let entry = ProjectEntry { path: project.clone(), name: "project".into(), agents: vec![kind],
        proma_workspaces: vec![], native_workspaces: vec![], archive_states: vec![],
        archived: false, hidden: false, retained: false };
    let agents = AGENT_ICON_ORDER.iter().filter(|a| a.id == native).copied().collect::<Vec<_>>();
    let roots = BTreeSet::from([project.clone()]);
    let selected = BTreeSet::from([native.to_owned()]);
    let plan = |action| {
        let mut skill = skill.clone();
        skill.key = format!("file:{}", source.join("SKILL.md").canonicalize().unwrap().display());
        let keys = BTreeSet::from([skill.key.clone()]);
        Catalog::scan(vec![skill], vec![entry.clone()], agents.clone(), None)
            .plan(action, &keys, &selected, &roots, &BTreeSet::new()).unwrap()
    };
    let applied = plan(Action::Install).execute();
    assert_eq!((applied.changed(), applied.failed()), (1, 0));
    let paths = managed_entries(&project, &format!(".{native}"));
    assert_eq!(paths.len(), 1);
    run(&project, &source, &paths[0], &plan);
}

fn pi_config_parts(text: &str) -> (&str, serde_json::Value, &str) {
    let suffix = ");\n}\n";
    let marker = if text.starts_with("// Pleat managed OMP") { "ompAdaptation(omp, " } else { "piAdaptation(pi, " };
    let offset = text.rfind(marker).unwrap() + marker.len();
    let (prefix, tail) = text.split_at(offset);
    (prefix, serde_json::from_str(tail.strip_suffix(suffix).unwrap()).unwrap(), suffix)
}

#[test]
fn pi_omp_generated_json_orders_keep_original_bytes_inode_and_removal_cas() {
    use std::os::unix::fs::{MetadataExt, symlink};
    for (native, kind) in [("pi", AgentKind::Pi), ("omp", AgentKind::Omp)] {
        for order in PI_CONFIG_ORDERS {
            pi_json_fixture(native, kind, |project, source, path, plan| {
                let generated = fs::read_to_string(path).unwrap();
                let (prefix, value, suffix) = pi_config_parts(&generated);
                assert_eq!(generated, format!("{prefix}{}{suffix}", pi_config_fixture(PI_CONFIG_ORDERS[0], &value)));
                let bytes = format!("{prefix}{}{suffix}", pi_config_fixture(order, &value)).into_bytes();
                fs::write(path, &bytes).unwrap();
                let before = fs::metadata(path).unwrap();
                // A storage compatibility alias keeps the same source directory.
                let moved = source.with_file_name("moved source");
                fs::rename(source, &moved).unwrap();
                symlink(&moved, source).unwrap();
                let reinstall = plan(Action::Install);
                assert_eq!(reinstall.count(kitter::cross_project::Disposition::Unchanged), 1);
                assert_eq!(reinstall.execute().failed(), 0);
                let after = fs::metadata(path).unwrap();
                assert_eq!((after.dev(), after.ino(), after.mode()), (before.dev(), before.ino(), before.mode()));
                assert_eq!(fs::read(path).unwrap(), bytes);
                let remove = plan(Action::Remove);
                assert_eq!(remove.count(kitter::cross_project::Disposition::Change), 1);
                let edited = [bytes.as_slice(), b"// concurrent edit\n"].concat();
                fs::write(path, &edited).unwrap();
                assert_eq!(remove.execute().failed(), 1);
                assert_eq!(fs::read(path).unwrap(), edited);
                assert!(project.join(format!(".{native}/skills/probe")).is_symlink());
                fs::write(path, &bytes).unwrap();
                let result = plan(Action::Remove).execute();
                assert_eq!((result.changed(), result.failed()), (1, 0));
                assert!(!path.exists());
                let recovered = fs::read_dir(path.parent().unwrap().parent().unwrap()).unwrap().flatten()
                    .map(|e| e.path().join("instruction.md")).find(|p| p.is_file()).unwrap();
                assert_eq!(fs::read(&recovered).unwrap(), bytes);
                assert_eq!(fs::metadata(recovered).unwrap().ino(), before.ino());
                assert_eq!(fs::read_to_string(moved.join("local-data/ADAPTATION.md")).unwrap(), "Keep adaptation");
            });
        }
    }
}

#[test]
fn pi_omp_unknown_json_or_javascript_still_blocks_removal() {
    use std::os::unix::fs::MetadataExt;
    for (native, kind) in [("pi", AgentKind::Pi), ("omp", AgentKind::Omp)] {
        pi_json_fixture(native, kind, |project, source, path, plan| {
            let generated = fs::read_to_string(path).unwrap();
            let (prefix, value, suffix) = pi_config_parts(&generated);
            let canonical = pi_config_fixture(PI_CONFIG_ORDERS[0], &value);
            let reader_alias = project.join("reader-alias");
            std::os::unix::fs::symlink(std::env::current_exe().unwrap().canonicalize().unwrap(), &reader_alias).unwrap();
            let mut cases = vec![
                format!("{prefix}{}{suffix}", serde_json::to_string_pretty(&value).unwrap()),
                format!("{prefix}{canonical} {suffix}"),
                format!("{prefix}{canonical}{}", suffix.trim_end()),
                format!("{prefix}{}{suffix}", canonical.replace('/', "\\/")),
                format!("{prefix}{}{suffix}", canonical.replace("技能", "\\u6280\\u80fd")),
                format!("{prefix}{}{suffix}", canonical.replacen("\"schemaVersion\":1", "\"schemaVersion\":1,\"schemaVersion\":1", 1)),
                format!("{prefix}{}{suffix}", canonical.replacen("\"schemaVersion\":1", "\"extra\":true,\"schemaVersion\":1", 1)),
                format!("{prefix}{}{suffix}", canonical.replacen("\"schemaVersion\":1", "\"schemaVersion\":2", 1)),
                format!("{prefix}{{\"readerExecutable\":{},\"manifest\":{}}}{suffix}", value["readerExecutable"], value["manifest"]),
                format!("// foreign code\n{generated}"),
                format!("{generated}\n// foreign suffix\n"),
            ];
            for (pointer, replacement) in [
                ("/manifest/skills/0/sourceIdentity", serde_json::json!("unknown-identity")),
                ("/manifest/skills/0/installationSkillPath", serde_json::json!(source.join("SKILL.md"))),
                ("/manifest/skills/0/canonicalSourceRoot", serde_json::json!(project)),
                ("/manifest/skills/0/canonicalSourceRoot", serde_json::json!("relative/source")),
                ("/manifest/skills/0/nativeName", serde_json::json!("another-skill")),
                ("/readerExecutable", serde_json::json!("relative-reader")),
                ("/readerExecutable", serde_json::json!("/unknown\nreader")),
                ("/readerExecutable", serde_json::json!(project.join("missing-reader"))),
                ("/readerExecutable", serde_json::json!(project)),
                ("/readerExecutable", serde_json::json!(reader_alias)),
            ] {
                let mut changed = value.clone();
                *changed.pointer_mut(pointer).unwrap() = replacement;
                for order in PI_CONFIG_ORDERS {
                    cases.push(format!("{prefix}{}{suffix}", pi_config_fixture(order, &changed)));
                }
            }
            for edited in cases {
                fs::write(path, edited.as_bytes()).unwrap();
                let before = fs::metadata(path).unwrap();
                let remove = plan(Action::Remove);
                assert_eq!(remove.count(kitter::cross_project::Disposition::Protected), 1);
                let result = remove.execute();
                assert_eq!(result.changed(), 0, "{native}: unknown encoding was removed");
                assert_eq!(fs::read(path).unwrap(), edited.as_bytes());
                assert_eq!(fs::metadata(path).unwrap().ino(), before.ino());
                assert!(project.join(format!(".{native}/skills/probe")).is_symlink());
                assert_eq!(fs::read_to_string(source.join("local-data/ADAPTATION.md")).unwrap(), "Keep adaptation");
            }
        });
    }
}
