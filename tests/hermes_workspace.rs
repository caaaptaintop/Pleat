//! Isolated Pleat contracts; no Hermes process, model or shared HOME is used.
use kitter::{
    InstallTarget,
    agents::{self, AGENT_ICON_ORDER},
    cross_project::{Action, Catalog, Disposition, Skill},
    effective_skills::{self, AgentKind, SkillVisibility},
    project_discovery::{self, ProjectEntry},
    proma_batch,
};
use serde_json::json;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

struct Fixture {
    home: PathBuf,
    profile: PathBuf,
    repo: PathBuf,
    nested: PathBuf,
    source: PathBuf,
}
impl Fixture {
    fn new(root: PathBuf) -> Self {
        let home = root.join("home");
        let profile = home.join(".hermes/profiles/work");
        let repo = root.join("repo");
        let nested = repo.join("sub/folder");
        let source = root.join("library/probe");
        for path in [&home, &profile, &nested, &source, &repo.join(".git")] {
            fs::create_dir_all(path).unwrap();
        }
        skill(&source, "probe", "original");
        fs::write(profile.join("config.yaml"), "{}\n").unwrap();
        Self {
            home,
            profile,
            repo,
            nested,
            source,
        }
    }
    fn configure(&self, trusted: &[&Path], enabled: bool) {
        fs::write(
            self.profile.join("config.yaml"),
            serde_json::to_vec(&json!({"skills": {
            "trusted_project_dirs": trusted, "project_discovery": enabled }}))
            .unwrap(),
        )
        .unwrap();
    }
    fn assert_config_preserved_with_plugin(&self, before: &[u8], enabled: bool) {
        let original: serde_yaml::Value = serde_yaml::from_slice(before).unwrap();
        let actual: serde_yaml::Value = serde_yaml::from_slice(&fs::read(self.profile.join("config.yaml")).unwrap()).unwrap();
        let mut rest=actual.as_mapping().unwrap().clone();
        let plugins=rest.remove(serde_yaml::Value::String("plugins".into())).unwrap();
        assert!(original.get("plugins").is_none(), "Fixture must start without plugin settings");
        assert!(serde_yaml::Value::Mapping(rest)==original, "Unrelated native config values changed");
        let plugins=plugins.as_mapping().unwrap();
        assert!(plugins.len()==1, "Unexpected plugin config changes");
        let values=plugins.get(serde_yaml::Value::String("enabled".into())).unwrap().as_sequence().unwrap();
        let expected=if enabled { vec![serde_yaml::Value::String("pleat-native-skills".into())] } else { vec![] };
        assert!(values==&expected, "Managed native plugin enable state differs");
    }
    fn estimates(&self, cwd: &Path) -> effective_skills::AgentContextEstimate {
        effective_skills::estimate_project_with_home(cwd, &self.home)
            .into_iter()
            .find(|e| e.agent == AgentKind::Hermes)
            .unwrap()
    }
    fn project(&self, path: &Path) -> ProjectEntry {
        ProjectEntry {
            path: path.to_path_buf(),
            name: "fixture".into(),
            agents: vec![AgentKind::Hermes],
            proma_workspaces: Vec::new(),
            native_workspaces: Vec::new(),
            archive_states: Vec::new(),
            archived: false,
            hidden: false,
            retained: false,
        }
    }
    fn catalog(&self, projects: Vec<ProjectEntry>) -> Catalog {
        Catalog::scan(
            vec![Skill {
                key: proma_batch::source_key(&self.source.join("SKILL.md")),
                name: "probe".into(),
                source: self.source.clone(),
                provider: "library",
                install_name: Some("probe".into()),
            }],
            projects,
            AGENT_ICON_ORDER
                .iter()
                .filter(|a| a.id == "hermes")
                .copied()
                .collect(),
            None,
        )
    }
    fn plan(
        &self,
        catalog: &Catalog,
        action: Action,
        project: &Path,
    ) -> kitter::cross_project::Plan {
        catalog
            .plan(
                action,
                &BTreeSet::from([proma_batch::source_key(&self.source.join("SKILL.md"))]),
                &BTreeSet::from(["hermes".into()]),
                &BTreeSet::from([project.to_path_buf()]),
                &BTreeSet::new(),
            )
            .unwrap()
    }
    fn native(&self) -> serde_json::Value {
        let output = Command::new(env!("CARGO_BIN_EXE_kitter"))
            .args(["projects", "--json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn db(&self) -> rusqlite::Connection {
        let connection = rusqlite::Connection::open(self.profile.join("projects.db")).unwrap();
        connection.execute_batch("CREATE TABLE projects(id TEXT PRIMARY KEY, name TEXT NOT NULL DEFAULT '', primary_path TEXT, archived INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE project_folders(project_id TEXT, path TEXT, is_primary INTEGER, added_at INTEGER);").unwrap();
        connection
            .execute(
                "INSERT INTO projects(id,name,primary_path,archived) VALUES('native-one', 'Native multi folder', ?1, 0)",
                [self.repo.to_str().unwrap()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO project_folders VALUES('native-one', ?1, 1, 1)",
                [self.repo.to_str().unwrap()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO project_folders VALUES('native-one', ?1, 0, 2)",
                [self.nested.to_str().unwrap()],
            )
            .unwrap();
        connection
    }
}

fn skill(dir: &Path, name: &str, body: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {body}\n---\n{body}\n"),
    )
    .unwrap();
}

// Exact compact encodings from the sorted-map and insertion-order producers.
// Literal fixtures keep this regression independent of serde_json's map feature.
const HERMES_DESCRIPTOR_ORDERS: [&str; 2] = [
    concat!(r#"{"manager":"pleat-native-hermes-v1","schemaVersion":1,"skills":[{"canonicalSourceRoot":@source@,"installationSkillPath":@installation@,"nativeName":@name@,"readerExecutable":@reader@,"sourceIdentity":@identity@}]}"#, "\n"),
    concat!(r#"{"schemaVersion":1,"manager":"pleat-native-hermes-v1","skills":[{"installationSkillPath":@installation@,"canonicalSourceRoot":@source@,"sourceIdentity":@identity@,"nativeName":@name@,"readerExecutable":@reader@}]}"#, "\n"),
];

fn hermes_descriptor_fixture(template: &str, value: &serde_json::Value) -> Vec<u8> {
    let entry = &value["skills"][0];
    [("@source@", &entry["canonicalSourceRoot"]),
        ("@installation@", &entry["installationSkillPath"]), ("@name@", &entry["nativeName"]),
        ("@identity@", &entry["sourceIdentity"]), ("@reader@", &entry["readerExecutable"])]
        .into_iter().fold(template.to_owned(), |text, (token, value)|
            text.replace(token, &serde_json::to_string(value).unwrap())).into_bytes()
}

fn hermes_only_record(f: &Fixture) -> PathBuf {
    let paths = fs::read_dir(f.profile.join("plugins/pleat-native-skills/registrations")).unwrap()
        .flatten().map(|entry| entry.path()).filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect::<Vec<_>>();
    assert_eq!(paths.len(), 1);
    paths[0].clone()
}

#[cfg(all(unix, any(target_os = "macos", target_os = "linux")))]
#[test]
fn hermes_generated_json_orders_keep_original_bytes_inode_and_removal_cas() {
    use std::os::unix::fs::MetadataExt;
    isolated("hermes_generated_json_orders_keep_original_bytes_inode_and_removal_cas", |f| {
        f.configure(&[&f.repo], true);
        fs::create_dir_all(f.source.join("local-data")).unwrap();
        fs::write(f.source.join("local-data/ADAPTATION.md"), "Keep adaptation").unwrap();
        for order in HERMES_DESCRIPTOR_ORDERS {
            let catalog = f.catalog(vec![f.project(&f.nested)]);
            let result = f.plan(&catalog, Action::Install, &f.nested).execute();
            assert_eq!((result.changed(), result.failed()), (1, 0));
            let path = hermes_only_record(&f);
            let generated = fs::read(&path).unwrap();
            let value = serde_json::from_slice(&generated).unwrap();
            assert_eq!(generated, hermes_descriptor_fixture(HERMES_DESCRIPTOR_ORDERS[0], &value));
            let bytes = hermes_descriptor_fixture(order, &value);
            fs::write(&path, &bytes).unwrap();
            let before = fs::metadata(&path).unwrap();
            let reinstall = f.plan(&f.catalog(vec![f.project(&f.nested)]), Action::Install, &f.nested);
            assert_eq!(reinstall.count(Disposition::Unchanged), 1);
            assert_eq!(reinstall.execute().failed(), 0);
            let after = fs::metadata(&path).unwrap();
            assert_eq!((after.dev(), after.ino(), after.mode()), (before.dev(), before.ino(), before.mode()));
            assert_eq!(fs::read(&path).unwrap(), bytes);
            let remove = f.plan(&f.catalog(vec![f.project(&f.nested)]), Action::Remove, &f.nested);
            assert_eq!(remove.count(Disposition::Change), 1);
            let edited = [bytes.as_slice(), b" "].concat();
            fs::write(&path, &edited).unwrap();
            assert_eq!(remove.execute().failed(), 1);
            assert_eq!(fs::read(&path).unwrap(), edited);
            assert!(f.repo.join(".hermes/skills/probe").is_symlink());
            fs::write(&path, &bytes).unwrap();
            let result = f.plan(&f.catalog(vec![f.project(&f.nested)]), Action::Remove, &f.nested).execute();
            assert_eq!((result.changed(), result.failed()), (1, 0));
            assert!(!path.exists());
            let recovered = fs::read_dir(path.parent().unwrap().parent().unwrap()).unwrap().flatten()
                .map(|e| e.path().join("instruction.md")).find(|p| p.is_file() && fs::read(p).unwrap() == bytes).unwrap();
            assert_eq!(fs::metadata(recovered).unwrap().ino(), before.ino());
            assert!(!f.profile.join("plugins/pleat-native-skills/adapter.py").exists());
            assert_eq!(fs::read_to_string(f.source.join("local-data/ADAPTATION.md")).unwrap(), "Keep adaptation");
        }
    });
}

#[cfg(all(unix, any(target_os = "macos", target_os = "linux")))]
#[test]
fn hermes_unknown_json_encoding_or_identity_still_blocks_removal() {
    use std::os::unix::fs::MetadataExt;
    isolated("hermes_unknown_json_encoding_or_identity_still_blocks_removal", |f| {
        f.configure(&[&f.repo], true);
        let result = f.plan(&f.catalog(vec![f.project(&f.nested)]), Action::Install, &f.nested).execute();
        assert_eq!((result.changed(), result.failed()), (1, 0));
        let path = hermes_only_record(&f);
        let generated = fs::read(&path).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&generated).unwrap();
        let canonical = String::from_utf8(hermes_descriptor_fixture(HERMES_DESCRIPTOR_ORDERS[0], &value)).unwrap();
        let reader_alias = f.profile.join("reader-alias");
        std::os::unix::fs::symlink(std::env::current_exe().unwrap().canonicalize().unwrap(), &reader_alias).unwrap();
        let mut cases = vec![
            serde_json::to_string_pretty(&value).unwrap() + "\n",
            canonical.clone() + " ",
            canonical.trim_end().to_owned(),
            canonical.replace('/', "\\/"),
            canonical.replacen("\"schemaVersion\":1", "\"schemaVersion\":1,\"schemaVersion\":1", 1),
            canonical.replacen("\"schemaVersion\":1", "\"extra\":true,\"schemaVersion\":1", 1),
            canonical.replacen("pleat-native-hermes-v1", "unknown-manager", 1),
            canonical.replacen("\"schemaVersion\":1", "\"schemaVersion\":2", 1),
            format!("{{\"skills\":{},\"schemaVersion\":1,\"manager\":\"pleat-native-hermes-v1\"}}\n", value["skills"]),
        ];
        for (pointer, replacement) in [
            ("/skills/0/sourceIdentity", json!("unknown-identity")),
            ("/skills/0/installationSkillPath", json!(f.source.join("SKILL.md"))),
            ("/skills/0/canonicalSourceRoot", json!(f.repo)),
            ("/skills/0/canonicalSourceRoot", json!("relative/source")),
            ("/skills/0/nativeName", json!("another-skill")),
            ("/skills/0/readerExecutable", json!("relative-reader")),
            ("/skills/0/readerExecutable", json!("/unknown\nreader")),
            ("/skills/0/readerExecutable", json!(f.profile.join("missing-reader"))),
            ("/skills/0/readerExecutable", json!(f.profile)),
            ("/skills/0/readerExecutable", json!(reader_alias)),
        ] {
            let mut changed = value.clone();
            *changed.pointer_mut(pointer).unwrap() = replacement;
            for order in HERMES_DESCRIPTOR_ORDERS {
                cases.push(String::from_utf8(hermes_descriptor_fixture(order, &changed)).unwrap());
            }
        }
        let config = fs::read(f.profile.join("config.yaml")).unwrap();
        for edited in cases {
            fs::write(&path, edited.as_bytes()).unwrap();
            let before = fs::metadata(&path).unwrap();
            let remove = f.plan(&f.catalog(vec![f.project(&f.nested)]), Action::Remove, &f.nested);
            assert_eq!(remove.count(Disposition::Protected), 1);
            assert_eq!(remove.execute().changed(), 0);
            assert_eq!(fs::read(&path).unwrap(), edited.as_bytes());
            assert_eq!(fs::metadata(&path).unwrap().ino(), before.ino());
            assert!(f.repo.join(".hermes/skills/probe").is_symlink());
            assert!(f.profile.join("plugins/pleat-native-skills/adapter.py").is_file());
            assert_eq!(fs::read(f.profile.join("config.yaml")).unwrap(), config);
        }
    });
}

fn isolated(name: &str, run: impl FnOnce(Fixture)) {
    if std::env::var("PLEAT_HER_CHILD").as_deref() != Ok(name) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .env_clear()
            .args(["--exact", name, "--nocapture"])
            .env("PLEAT_HER_CHILD", name)
            .env("PLEAT_HER_ROOT", &root)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("PATH", root.join("bin"))
            .env("HERMES_HOME", "$HOME/.hermes/profiles/work")
            .env("HERMES_DATA_DIR_SUFFIX", "")
            .env("KITTER_HOME", root.join("pleat-data"))
            .env("KITTER_CODEX_PROJECTS_HOME", home.join(".codex"))
            .env("KITTER_PROMA_HOME", home.join(".proma"))
            .env("OPENCLAW_STATE_DIR", home.join(".openclaw"))
            .env("XDG_CONFIG_HOME", home.join(".config"));
        if name == "hermes_sticky_profile_global_target" {
            command.env_remove("HERMES_HOME");
        }
        if name == "hermes_explicit_root_with_sticky_profile_is_unknown" {
            command.env("HERMES_HOME", home.join(".hermes"));
        }
        if name == "hermes_disabled_names_share_native_normalization" {
            command.env("HERMES_PLATFORM", "terminal");
        }
        #[cfg(windows)]
        if let Some(value) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", value);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{name}:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    run(Fixture::new(PathBuf::from(
        std::env::var_os("PLEAT_HER_ROOT").unwrap(),
    )));
}

#[test]
fn hermes_sticky_profile_global_target() {
    isolated("hermes_sticky_profile_global_target", |f| {
        #[cfg(not(windows))]
        let root = f.home.join(".hermes");
        #[cfg(windows)]
        let root = f.home.join("AppData/Local/hermes");
        let profile = root.join("profiles/work");
        fs::create_dir_all(&profile).unwrap();
        fs::write(root.join("active_profile"), "work\n").unwrap();
        fs::write(profile.join("config.yaml"), "{}\n").unwrap();
        assert_eq!(
            agents::global_target_root(&f.home, InstallTarget::Hermes).unwrap(),
            profile.join("skills")
        );
        assert!(agents::PROJECT_INSTALL_TARGETS.contains(&InstallTarget::Hermes));
        assert!(!AgentKind::Hermes.is_global_only());
        let info = AGENT_ICON_ORDER.iter().find(|a| a.id == "hermes").unwrap();
        assert!(!info.global_only && info.owns_target(InstallTarget::Hermes) && info.shared_agents);
    });
}

#[test]
fn hermes_trust_exact_gitroot_and_profile_roots() {
    isolated("hermes_trust_exact_gitroot_and_profile_roots", |f| {
        skill(&f.profile.join("skills/local"), "local", "local");
        skill(&f.profile.join("created/create"), "create", "create");
        skill(&f.profile.join("external/extra"), "extra", "extra");
        skill(&f.repo.join(".hermes/skills/project"), "project", "project");
        skill(&f.repo.join(".agents/skills/shared"), "shared", "shared");
        skill(&f.nested.join(".hermes/skills/wrong"), "wrong", "wrong");
        f.configure(&[&f.nested], true);
        assert!(
            agents::validate_hermes_project(&f.nested).is_err(),
            "a trusted subfolder is not a trusted git root"
        );
        let denied = f.estimates(&f.nested);
        assert!(!denied.skills.iter().any(|s| s.name == "project"));
        fs::write(f.profile.join("config.yaml"), serde_json::to_vec(&json!({"skills": {
            "trusted_project_dirs": [f.repo], "create_dir": "created", "external_dirs": "external"
        }})).unwrap()).unwrap();
        assert_eq!(
            agents::validate_hermes_project(&f.nested).unwrap(),
            f.repo.join(".hermes/skills")
        );
        assert_eq!(
            agents::installation_root(&f.nested, InstallTarget::Hermes).unwrap(),
            f.repo.join(".hermes/skills")
        );
        let estimate = f.estimates(&f.nested);
        for name in ["project", "shared", "local", "create", "extra"] {
            assert!(estimate.skills.iter().any(|s| s.name == name), "{name}");
        }
        assert!(!estimate.skills.iter().any(|s| s.name == "wrong"));
        assert_eq!(
            estimate.diagnostic.unwrap().code,
            "hermes_native_eligibility_unknown"
        );
        assert!(
            !f.estimates(&f.home)
                .skills
                .iter()
                .any(|s| s.name == "project")
        );
        f.configure(&[&f.repo], false);
        assert!(agents::validate_hermes_project(&f.nested).is_err());
        assert!(
            !f.estimates(&f.nested)
                .skills
                .iter()
                .any(|s| s.name == "project")
        );
    });
}

#[test]
fn hermes_preview_apply_rechecks_trust_and_preserves_source() {
    isolated(
        "hermes_preview_apply_rechecks_trust_and_preserves_source",
        |f| {
            let catalog = f.catalog(vec![f.project(&f.nested)]);
            f.configure(&[], true);
            assert_eq!(
                f.plan(&catalog, Action::Install, &f.nested)
                    .count(Disposition::Protected),
                1
            );
            f.configure(&[&f.repo], true);
            let original_config = fs::read(f.profile.join("config.yaml")).unwrap();
            let plan = f.plan(&catalog, Action::Install, &f.nested);
            assert_eq!(plan.count(Disposition::Change), 1);
            f.configure(&[], true);
            let result = plan.execute();
            assert_eq!((result.changed(), result.failed()), (0, 1));
            assert!(!f.repo.join(".hermes/skills/probe").exists());
            f.configure(&[&f.repo], true);
            let result = f.plan(&catalog, Action::Install, &f.nested).execute();
            assert_eq!((result.changed(), result.failed()), (1, 0));
            assert_eq!(
                f.repo.join(".hermes/skills/probe").canonicalize().unwrap(),
                f.source
            );
            f.assert_config_preserved_with_plugin(&original_config, true);
            let installed = f.catalog(vec![f.project(&f.nested)]);
            assert_eq!(
                f.plan(&installed, Action::Remove, &f.nested)
                    .execute()
                    .changed(),
                1
            );
            assert!(f.source.join("SKILL.md").is_file());
        },
    );
}

#[test]
fn hermes_git_scope_changed_after_preview_is_protected() {
    isolated("hermes_git_scope_changed_after_preview_is_protected", |f| {
        f.configure(&[&f.repo, &f.nested], true);
        let catalog = f.catalog(vec![f.project(&f.nested)]);
        let plan = f.plan(&catalog, Action::Install, &f.nested);
        assert_eq!(plan.count(Disposition::Change), 1);
        fs::write(f.nested.join(".git"), "gitdir: isolated-worktree").unwrap();
        assert_eq!(plan.execute().failed(), 1);
        assert!(!f.repo.join(".hermes/skills/probe").exists());
        assert!(!f.nested.join(".hermes/skills/probe").exists());
    });
}

#[test]
fn hermes_direct_install_requires_trust_and_uses_native_git_root() {
    isolated(
        "hermes_direct_install_requires_trust_and_uses_native_git_root",
        |f| {
            f.configure(&[], true);
            assert!(
                kitter::project::install_from_path(
                    &f.nested,
                    &f.source,
                    "probe",
                    &[InstallTarget::Hermes]
                )
                .is_err()
            );
            assert!(!f.repo.join(".hermes/skills/probe").exists());
            f.configure(&[&f.repo], true);
            let config = fs::read(f.profile.join("config.yaml")).unwrap();
            kitter::project::install_from_path(
                &f.nested,
                &f.source,
                "probe",
                &[InstallTarget::Hermes],
            )
            .unwrap();
            assert_eq!(
                f.repo.join(".hermes/skills/probe").canonicalize().unwrap(),
                f.source
            );
            assert!(!f.nested.join(".hermes/skills/probe").exists());
            f.assert_config_preserved_with_plugin(&config, true);
        },
    );
}

#[test]
fn hermes_unreadable_or_invalid_config_is_unknown() {
    isolated("hermes_unreadable_or_invalid_config_is_unknown", |f| {
        fs::write(f.profile.join("config.yaml"), "skills: [unterminated").unwrap();
        let estimate = f.estimates(&f.repo);
        assert_eq!(
            estimate.diagnostic.as_ref().unwrap().code,
            "hermes_source_unknown"
        );
        assert!(serde_json::to_value(estimate).unwrap()["discovered_count"].is_null());
        assert!(agents::validate_hermes_project(&f.repo).is_err());
        assert_eq!(
            f.plan(
                &f.catalog(vec![f.project(&f.repo)]),
                Action::Install,
                &f.repo
            )
            .count(Disposition::Protected),
            1
        );
    });
}

#[test]
fn hermes_native_multifolder_identity_uses_product_metadata_reader() {
    isolated(
        "hermes_native_multifolder_identity_uses_product_metadata_reader",
        |f| {
            f.configure(&[&f.repo], true);
            let db = f.db();
            let before = fs::read(f.profile.join("projects.db")).unwrap();
            let projects = f.native();
            let projects = projects.as_array().unwrap();
            assert_eq!(projects.len(), 2);
            let identities = projects
                .iter()
                .map(|p| p["native_workspaces"][0]["source_id"].as_str().unwrap())
                .collect::<BTreeSet<_>>();
            assert_eq!(identities, BTreeSet::from(["native-one"]));
            assert_eq!(
                fs::read(f.profile.join("projects.db")).unwrap(),
                before,
                "catalog read must not modify native DB"
            );
            assert_ne!(projects[0]["path"], projects[1]["path"]);
            assert_eq!(
                agents::installation_root(&f.repo, InstallTarget::Hermes).unwrap(),
                agents::installation_root(&f.nested, InstallTarget::Hermes).unwrap()
            );
            db.execute("UPDATE projects SET id='changed-id'", [])
                .unwrap();
            db.execute("UPDATE project_folders SET project_id='changed-id'", [])
                .unwrap();
            assert!(
                f.native()
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|p| p["native_workspaces"][0]["source_id"] == "changed-id")
            );
        },
    );
}

#[test]
fn hermes_catalog_filters_support_disabled_and_same_tier_conflicts() {
    isolated(
        "hermes_catalog_filters_support_disabled_and_same_tier_conflicts",
        |f| {
            f.configure(&[], true);
            let root = f.profile.join("skills");
            skill(&root.join("first"), "duplicate", "one");
            skill(&root.join("second"), "duplicate", "two");
            skill(&root.join("first/references/support"), "support", "hidden");
            skill(&root.join(".archive/old"), "archived", "hidden");
            skill(&root.join("off"), "off", "disabled");
            fs::write(
                f.profile.join("config.yaml"),
                "skills:\n  disabled: '[\"off\"]'\n",
            )
            .unwrap();
            let estimate = f.estimates(&f.repo);
            assert!(
                !estimate
                    .skills
                    .iter()
                    .any(|s| ["support", "archived", "off"].contains(&s.name.as_str()))
            );
            let duplicates = estimate
                .skills
                .iter()
                .filter(|s| s.name == "duplicate")
                .collect::<Vec<_>>();
            assert_eq!(duplicates.len(), 2);
            assert!(
                duplicates
                    .iter()
                    .all(|s| s.visibility != SkillVisibility::Shadowed)
            );
            assert_eq!(
                duplicates
                    .iter()
                    .map(|s| s.id.as_str())
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from(["first", "second"])
            );
        },
    );
}

#[test]
fn hermes_explicit_root_with_sticky_profile_is_unknown() {
    isolated("hermes_explicit_root_with_sticky_profile_is_unknown", |f| {
        fs::write(f.home.join(".hermes/active_profile"), "work\n").unwrap();
        assert!(agents::global_target_root(&f.home, InstallTarget::Hermes).is_err());
        assert_eq!(
            f.estimates(&f.repo).diagnostic.unwrap().code,
            "hermes_source_unknown"
        );
    });
}

#[cfg(unix)]
#[test]
fn hermes_project_alias_cannot_mutate_profile_skills() {
    isolated("hermes_project_alias_cannot_mutate_profile_skills", |f| {
        f.configure(&[&f.repo], true);
        std::os::unix::fs::symlink(&f.profile, f.repo.join(".hermes")).unwrap();
        assert!(agents::validate_hermes_project(&f.repo).is_err());
        let plan = f.plan(
            &f.catalog(vec![f.project(&f.repo)]),
            Action::Install,
            &f.repo,
        );
        assert_eq!(plan.count(Disposition::Protected), 1);
        assert_eq!(plan.execute().changed(), 0);
        assert!(!f.profile.join("skills/probe").exists());
    });
}

#[cfg(unix)]
#[test]
fn hermes_global_root_and_subtree_aliases_are_protected() {
    isolated(
        "hermes_global_root_and_subtree_aliases_are_protected",
        |f| {
            use std::os::unix::fs::symlink;
            let original_source = fs::read(f.source.join("SKILL.md")).unwrap();
            for route in ["private", "shared", "both"] {
                for subtree in [false, true] {
                    let repo = f.repo.join(format!("{route}-{subtree}"));
                    fs::create_dir_all(repo.join(".git")).unwrap();
                    let global = f.profile.join("skills");
                    let target = if subtree {
                        global.join("category")
                    } else {
                        global.clone()
                    };
                    fs::create_dir_all(&target).unwrap();
                    let link = target.join("probe");
                    if link.symlink_metadata().is_err() {
                        symlink(&f.source, &link).unwrap();
                    }
                    for sub in [".hermes/skills", ".agents/skills"] {
                        let root = repo.join(sub);
                        fs::create_dir_all(root.parent().unwrap()).unwrap();
                        if route == "both"
                            || (route == "private" && sub == ".hermes/skills")
                            || (route == "shared" && sub == ".agents/skills")
                        {
                            symlink(&target, &root).unwrap();
                        } else {
                            fs::create_dir_all(&root).unwrap();
                        }
                    }
                    f.configure(&[&repo], true);
                    let config = fs::read(f.profile.join("config.yaml")).unwrap();
                    let before = fs::read_link(&link).unwrap();
                    let catalog = f.catalog(vec![f.project(&repo)]);
                    let remove = f.plan(&catalog, Action::Remove, &repo);
                    assert!(
                        remove.count(Disposition::Protected) > 0,
                        "{route} subtree={subtree}"
                    );
                    assert_eq!(remove.execute().changed(), 0);
                    if route != "shared" {
                        assert!(
                            kitter::project::install_from_path(
                                &repo,
                                &f.source,
                                "probe",
                                &[InstallTarget::Hermes]
                            )
                            .is_err()
                        );
                        let install = f.plan(&catalog, Action::Install, &repo);
                        assert_eq!(install.count(Disposition::Protected), 1);
                        assert_eq!(install.execute().changed(), 0);
                    }
                    assert_eq!(fs::read_link(&link).unwrap(), before);
                    assert_eq!(fs::read(f.profile.join("config.yaml")).unwrap(), config);
                    assert_eq!(
                        fs::read(f.source.join("SKILL.md")).unwrap(),
                        original_source
                    );
                }
            }
        },
    );
}

#[cfg(unix)]
#[test]
fn hermes_alias_retarget_after_preview_cannot_touch_global_links() {
    isolated(
        "hermes_alias_retarget_after_preview_cannot_touch_global_links",
        |f| {
            use std::os::unix::fs::symlink;
            for shared in [false, true] {
                for remove in [false, true] {
                    for destination in ["global-root", "global-child", "non-global"] {
                        if shared && !remove {
                            continue;
                        } // The Hermes install target is its private root.
                        let repo = f
                            .repo
                            .join(format!("retarget-{shared}-{remove}-{destination}"));
                        fs::create_dir_all(repo.join(".git")).unwrap();
                        let outside = repo.join("non-global-root");
                        fs::create_dir_all(&outside).unwrap();
                        let root = repo.join(if shared {
                            ".agents/skills"
                        } else {
                            ".hermes/skills"
                        });
                        fs::create_dir_all(root.parent().unwrap()).unwrap();
                        symlink(&outside, &root).unwrap();
                        if remove {
                            symlink(&f.source, outside.join("probe")).unwrap();
                        }
                        f.configure(&[&repo], true);
                        let config = fs::read(f.profile.join("config.yaml")).unwrap();
                        let action = if remove {
                            Action::Remove
                        } else {
                            Action::Install
                        };
                        let plan = f.plan(&f.catalog(vec![f.project(&repo)]), action, &repo);
                        assert_eq!(plan.count(Disposition::Change), 1);
                        let global = f.profile.join("skills");
                        let target = match destination {
                            "global-root" => global,
                            "global-child" => global.join("category"),
                            _ => repo.join("other-non-global-root"),
                        };
                        fs::create_dir_all(&target).unwrap();
                        let protected = target.join("probe");
                        if protected.symlink_metadata().is_err() {
                            symlink(&f.source, &protected).unwrap();
                        }
                        fs::remove_file(&root).unwrap();
                        symlink(&target, &root).unwrap();
                        assert_eq!(plan.execute().failed(), 1);
                        assert_eq!(fs::read_link(&protected).unwrap(), f.source);
                        assert_eq!(fs::read(f.profile.join("config.yaml")).unwrap(), config);
                        assert!(f.source.join("SKILL.md").is_file());
                        assert_eq!(outside.join("probe").symlink_metadata().is_ok(), remove);
                    }
                }
            }
        },
    );
}

#[cfg(unix)]
#[test]
fn hermes_nonoverlapping_aliases_allow_direct_shared_and_dedup() {
    isolated(
        "hermes_nonoverlapping_aliases_allow_direct_shared_and_dedup",
        |f| {
            use std::os::unix::fs::symlink;
            for whole_hermes in [false, true] {
                for same_physical_root in [false, true] {
                    let repo = f
                        .repo
                        .join(format!("valid-{whole_hermes}-{same_physical_root}"));
                    fs::create_dir_all(repo.join(".git")).unwrap();
                    // Component sibling of profile/skills must remain usable.
                    let alias = f
                        .profile
                        .join("skills-other")
                        .join(format!("{whole_hermes}-{same_physical_root}"));
                    let physical = if whole_hermes {
                        alias.join("skills")
                    } else {
                        alias.clone()
                    };
                    fs::create_dir_all(&physical).unwrap();
                    if whole_hermes {
                        symlink(&alias, repo.join(".hermes")).unwrap();
                    } else {
                        fs::create_dir_all(repo.join(".hermes")).unwrap();
                        symlink(&physical, repo.join(".hermes/skills")).unwrap();
                    }
                    let shared = repo.join(".agents/skills");
                    fs::create_dir_all(shared.parent().unwrap()).unwrap();
                    if same_physical_root {
                        symlink(&physical, &shared).unwrap();
                    } else {
                        fs::create_dir_all(&shared).unwrap();
                        symlink(&f.source, shared.join("probe")).unwrap();
                    }
                    f.configure(&[&repo], true);
                    let config = fs::read(f.profile.join("config.yaml")).unwrap();
                    kitter::project::install_from_path(
                        &repo,
                        &f.source,
                        "probe",
                        &[InstallTarget::Hermes],
                    )
                    .unwrap();
                    assert_eq!(physical.join("probe").canonicalize().unwrap(), f.source);
                    let remove = f.plan(&f.catalog(vec![f.project(&repo)]), Action::Remove, &repo);
                    assert_eq!(
                        remove.count(Disposition::Change),
                        if same_physical_root { 1 } else { 2 }
                    );
                    assert_eq!(
                        remove.execute().changed(),
                        if same_physical_root { 1 } else { 2 }
                    );
                    assert!(!physical.join("probe").exists());
                    assert!(!shared.join("probe").exists());
                    assert!(f.source.join("SKILL.md").is_file());
                    f.assert_config_preserved_with_plugin(&config, false);
                }
            }
        },
    );
}

#[test]
fn hermes_disabled_names_share_native_normalization() {
    isolated("hermes_disabled_names_share_native_normalization", |f| {
        let root = f.profile.join("skills");
        for (path, name) in [
            ("off", "off"),
            ("normal", "normal"),
            ("essential", "hermes-agent"),
            ("group/a", "duplicate"),
            ("group/b", "duplicate"),
        ] {
            skill(&root.join(path), name, "fixture");
        }
        let cases = [
            (json!([" off ", " group/a ", " hermes-agent ", ""]), true),
            (
                json!("[\" off \", \" group/a \", \" hermes-agent \"]"),
                true,
            ),
            (json!("[' off ', ' group/a ', ' hermes-agent ',]"), true),
            (json!(" off "), false),
        ];
        for platform in [false, true] {
            for (value, disabled_alias) in &cases {
                let mut settings = json!({"disabled": ["hermes-agent"], "platform_disabled": {"terminal": ["hermes-agent"]}});
                if platform {
                    settings["platform_disabled"]["terminal"] = value.clone();
                } else {
                    settings["disabled"] = value.clone();
                }
                fs::write(
                    f.profile.join("config.yaml"),
                    serde_json::to_vec(&json!({"skills":settings})).unwrap(),
                )
                .unwrap();
                let estimate = f.estimates(&f.home);
                assert!(estimate.diagnostic.is_none(), "{value} platform={platform}");
                assert!(!estimate.skills.iter().any(|s| s.name == "off"));
                assert_eq!(
                    estimate.skills.iter().any(|s| s.id == "group/a"),
                    !disabled_alias
                );
                for name in ["normal", "hermes-agent"] {
                    assert!(
                        estimate
                            .skills
                            .iter()
                            .any(|s| s.name == name && s.visibility == SkillVisibility::Automatic)
                    );
                }
                assert!(estimate.skills.iter().any(|s| s.id == "group/b"));
            }
            for numeric in [".nan", ".inf", "-.inf", "1.0e+20", "0.00001", "42"] {
                let yaml = if platform {
                    format!("skills:\n  platform_disabled:\n    terminal: [{numeric}]\n")
                } else { format!("skills:\n  disabled: [{numeric}]\n") };
                fs::write(f.profile.join("config.yaml"), yaml).unwrap();
                let estimate = f.estimates(&f.home);
                assert!(estimate.diagnostic.is_some(), "numeric name must be unknown: {numeric} platform={platform}");
                assert!(estimate.skills.iter().all(|s| s.visibility != SkillVisibility::Automatic));
                assert!(serde_json::to_value(estimate).unwrap()["discovered_count"].is_null());
            }
        }
    });
}
