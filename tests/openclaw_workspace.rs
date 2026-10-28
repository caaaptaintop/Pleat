//! Static Pleat contracts against the pinned OpenClaw protocol. These tests
//! spawn only this fixture binary, never OpenClaw, a Gateway, SDK or installer.
#![cfg(any(unix, windows))]

use kitter::{
    InstallTarget,
    agents::{self, AGENT_ICON_ORDER},
    cross_project::{Action, Catalog, Disposition, Skill},
    effective_skills::{self, AgentKind, SkillVisibility},
    project,
    project_discovery::{self, ProjectEntry},
    proma_batch,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

// Exercise the platform link kind without exposing the crate's private module.
mod directory_link {
    pub fn create(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link)
        }
        #[cfg(windows)]
        {
            junction::create(target, link)
        }
    }
}

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    state: PathBuf,
    config: PathBuf,
    workspace: PathBuf,
    source: PathBuf,
}

impl Fixture {
    fn new(root: PathBuf, profile: bool) -> Self {
        let home = root.join("home");
        let effective_home = std::env::var("OPENCLAW_HOME")
            .ok()
            .map(|raw| fixture_user_path(&raw, &home))
            .unwrap_or_else(|| home.clone());
        let state = std::env::var("OPENCLAW_STATE_DIR")
            .ok()
            .map(|raw| fixture_user_path(&raw, &effective_home))
            .unwrap_or_else(|| {
                effective_home.join(if profile {
                    ".openclaw-review"
                } else {
                    ".openclaw"
                })
            });
        let config = std::env::var("OPENCLAW_CONFIG_PATH")
            .ok()
            .map(|raw| fixture_user_path(&raw, &effective_home))
            .unwrap_or_else(|| state.join("openclaw.json"));
        let workspace = root.join("workspace");
        let source = root.join("library/probe");
        for path in [&state, &workspace, &source] {
            fs::create_dir_all(path).unwrap();
        }
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        write_skill(&source, "probe", "v1");
        fs::create_dir_all(source.join("local-data")).unwrap();
        fs::write(source.join("local-data/keep"), "local state").unwrap();
        Self {
            root,
            home,
            state,
            config,
            workspace,
            source,
        }
    }
    fn configure(&self, trust: Value) {
        self.config(
            json!({"agents":{"entries":{"reviewer":{"workspace":self.workspace}}},
            "skills":{"load":{"allowSymlinkTargets":trust}}}),
        );
    }
    fn config(&self, value: Value) {
        fs::write(&self.config, serde_json::to_vec(&value).unwrap()).unwrap();
    }
    fn projects(&self) -> Vec<ProjectEntry> {
        project_discovery::discover_agent_projects()
            .unwrap()
            .into_iter()
            .filter(|entry| entry.agents.contains(&AgentKind::OpenClaw))
            .collect()
    }
    fn catalog(&self, entries: Vec<ProjectEntry>) -> Catalog {
        Catalog::scan(
            vec![self.skill()],
            entries,
            vec![
                *AGENT_ICON_ORDER
                    .iter()
                    .find(|a| a.id == "openclaw")
                    .unwrap(),
            ],
            None,
        )
    }
    fn skill(&self) -> Skill {
        Skill {
            key: proma_batch::source_key(&self.source.join("SKILL.md")),
            name: "probe".into(),
            source: self.source.clone(),
            provider: "library",
            install_name: Some("probe".into()),
        }
    }
    fn visible(&self, cwd: &Path) -> Vec<effective_skills::EffectiveSkill> {
        self.estimate(cwd)
            .skills
            .into_iter()
            .filter(|skill| skill.visibility != SkillVisibility::Shadowed)
            .collect()
    }
    fn estimate(&self, cwd: &Path) -> effective_skills::AgentContextEstimate {
        effective_skills::estimate_project_with_home(cwd, &self.home)
            .into_iter()
            .find(|snapshot| snapshot.agent == AgentKind::OpenClaw)
            .unwrap()
    }
}

fn fixture_user_path(raw: &str, home: &Path) -> PathBuf {
    if raw == "~" {
        home.to_path_buf()
    } else if let Some(suffix) = raw.strip_prefix("~/") {
        home.join(suffix)
    } else {
        PathBuf::from(raw)
    }
}

fn write_skill(dir: &Path, name: &str, description: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\nFixture.\n"),
    )
    .unwrap();
}

fn isolated(name: &str, run: impl FnOnce(&Fixture)) {
    if std::env::var("PLEAT_OCW_CHILD").as_deref() != Ok(name) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .env_clear()
            .args(["--exact", name, "--nocapture"])
            .env("PLEAT_OCW_CHILD", name)
            .env("PLEAT_OCW_ROOT", &root)
            .env("HOME", &home)
            .env("PATH", root.join("bin"))
            .env("USERPROFILE", &home)
            .env("OPENCLAW_CONFIG_PATH", home.join(".openclaw/openclaw.json"))
            .env("OPENCLAW_STATE_DIR", home.join(".openclaw"))
            .env("CODEX_HOME", home.join(".codex"))
            .env("KITTER_CODEX_PROJECTS_HOME", home.join(".codex"))
            .env("KITTER_PROMA_HOME", home.join(".proma"))
            .env("HERMES_HOME", home.join(".hermes"))
            .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
            .env("GROK_HOME", home.join(".grok"))
            .env("WORKBUDDY_CONFIG_DIR", home.join(".workbuddy"))
            .env("XDG_CONFIG_HOME", home.join(".config"));
        for key in ["SystemRoot", "SYSTEMROOT", "WINDIR", "TMP", "TEMP", "TMPDIR"] {
            if let Some(value) = std::env::var_os(key) { command.env(key, value); }
        }
        for key in [
            "OPENCLAW_PROFILE",
            "OPENCLAW_HOME",
            "OPENCLAW_WORKSPACE_DIR",
            "OPENCLAW_BUNDLED_SKILLS_DIR",
            "CLAWDBOT_STATE_DIR",
            "CLAWDBOT_CONFIG_PATH",
            "WORKBUDDY_INSTANCE_NUMBER",
        ] {
            command.env_remove(key);
        }
        if name == "openclaw_profile_keeps_global_root_separate" {
            command
                .env_remove("OPENCLAW_STATE_DIR")
                .env_remove("OPENCLAW_CONFIG_PATH")
                .env("OPENCLAW_PROFILE", "review");
        }
        if name == "openclaw_workspace_env_is_only_a_default" {
            command.env("OPENCLAW_WORKSPACE_DIR", root.join("env-workspace"));
        }
        if name == "openclaw_bundled_plugin_and_extra_roots_keep_native_boundaries" {
            command.env("OPENCLAW_BUNDLED_SKILLS_DIR", root.join("bundled"));
        }
        if name.starts_with("openclaw_review_home_") {
            command
                .env("OPENCLAW_HOME", "~/relocated")
                .env_remove("OPENCLAW_STATE_DIR")
                .env_remove("OPENCLAW_CONFIG_PATH");
            if name.ends_with("profile") {
                command.env("OPENCLAW_PROFILE", "review");
            }
            if name.ends_with("explicit_config") {
                command
                    .env("OPENCLAW_STATE_DIR", "~/chosen-state")
                    .env("OPENCLAW_CONFIG_PATH", "~/chosen-config/openclaw.json");
            }
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
    let fixture = Fixture::new(
        PathBuf::from(std::env::var_os("PLEAT_OCW_ROOT").unwrap()),
        std::env::var("OPENCLAW_PROFILE").as_deref() == Ok("review"),
    );
    run(&fixture);
}

// Requires native adaptation publication or an accepted native-write preview.
#[cfg(unix)]
#[test]
fn openclaw_profile_plugin_scope_and_last_uninstall_preserve_source_data() {
    isolated("openclaw_profile_plugin_scope_and_last_uninstall_preserve_source_data", |f| {
        f.configure(json!([f.source]));
        let source = fs::read(f.source.join("SKILL.md")).unwrap();
        let local = fs::read(f.source.join("local-data/keep")).unwrap();
        for root in [&f.home, &f.workspace] {
            project::install_from_path(root, &f.source, "probe", &[InstallTarget::OpenClaw]).unwrap();
        }
        let plugin = f.state.join("extensions/pleat-openclaw-adaptation");
        let manifest: Value = serde_json::from_slice(&fs::read(plugin.join("openclaw.plugin.json")).unwrap()).unwrap();
        assert_eq!(manifest["contracts"]["tools"], json!(["pleat_read_skill"]));
        let records = plugin.join("registrations");
        let rows = fs::read_dir(&records).unwrap().map(|entry| {
            serde_json::from_slice::<Value>(&fs::read(entry.unwrap().path()).unwrap()).unwrap()
        }).collect::<Vec<_>>();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.iter().map(|r| r["skills"][0]["installationId"].as_str().unwrap()).collect::<BTreeSet<_>>().len(), 2);
        assert!(rows.iter().any(|r| r["skills"][0]["scope"] == "global" && r["skills"][0]["workspaceRoot"].is_null()));
        assert!(rows.iter().any(|r| r["skills"][0]["scope"] == "workspace" && r["skills"][0]["workspaceRoot"] == json!(f.workspace)));
        let config: Value = json5::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
        assert_eq!(config["plugins"]["entries"]["pleat-openclaw-adaptation"]["hooks"]["allowConversationAccess"], true);
        project::uninstall(&f.home, &f.root.join("library"), "probe", &[InstallTarget::OpenClaw]).unwrap();
        assert_eq!(fs::read_dir(&records).unwrap().count(), 1);
        assert!(plugin.join("index.js").is_file() && f.workspace.join("skills/probe/SKILL.md").is_file());
        project::uninstall(&f.workspace, &f.root.join("library"), "probe", &[InstallTarget::OpenClaw]).unwrap();
        assert_eq!(fs::read_dir(&records).unwrap().count(), 0);
        assert!(!plugin.join("index.js").exists() && !plugin.join("reader.json").exists());
        let config: Value = json5::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
        assert!(config["plugins"]["entries"].get("pleat-openclaw-adaptation").is_none());
        assert_eq!(fs::read(f.source.join("SKILL.md")).unwrap(), source);
        assert_eq!(fs::read(f.source.join("local-data/keep")).unwrap(), local);
    });
}

#[test]
fn openclaw_profile_plugin_respects_disabled_policy_and_foreign_files() {
    isolated("openclaw_profile_plugin_respects_disabled_policy_and_foreign_files", |f| {
        let plugin = f.state.join("extensions/pleat-openclaw-adaptation");
        for policy in [json!({"enabled":false}), json!({"deny":["pleat-openclaw-adaptation"]}),
            json!({"entries":{"pleat-openclaw-adaptation":{"enabled":false}}})] {
            f.config(json!({"agents":{"entries":{"reviewer":{"workspace":f.workspace}}},
                "skills":{"load":{"allowSymlinkTargets":[f.source]}},"plugins":policy}));
            let before = fs::read(&f.config).unwrap();
            assert!(project::install_from_path(&f.workspace, &f.source, "probe", &[InstallTarget::OpenClaw]).is_err());
            assert_eq!(fs::read(&f.config).unwrap(), before);
            assert!(!f.workspace.join("skills/probe").exists() && !plugin.exists());
        }
        f.configure(json!([f.source]));
        fs::create_dir_all(&plugin).unwrap(); fs::write(plugin.join("index.js"), "user-owned plugin").unwrap();
        let before = fs::read(&f.config).unwrap();
        assert!(project::install_from_path(&f.workspace, &f.source, "probe", &[InstallTarget::OpenClaw]).is_err());
        assert_eq!(fs::read(&f.config).unwrap(), before);
        assert_eq!(fs::read_to_string(plugin.join("index.js")).unwrap(), "user-owned plugin");
        assert!(!f.workspace.join("skills/probe").exists());
    });
}

fn desktop_ordered_object(value: &Value, keys: &Value) -> String {
    let members = keys.as_array().unwrap().iter().map(|key| {
        let key = key.as_str().unwrap();
        format!("{}:{}", serde_json::to_string(key).unwrap(), serde_json::to_string(&value[key]).unwrap())
    }).collect::<Vec<_>>();
    format!("{{{}}}", members.join(","))
}

fn use_recorded_desktop_json_order(plugin: &Path) {
    // A real 81659ad desktop output fixture, independent of Cargo's JSON feature
    // union. Scalar paths/identities come from this test's own installations.
    let fixture: Value = serde_json::from_str(include_str!("fixtures/openclaw-desktop-json-order.json")).unwrap();
    for record in fs::read_dir(plugin.join("registrations")).unwrap() {
        let path = record.unwrap().path();
        let value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let entry = desktop_ordered_object(&value["skills"][0], &fixture["entry_order"]);
        fs::write(path, format!("{{\"schemaVersion\":1,\"manager\":\"pleat-native-openclaw-v1\",\"skills\":[{entry}]}}\n")).unwrap();
    }
    let reader = plugin.join("reader.json");
    let value: Value = serde_json::from_slice(&fs::read(&reader).unwrap()).unwrap();
    fs::write(reader, desktop_ordered_object(&value, &fixture["reader_order"]) + "\n").unwrap();
    fs::write(plugin.join("openclaw.plugin.json"), fixture["manifest"].as_str().unwrap()).unwrap();
    fs::write(plugin.join("package.json"), fixture["package"].as_str().unwrap()).unwrap();
}

// Requires native adaptation publication or an accepted native-write preview.
#[cfg(unix)]
#[test]
fn openclaw_profile_legacy_desktop_order_preserves_other_records_on_uninstall() {
    isolated("openclaw_profile_legacy_desktop_order_preserves_other_records_on_uninstall", |f| {
        f.configure(json!([f.source]));
        let body = fs::read(f.source.join("SKILL.md")).unwrap();
        let local = fs::read(f.source.join("local-data/keep")).unwrap();
        for root in [&f.home, &f.workspace] {
            project::install_from_path(root, &f.source, "probe", &[InstallTarget::OpenClaw]).unwrap();
        }
        let plugin = f.state.join("extensions/pleat-openclaw-adaptation");
        use_recorded_desktop_json_order(&plugin);
        let before = fs::read_dir(plugin.join("registrations")).unwrap().map(|e| {
            let path = e.unwrap().path(); (path.clone(), fs::read(path).unwrap())
        }).collect::<Vec<_>>();
        project::uninstall(&f.home, &f.root.join("library"), "probe", &[InstallTarget::OpenClaw]).unwrap();
        let retained = before.iter().filter(|(path, _)| path.exists()).collect::<Vec<_>>();
        assert_eq!(retained.len(), 1);
        assert_eq!(fs::read(&retained[0].0).unwrap(), retained[0].1);
        assert!(f.workspace.join("skills/probe/SKILL.md").is_file());
        project::uninstall(&f.workspace, &f.root.join("library"), "probe", &[InstallTarget::OpenClaw]).unwrap();
        assert_eq!(fs::read_dir(plugin.join("registrations")).unwrap().count(), 0);
        assert!(!plugin.join("index.js").exists());
        assert_eq!(fs::read(f.source.join("SKILL.md")).unwrap(), body);
        assert_eq!(fs::read(f.source.join("local-data/keep")).unwrap(), local);
    });
}

// Requires native adaptation publication or an accepted native-write preview.
#[cfg(unix)]
#[test]
fn openclaw_profile_unknown_json_encoding_is_preserved_with_installation() {
    isolated("openclaw_profile_unknown_json_encoding_is_preserved_with_installation", |f| {
        f.configure(json!([f.source]));
        project::install_from_path(&f.workspace, &f.source, "probe", &[InstallTarget::OpenClaw]).unwrap();
        let plugin = f.state.join("extensions/pleat-openclaw-adaptation");
        use_recorded_desktop_json_order(&plugin);
        let path = fs::read_dir(plugin.join("registrations")).unwrap().next().unwrap().unwrap().path();
        let original = fs::read(&path).unwrap();
        for bytes in [serde_json::to_vec_pretty(&serde_json::from_slice::<Value>(&original).unwrap()).unwrap(),
            [original.as_slice(), b" "].concat()] {
            fs::write(&path, &bytes).unwrap();
            assert!(project::uninstall(&f.workspace, &f.root.join("library"), "probe", &[InstallTarget::OpenClaw]).is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
            assert!(f.workspace.join("skills/probe/SKILL.md").is_file());
            assert!(plugin.join("index.js").is_file());
        }
    });
}

// Requires native adaptation publication or an accepted native-write preview.
#[cfg(unix)]
#[test]
fn openclaw_profile_plugin_preview_cas_keeps_concurrent_config_and_source() {
    isolated("openclaw_profile_plugin_preview_cas_keeps_concurrent_config_and_source", |f| {
        f.configure(json!([f.source]));
        let catalog = f.catalog(f.projects());
        let ids = catalog.native_workspaces().iter().map(|w| w.id()).collect();
        let plan = catalog.plan(Action::Install, &BTreeSet::from([f.skill().key]),
            &BTreeSet::from(["openclaw".into()]), &BTreeSet::new(), &ids).unwrap();
        assert_eq!(plan.count(Disposition::Change), 1);
        assert_eq!(plan.items[0].instruction_paths.len(), 7);
        assert!(plan.items[0].instruction_paths.contains(&f.config));
        let mut current: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
        current["unrelated-user-value"] = json!("must survive"); f.config(current);
        let before = fs::read(&f.config).unwrap();
        let outcome = plan.execute(); assert_eq!((outcome.changed(), outcome.failed()), (0, 1));
        assert_eq!(fs::read(&f.config).unwrap(), before);
        assert!(!f.workspace.join("skills/probe").exists());
        assert!(!f.state.join("extensions/pleat-openclaw-adaptation").exists());
        assert_eq!(fs::read_to_string(f.source.join("local-data/keep")).unwrap(), "local state");
    });
}

#[test]
fn openclaw_workspace_env_is_only_a_default() {
    isolated("openclaw_workspace_env_is_only_a_default", |f| {
        let fallback = f.root.join("env-workspace");
        fs::create_dir_all(&fallback).unwrap();
        f.configure(json!([]));
        assert_eq!(f.projects()[0].path, f.workspace);
        f.config(json!({"agents":{"entries":{"reviewer":{}}}}));
        let entries = f.projects();
        assert_eq!(entries[0].path, fallback);
        assert_eq!(
            entries[0].native_workspaces[0].source_id.as_deref(),
            Some("reviewer")
        );
    });
}

// Requires native adaptation publication or an accepted native-write preview.
#[cfg(unix)]
#[test]
fn openclaw_native_binding_change_rejects_previewed_install() {
    isolated(
        "openclaw_native_binding_change_rejects_previewed_install",
        |f| {
            f.configure(json!([f.source]));
            let catalog = f.catalog(f.projects());
            let ids = catalog
                .native_workspaces()
                .iter()
                .map(|workspace| workspace.id())
                .collect();
            let plan = catalog
                .plan(
                    Action::Install,
                    &BTreeSet::from([f.skill().key]),
                    &BTreeSet::from(["openclaw".into()]),
                    &BTreeSet::new(),
                    &ids,
                )
                .unwrap();
            assert_eq!(plan.count(Disposition::Change), 1);
            // The same physical folder now belongs to a different native agentId.
            f.config(
                json!({"agents":{"entries":{"replacement":{"workspace":f.workspace}}},
            "skills":{"load":{"allowSymlinkTargets":[f.source]}}}),
            );
            let result = plan.execute();
            assert_eq!((result.changed(), result.failed()), (0, 1));
            assert!(f.workspace.join("skills/probe").symlink_metadata().is_err());
        },
    );
}

// Requires native adaptation publication or an accepted native-write preview.
#[cfg(unix)]
#[test]
fn openclaw_shared_path_agent_selections_make_one_physical_link() {
    isolated(
        "openclaw_shared_path_agent_selections_make_one_physical_link",
        |f| {
            f.config(json!({"agents":{"entries":{"first":{"workspace":f.workspace},"second":{"workspace":f.workspace}}},
            "skills":{"load":{"allowSymlinkTargets":[f.source]}}}));
            let catalog = f.catalog(f.projects());
            let ids = catalog
                .native_workspaces()
                .iter()
                .map(|workspace| workspace.id())
                .collect::<BTreeSet<_>>();
            assert_eq!(ids.len(), 2);
            let plan = catalog
                .plan(
                    Action::Install,
                    &BTreeSet::from([f.skill().key]),
                    &BTreeSet::from(["openclaw".into()]),
                    &BTreeSet::new(),
                    &ids,
                )
                .unwrap();
            assert_eq!(plan.count(Disposition::Change), 1);
            let result = plan.execute();
            assert_eq!((result.changed(), result.failed()), (1, 0));
            assert_eq!(
                f.workspace.join("skills/probe").canonicalize().unwrap(),
                f.source
            );
        },
    );
}

#[test]
fn openclaw_bundled_plugin_and_extra_roots_keep_native_boundaries() {
    isolated(
        "openclaw_bundled_plugin_and_extra_roots_keep_native_boundaries",
        |f| {
            let bundled = f.root.join("bundled");
            let plugin = f.state.join("extensions/demo");
            let extra = f.root.join("workshop-skills"); // Its basename must not change its source policy.
            let workshop = f.state.join("agents/reviewer/agent/workshop-skills");
            for (root, description) in [
                (&bundled, "bundled"),
                (&plugin.join("skills"), "plugin"),
                (&extra, "extra"),
            ] {
                write_skill(&root.join("same"), "same", description);
            }
            fs::write(
                plugin.join("openclaw.plugin.json"),
                r#"{"id":"demo","skills":["skills"]}"#,
            )
            .unwrap();
            fs::create_dir_all(&workshop).unwrap();
            for root in [&bundled, &workshop, &extra] {
                directory_link::create(&f.source, &root.join("probe")).unwrap();
            }
            f.config(json!({"agents":{"entries":{"reviewer":{"workspace":f.workspace}}},
            "skills":{"load":{"extraDirs":[extra],"allowSymlinkTargets":[f.source]}},"plugins":{"enabled":true}}));
            let visible = f.visible(&f.workspace);
            assert_eq!(
                visible
                    .iter()
                    .find(|s| s.name == "same")
                    .unwrap()
                    .description,
                "bundled"
            );
            // Bundled/workshop escapes stay rejected even when the extra root is trusted.
            assert_eq!(
                visible
                    .iter()
                    .find(|s| s.name == "probe")
                    .unwrap()
                    .root_path,
                Some(extra)
            );
        },
    );
}

#[test]
fn openclaw_native_ids_merge_without_losing_ownership() {
    isolated("openclaw_native_ids_merge_without_losing_ownership", |f| {
        f.config(json!({"agents":{"entries":{"writer":{"workspace":f.workspace},"reader":{"workspace":f.workspace}}}}));
        let entries = f.projects();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].agents, [AgentKind::OpenClaw]);
        assert_eq!(entries[0].native_workspaces.len(), 2);
        let native = &entries[0].native_workspaces;
        assert_ne!(native[0].id(), native[1].id());
        assert_eq!(
            native
                .iter()
                .map(|w| w.source_id.as_deref().unwrap())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["reader", "writer"])
        );
        assert!(native.iter().all(|w| w.path == f.workspace
            && w.project_roots.as_slice() == [f.workspace.clone()]
            && w.archived.is_none()));
        let selected =
            project_discovery::agents_for_project(&f.workspace, AGENT_ICON_ORDER, &entries);
        assert_eq!(
            selected.iter().map(|a| a.id).collect::<Vec<_>>(),
            ["openclaw"]
        );
        let roots = entries[0].skill_directories(selected[0]);
        assert_eq!(
            roots,
            [
                f.workspace.join("skills"),
                f.workspace.join(".agents/skills")
            ]
        );
        let hidden = BTreeSet::from([f.workspace.clone()]);
        assert!(project_discovery::merge_projects(&[], &entries, &hidden).is_empty());
        assert_eq!(
            project_discovery::merge_projects(&[], &entries, &BTreeSet::new())[0].native_workspaces,
            *native
        );
        // Empty authored roster supersedes the previous catalog, not an archive inference.
        f.config(json!({"agents":{"ownership":"explicit","entries":{}}}));
        assert!(
            !project_discovery::refresh_agent_projects(&entries)
                .unwrap()
                .iter()
                .any(|p| p.agents.contains(&AgentKind::OpenClaw))
        );
    });
}

#[test]
fn openclaw_native_defaults_and_include_keep_agent_ids() {
    isolated("openclaw_native_defaults_and_include_keep_agent_ids", |f| {
        let second = f.workspace.join("second");
        fs::create_dir_all(&second).unwrap();
        fs::write(f.state.join("agents.json5"), serde_json::to_vec(&json!({"ownership":"explicit",
            "defaults":{"workspace":f.workspace},"entries":{"first":{"workspace":f.workspace},"second":{}}})).unwrap()).unwrap();
        fs::write(&f.config, "{agents: {$include:'agents.json5'}}").unwrap();
        let entries = f.projects();
        assert_eq!(entries.len(), 2);
        assert!(
            entries.iter().any(|p| p.path == second
                && p.native_workspaces[0].source_id.as_deref() == Some("second"))
        );
        let previous = entries.clone();
        fs::write(&f.config, "invalid-private-body").unwrap();
        let retained = project_discovery::refresh_agent_projects(&previous).unwrap();
        assert!(
            previous
                .iter()
                .all(|p| retained.iter().any(|r| r.path == p.path
                    && r.native_workspaces == p.native_workspaces
                    && !r.archived))
        );
        f.config(json!({"agents":{"list":[{"id":"legacy","workspace":f.workspace}]}}));
        assert_eq!(
            f.projects()[0].native_workspaces[0].source_id.as_deref(),
            Some("legacy")
        );
    });
}

// Requires native adaptation publication or an accepted native-write preview.
#[cfg(unix)]
#[test]
fn openclaw_multiple_workspace_selection_uses_common_link_lifecycle() {
    isolated(
        "openclaw_multiple_workspace_selection_uses_common_link_lifecycle",
        |f| {
            let second = f.root.join("workspace-two");
            fs::create_dir_all(&second).unwrap();
            f.config(json!({"agents":{"entries":{"first":{"workspace":f.workspace},"second":{"workspace":second}}},
            "skills":{"load":{"allowSymlinkTargets":[f.source]}}}));
            let before = fs::read(&f.config).unwrap();
            let catalog = f.catalog(f.projects());
            let ids = catalog
                .native_workspaces()
                .iter()
                .map(|w| w.id())
                .collect::<BTreeSet<_>>();
            let skills = BTreeSet::from([f.skill().key]);
            let agents = BTreeSet::from(["openclaw".to_string()]);
            let plan = catalog
                .plan(Action::Install, &skills, &agents, &BTreeSet::new(), &ids)
                .unwrap();
            assert_eq!(plan.count(Disposition::Change), 2);
            assert!(
                plan.items
                    .iter()
                    .all(|item| item.path == item.project.join("skills/probe")
                        && item.affected_agents.iter().any(|a| a.id == "openclaw")
                    && item.instruction_paths.len() == 7
                    && item.instruction_paths.contains(&f.config)
                        && item.native_workspace_paths == [item.project.clone()])
            );
            let result = plan.execute();
            assert_eq!((result.changed(), result.failed()), (2, 0));
            write_skill(&f.source, "probe", "updated");
            for workspace in [&f.workspace, &second] {
                assert_eq!(
                    workspace.join("skills/probe").canonicalize().unwrap(),
                    f.source
                );
                assert!(
                    f.visible(workspace)
                        .iter()
                        .any(|s| s.name == "probe" && s.description == "updated")
                );
                assert!(
                    project::list(workspace, &f.root.join("library")).unwrap()[0]
                        .installations
                        .iter()
                        .any(|i| i.target == InstallTarget::OpenClaw && i.managed)
                );
            }
            let mut current: Value = json5::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
            let mut unrelated = current.clone(); unrelated.as_object_mut().unwrap().remove("plugins");
            assert_eq!(unrelated, serde_json::from_slice::<Value>(&before).unwrap());
            assert_eq!(current["plugins"]["entries"]["pleat-openclaw-adaptation"]["enabled"], true);
            // The new adapter owns one config entry. Revoke source trust
            // before taking a fresh removal preview, retaining native routes
            // and the unrelated plugin configuration for the CAS transaction.
            current["skills"]["load"]["allowSymlinkTargets"] = json!([]);
            f.config(current);
            let catalog = f.catalog(f.projects());
            let remove = catalog
                .plan(Action::Remove, &skills, &agents, &BTreeSet::new(), &ids)
                .unwrap();
            let result = remove.execute();
            assert_eq!((result.changed(), result.failed()), (2, 0));
            for workspace in [&f.workspace, &second] {
                assert!(workspace.join("skills/probe").symlink_metadata().is_err());
            }
            assert_eq!(
                fs::read_to_string(f.source.join("local-data/keep")).unwrap(),
                "local state"
            );
            assert!(f.source.join("SKILL.md").is_file());
        },
    );
}

#[cfg(windows)]
fn assert_native_write_refused(f: &Fixture, root: &Path, source: &Path, name: &str) {
    let config = fs::read(&f.config).unwrap();
    let body = fs::read(source.join("SKILL.md")).unwrap();
    let local = fs::read(f.source.join("local-data/keep")).unwrap();
    let target = agents::installation_root(root, InstallTarget::OpenClaw).unwrap().join(name);
    let error = project::install_from_path(root, source, name, &[InstallTarget::OpenClaw]).unwrap_err();
    assert!(format!("{error:#}").contains("此平台尚未支持原生适配受管写入"), "{error:#}");
    assert!(target.symlink_metadata().is_err());
    assert!(!f.state.join("extensions/pleat-openclaw-adaptation").exists());
    assert_eq!(fs::read(&f.config).unwrap(), config);
    assert_eq!(fs::read(source.join("SKILL.md")).unwrap(), body);
    assert_eq!(fs::read(f.source.join("local-data/keep")).unwrap(), local);
}

#[test]
fn openclaw_trust_shape_default_and_real_target_are_enforced() {
    isolated(
        "openclaw_trust_shape_default_and_real_target_are_enforced",
        |f| {
            let target = f.workspace.join("skills/probe");
            let alias = f.root.join("trusted-alias");
            directory_link::create(&f.source, &alias).unwrap();
            let sibling = f.root.join("library/probe-other");
            write_skill(&sibling, "probe", "sibling");
            f.configure(json!([]));
            let known = f.projects();
            for trust in [
                Value::Null,
                json!(false),
                json!(true),
                json!(f.source),
                json!([]),
                json!([f.source, true]),
                json!([sibling]),
            ] {
                f.configure(trust);
                let before = fs::read(&f.config).unwrap();
                let error = project::install_from_path(
                    &f.workspace,
                    &f.source,
                    "probe",
                    &[InstallTarget::OpenClaw],
                )
                .unwrap_err();
                assert!(
                    error.to_string().contains("allowSymlinkTargets")
                        || error.to_string().contains("配置无效")
                );
                assert!(target.symlink_metadata().is_err());
                assert_eq!(fs::read(&f.config).unwrap(), before);
                let catalog = f.catalog(known.clone());
                let plan = catalog
                    .plan(
                        Action::Install,
                        &BTreeSet::from([f.skill().key]),
                        &BTreeSet::from(["openclaw".into()]),
                        &BTreeSet::from([f.workspace.clone()]),
                        &BTreeSet::new(),
                    )
                    .unwrap();
                assert_eq!(plan.count(Disposition::Protected), 1);
            }
            // The array resolves aliases and admits only descendants of its real target.
            f.configure(json!([alias]));
            #[cfg(unix)]
            {
                project::install_from_path(
                    &f.workspace,
                    &f.source,
                    "probe",
                    &[InstallTarget::OpenClaw],
                )
                .unwrap();
                assert_eq!(target.canonicalize().unwrap(), f.source);
            }
            #[cfg(windows)]
            assert_native_write_refused(f, &f.workspace, &f.source, "probe");
            let denied = project::install_from_path(
                &f.workspace,
                &sibling,
                "sibling",
                &[InstallTarget::OpenClaw],
            ).unwrap_err();
            assert!(format!("{denied:#}").contains("allowSymlinkTargets"), "{denied:#}");
            #[cfg(unix)]
            project::uninstall(
                &f.workspace,
                &f.root.join("library"),
                "probe",
                &[InstallTarget::OpenClaw],
            )
            .unwrap();
            // A source contained in the actual native root needs no external trust.
            let contained = f.workspace.join("skills/.source/probe");
            write_skill(&contained, "probe", "contained");
            f.configure(json!([]));
            #[cfg(unix)]
            {
                project::install_from_path(
                    &f.workspace,
                    &contained,
                    "probe",
                    &[InstallTarget::OpenClaw],
                )
                .unwrap();
                assert_eq!(target.canonicalize().unwrap(), contained);
            }
            #[cfg(windows)]
            assert_native_write_refused(f, &f.workspace, &contained, "probe");
        },
    );
}

// Requires native adaptation publication or an accepted native-write preview.
#[cfg(unix)]
#[test]
fn openclaw_revoked_trust_rejects_previewed_install() {
    isolated("openclaw_revoked_trust_rejects_previewed_install", |f| {
        f.configure(json!([f.source]));
        let catalog = f.catalog(f.projects());
        let plan = catalog
            .plan(
                Action::Install,
                &BTreeSet::from([f.skill().key]),
                &BTreeSet::from(["openclaw".into()]),
                &BTreeSet::from([f.workspace.clone()]),
                &BTreeSet::new(),
            )
            .unwrap();
        assert_eq!(plan.count(Disposition::Change), 1);
        // This selection is a directory, not an explicit native-workspace
        // selection; it must not expand to other native members.
        assert!(plan.items[0].native_workspace_paths.is_empty());
        assert_eq!(plan.items[0].project, f.workspace);
        assert_eq!(plan.items[0].path, f.workspace.join("skills/probe"));
        f.configure(json!([]));
        let before = fs::read(&f.config).unwrap();
        let result = plan.execute();
        assert_eq!((result.changed(), result.failed()), (0, 1));
        assert!(f.workspace.join("skills/probe").symlink_metadata().is_err());
        assert_eq!(fs::read(&f.config).unwrap(), before);
    });
}

#[test]
fn openclaw_scanner_obeys_workspace_and_execution_precedence() {
    isolated(
        "openclaw_scanner_obeys_workspace_and_execution_precedence",
        |f| {
            f.configure(json!([]));
            let execution = f.root.join("execution");
            let extra = f.root.join("extra");
            for (path, description) in [
                (f.workspace.join("skills/group/collision"), "workspace"),
                (
                    f.workspace.join(".agents/skills/collision"),
                    "project-agent",
                ),
                (f.home.join(".agents/skills/collision"), "personal"),
                (f.state.join("skills/collision"), "managed"),
                (
                    f.state
                        .join("agents/reviewer/agent/workshop-skills/collision"),
                    "workshop",
                ),
                (extra.join("collision"), "extra"),
                (execution.join("skills/collision"), "execution"),
            ] {
                write_skill(&path, "collision", description);
            }
            write_skill(
                &execution.join(".agents/skills/execution-only"),
                "execution-only",
                "execution",
            );
            let mut config: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
            config["skills"]["load"]["extraDirs"] = json!([extra]);
            f.config(config);
            fs::write(
                execution.join("skills/README.md"),
                "---\nname: not-a-skill\ndescription: ignore\n---\n",
            )
            .unwrap();
            let visible = f.visible(&execution);
            assert!(!visible.iter().any(|s| s.name == "not-a-skill"));
            assert_eq!(visible.iter().filter(|s| s.name == "collision").count(), 1);
            assert_eq!(
                visible
                    .iter()
                    .find(|s| s.name == "collision")
                    .unwrap()
                    .description,
                "workspace"
            );
            assert!(visible.iter().any(|s| s.name == "execution-only"));
            // Selected nested workspaces never inherit repository-parent roots.
            let nested = f.workspace.join("nested");
            fs::create_dir_all(nested.join(".git")).unwrap();
            f.config(json!({"agents":{"entries":{"nested":{"workspace":nested}}}}));
            assert!(
                !f.visible(&nested)
                    .iter()
                    .any(|s| s.name == "collision" && s.description == "workspace")
            );
        },
    );
}

#[test]
fn openclaw_scanner_rejects_escaped_skill_files_and_untrusted_aliases() {
    isolated(
        "openclaw_scanner_rejects_escaped_skill_files_and_untrusted_aliases",
        |f| {
            f.configure(json!([]));
            fs::create_dir_all(f.workspace.join("skills")).unwrap();
            fs::create_dir_all(f.state.join("skills")).unwrap();
            directory_link::create(&f.source, &f.workspace.join("skills/probe")).unwrap();
            directory_link::create(&f.source, &f.state.join("skills/probe")).unwrap();
            let visible = f.visible(&f.workspace);
            // The rejected high-priority alias cannot suppress a valid managed root.
            let probe = visible.iter().find(|s| s.name == "probe").unwrap();
            assert_eq!(
                probe.root_path.as_deref(),
                Some(f.state.join("skills").as_path())
            );
            f.configure(json!([f.source]));
            assert_eq!(
                f.visible(&f.workspace)
                    .iter()
                    .find(|s| s.name == "probe")
                    .unwrap()
                    .root_path,
                Some(f.workspace.join("skills"))
            );
            #[cfg(unix)]
            {
                fs::remove_file(f.source.join("SKILL.md")).unwrap();
                let outside = f.root.join("outside");
                write_skill(&outside, "escape", "not allowed");
                std::os::unix::fs::symlink(outside.join("SKILL.md"), f.source.join("SKILL.md"))
                    .unwrap();
                assert!(!f.visible(&f.workspace).iter().any(|s| s.name == "escape"));
            }
        },
    );
}

#[test]
fn openclaw_global_root_allows_links_and_conflicts_keep_user_files() {
    isolated(
        "openclaw_global_root_allows_links_and_conflicts_keep_user_files",
        |f| {
            f.configure(json!([]));
            assert!(!AgentKind::OpenClaw.is_global_only());
            assert_eq!(
                agents::installation_root(&f.workspace, InstallTarget::OpenClaw).unwrap(),
                f.workspace.join("skills")
            );
            assert_eq!(
                agents::global_target_root(&f.home, InstallTarget::OpenClaw).unwrap(),
                f.state.join("skills")
            );
            #[cfg(unix)]
            {
                project::install_from_path(&f.home, &f.source, "probe", &[InstallTarget::OpenClaw])
                    .unwrap();
                assert!(f.visible(&f.home).iter().any(|s| s.name == "probe"));
            }
            #[cfg(windows)]
            assert_native_write_refused(f, &f.home, &f.source, "probe");
            f.configure(json!([f.source]));
            let occupied = f.workspace.join("skills/probe");
            write_skill(&occupied, "user-owned", "keep");
            assert!(
                project::install_from_path(
                    &f.workspace,
                    &f.source,
                    "probe",
                    &[InstallTarget::OpenClaw]
                )
                .is_err()
            );
            assert!(
                fs::read_to_string(occupied.join("SKILL.md"))
                    .unwrap()
                    .contains("user-owned")
            );
            #[cfg(unix)]
            project::uninstall(
                &f.home,
                &f.root.join("library"),
                "probe",
                &[InstallTarget::OpenClaw],
            )
            .unwrap();
            assert!(f.source.join("SKILL.md").is_file());
        },
    );
}

#[test]
fn openclaw_profile_keeps_global_root_separate() {
    isolated("openclaw_profile_keeps_global_root_separate", |f| {
        f.configure(json!([]));
        assert_eq!(
            agents::global_target_root(&f.home, InstallTarget::OpenClaw).unwrap(),
            f.state.join("skills")
        );
        write_skill(
            &f.home.join(".agents/skills/personal"),
            "personal",
            "default state only",
        );
        write_skill(&f.state.join("skills/profile"), "profile", "profile state");
        assert!(!f.visible(&f.workspace).iter().any(|s| s.name == "personal"));
        assert!(f.visible(&f.workspace).iter().any(|s| s.name == "profile"));
        #[cfg(unix)]
        {
            project::install_from_path(&f.home, &f.source, "probe", &[InstallTarget::OpenClaw])
                .unwrap();
            assert_eq!(
                f.state.join("skills/probe").canonicalize().unwrap(),
                f.source
            );
        }
        #[cfg(windows)]
        assert_native_write_refused(f, &f.home, &f.source, "probe");
        assert!(
            f.home
                .join(".openclaw/skills/probe")
                .symlink_metadata()
                .is_err()
        );
    });
}

fn relocated_home_contract(name: &str) {
    isolated(name, |f| {
        let effective_home = f.home.join("relocated");
        let native_workspace = effective_home.join("workspace");
        let right = effective_home.join("trusted/right");
        let wrong = f.home.join("trusted/wrong");
        for path in [&native_workspace, &right, &wrong] {
            fs::create_dir_all(path).unwrap();
        }
        write_skill(&right, "right", "native home");
        write_skill(&wrong, "wrong", "OS home must not be trusted");
        write_skill(
            &f.home.join(".agents/skills/personal"),
            "os-personal",
            "OS home personal root",
        );
        write_skill(
            &effective_home.join(".agents/skills/personal"),
            "relocated-personal",
            "not the native personal root",
        );
        let extra = effective_home.join("extra");
        write_skill(&extra.join("extra"), "home-extra", "effective home");
        f.config(
            json!({"agents":{"entries":{"reviewer":{"workspace":"~/workspace"}}},
            "skills":{"load":{"allowSymlinkTargets":["~/trusted"],"extraDirs":["~/extra"]}}}),
        );
        let projects = f.projects();
        assert_eq!(projects[0].path, native_workspace);
        assert_eq!(
            projects[0].native_workspaces[0].source_id.as_deref(),
            Some("reviewer")
        );
        assert_eq!(
            agents::global_target_root(&f.home, InstallTarget::OpenClaw).unwrap(),
            f.state.join("skills")
        );
        #[cfg(unix)]
        project::install_from_path(
            &native_workspace,
            &right,
            "right",
            &[InstallTarget::OpenClaw],
        )
        .unwrap();
        #[cfg(windows)]
        {
            assert_native_write_refused(f, &native_workspace, &right, "right");
            // This explicit fixture link tests read-only trust/discovery under
            // relocated HOME; it is not a successful managed installation.
            fs::create_dir_all(native_workspace.join("skills")).unwrap();
            directory_link::create(&right, &native_workspace.join("skills/right")).unwrap();
        }
        let denied = project::install_from_path(
            &native_workspace,
            &wrong,
            "wrong",
            &[InstallTarget::OpenClaw],
        ).unwrap_err();
        assert!(format!("{denied:#}").contains("allowSymlinkTargets"), "{denied:#}");
        let visible = f.visible(&native_workspace);
        assert!(visible.iter().any(|s| s.name == "right"));
        assert!(visible.iter().any(|s| s.name == "home-extra"));
        assert!(
            !visible
                .iter()
                .any(|s| s.name == "wrong" || s.name == "relocated-personal")
        );
        let default_state = f.state == effective_home.join(".openclaw");
        assert_eq!(
            visible.iter().any(|s| s.name == "os-personal"),
            default_state
        );
        #[cfg(unix)]
        {
            project::install_from_path(&f.home, &f.source, "probe", &[InstallTarget::OpenClaw])
                .unwrap();
            assert_eq!(
                f.state.join("skills/probe").canonicalize().unwrap(),
                f.source
            );
        }
        #[cfg(windows)]
        assert_native_write_refused(f, &f.home, &f.source, "probe");
        assert!(
            f.home
                .join(".openclaw/skills/probe")
                .symlink_metadata()
                .is_err()
        );
        #[cfg(unix)]
        project::uninstall(
            &f.home,
            &f.root.join("library"),
            "probe",
            &[InstallTarget::OpenClaw],
        )
        .unwrap();
        assert!(f.source.join("SKILL.md").is_file());
    });
}

#[test]
fn openclaw_review_home_default() {
    relocated_home_contract("openclaw_review_home_default");
}
#[test]
fn openclaw_review_home_profile() {
    relocated_home_contract("openclaw_review_home_profile");
}
#[test]
fn openclaw_review_home_explicit_config() {
    relocated_home_contract("openclaw_review_home_explicit_config");
}

#[test]
fn openclaw_review_plain_skills_do_not_create_native_association() {
    isolated(
        "openclaw_review_plain_skills_do_not_create_native_association",
        |f| {
            f.configure(json!([f.source]));
            write_skill(
                &f.workspace.join("skills/native-only"),
                "native-only",
                "registered workspace",
            );
            let ordinary = f.root.join("codex-only");
            write_skill(&ordinary.join("skills/plain"), "plain", "ordinary folder");
            let entry = ProjectEntry {
                path: ordinary.clone(),
                name: "Codex only".into(),
                agents: vec![AgentKind::Codex],
                native_workspaces: Vec::new(),
                proma_workspaces: Vec::new(),
                archive_states: Vec::new(),
                archived: false,
                hidden: false,
                retained: false,
            };
            let mut catalog = f.projects();
            catalog.push(entry);
            let associated =
                project_discovery::agents_for_project(&ordinary, AGENT_ICON_ORDER, &catalog);
            assert!(!associated.iter().any(|a| a.id == "openclaw"));
            let estimates = effective_skills::estimate_project_with_catalog(
                &ordinary,
                AGENT_ICON_ORDER,
                &catalog,
            );
            assert!(!estimates.iter().any(|s| s.agent == AgentKind::OpenClaw));
            assert!(
                !estimates
                    .iter()
                    .flat_map(|s| &s.skills)
                    .any(|s| s.name == "native-only")
            );
            assert!(
                project_discovery::refresh_agent_projects(&catalog)
                    .unwrap()
                    .iter()
                    .flat_map(|p| &p.native_workspaces)
                    .all(|w| w.path != ordinary)
            );
            assert!(
                project_discovery::agents_for_project(&f.workspace, AGENT_ICON_ORDER, &catalog)
                    .iter()
                    .any(|a| a.id == "openclaw")
            );
            #[cfg(windows)]
            assert_native_write_refused(f, &ordinary, &f.source, "probe");
            // Explicit OpenClaw operations/view remain possible without inventing native registration.
            #[cfg(unix)]
            {
                project::install_from_path(&ordinary, &f.source, "probe", &[InstallTarget::OpenClaw])
                    .unwrap();
                assert_eq!(
                    ordinary.join("skills/probe").canonicalize().unwrap(),
                    f.source
                );
                assert!(f.visible(&ordinary).iter().any(|s| s.name == "probe"));
                project::uninstall(
                    &ordinary,
                    &f.root.join("library"),
                    "probe",
                    &[InstallTarget::OpenClaw],
                )
                .unwrap();
                assert!(f.source.join("SKILL.md").is_file());
            }
        },
    );
}

#[test]
fn openclaw_review_agent_dir_uses_normalized_native_entry() {
    isolated(
        "openclaw_review_agent_dir_uses_normalized_native_entry",
        |f| {
            let custom = f.root.join("custom-agent");
            write_skill(
                &custom.join("workshop-skills/real"),
                "real-workshop",
                "custom agentDir",
            );
            write_skill(
                &f.state.join("agents/reviewer/agent/workshop-skills/stale"),
                "stale-workshop",
                "not this agentDir",
            );
            for agents in [
                json!({"list":[{"id":"Reviewer","workspace":f.workspace,"agentDir":custom}]}),
                json!({"entries":{"Reviewer":{"workspace":f.workspace,"agentDir":custom}}}),
                json!({"entries":{"reviewer":{"workspace":f.workspace,"agentDir":custom}}}),
            ] {
                f.config(json!({"agents":agents}));
                assert_eq!(
                    f.projects()[0].native_workspaces[0].source_id.as_deref(),
                    Some("reviewer")
                );
                let visible = f.visible(&f.workspace);
                assert!(visible.iter().any(|s| s.name == "real-workshop"));
                assert!(!visible.iter().any(|s| s.name == "stale-workshop"));
            }
            f.configure(json!([]));
            assert!(
                f.visible(&f.workspace)
                    .iter()
                    .any(|s| s.name == "stale-workshop")
            );
        },
    );
}

#[test]
fn openclaw_review_config_failure_is_unknown_and_recovers() {
    isolated(
        "openclaw_review_config_failure_is_unknown_and_recovers",
        |f| {
            write_skill(
                &f.workspace.join("skills/disabled"),
                "disabled",
                "must remain disabled",
            );
            write_skill(&f.workspace.join("skills/known"), "known", "observed");
            let good = json!({"agents":{"entries":{"reviewer":{"workspace":f.workspace}}},
            "skills":{"entries":{"disabled":{"enabled":false}},"allowBundled":[],"load":{"allowSymlinkTargets":[f.source]}}});
            f.config(good.clone());
            let previous = f.projects();
            let valid = f.estimate(&f.workspace);
            assert!(valid.diagnostic.is_none());
            let valid_json = serde_json::to_value(&valid).unwrap();
            assert!(valid_json.get("diagnostic").is_none());
            assert!(valid_json["discovered_count"].is_number());
            let codex = effective_skills::estimate_project_with_home(&f.workspace, &f.home)
                .into_iter()
                .find(|estimate| estimate.agent == AgentKind::Codex)
                .unwrap();
            let codex_json = serde_json::to_value(codex).unwrap();
            assert!(codex_json.get("diagnostic").is_none());
            assert!(codex_json["discovered_count"].is_number());
            assert!(valid.skills.iter().any(|skill| skill.name == "known"));
            assert!(!valid.skills.iter().any(|skill| skill.name == "disabled"));
            for (body, code) in [
            ("invalid-json-private-fixture".to_string(), "openclaw_config_invalid"),
            (json!({"agents":[]}).to_string(), "openclaw_config_invalid"),
            (json!({"agents":{"entries":{"reviewer":{"workspace":42}}}}).to_string(), "openclaw_config_invalid"),
            (json!({"agents":{"entries":{"reviewer":{"workspace":f.workspace,"agentDir":[]}}}}).to_string(), "openclaw_config_invalid"),
            (json!({"agents":{"entries":{"reviewer":{"workspace":f.workspace}}},"skills":{"entries":{"disabled":{"enabled":"yes"}}}}).to_string(), "openclaw_config_invalid"),
            ("missing".into(), "openclaw_config_missing"),
            ("unreadable-directory".into(), "openclaw_config_unreadable"),
        ] {
            fs::remove_file(&f.config).unwrap();
            if body == "unreadable-directory" { fs::create_dir(&f.config).unwrap(); }
            else if body != "missing" { fs::write(&f.config, body).unwrap(); }
            let unknown = f.estimate(&f.workspace);
            assert_eq!(unknown.diagnostic.as_ref().unwrap().code, code);
            assert!(unknown.skills.is_empty(), "unobserved entries must not become Automatic");
            let serialized = serde_json::to_value(&unknown).unwrap();
            assert_eq!(serialized["diagnostic"]["code"], code);
            assert!(serialized["discovered_count"].is_null());
            assert!(serialized["model_visible_count"].is_null());
            assert!(serialized["estimated_tokens"].is_null());
            let retained = project_discovery::refresh_agent_projects(&previous).unwrap();
            assert!(retained.iter().any(|entry| entry.path == f.workspace && !entry.archived
                && entry.native_workspaces == previous[0].native_workspaces));
            let catalog = f.catalog(retained);
            let plan = catalog.plan(Action::Install, &BTreeSet::from([f.skill().key]),
                &BTreeSet::from(["openclaw".into()]), &BTreeSet::from([f.workspace.clone()]), &BTreeSet::new()).unwrap();
            assert_eq!(plan.count(Disposition::Protected), 1);
            if f.config.is_dir() { fs::remove_dir(&f.config).unwrap(); }
            f.config(good.clone());
            let recovered = f.estimate(&f.workspace);
            assert!(recovered.diagnostic.is_none());
            assert!(recovered.skills.iter().any(|skill| skill.name == "known"));
            assert!(!recovered.skills.iter().any(|skill| skill.name == "disabled"));
            assert_eq!(f.projects()[0].native_workspaces, previous[0].native_workspaces);
        }
        },
    );
}
