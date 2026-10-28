//! Isolated Pleat contracts; these tests do not launch the Agent applications.
#![cfg(any(unix, windows))]

use kitter::{
    agents::AGENT_ICON_ORDER,
    cross_project::{Action, Catalog, Disposition, Skill},
    effective_skills::estimate_project_with_home,
    project_discovery::ProjectEntry,
    proma, proma_batch,
};
use std::{collections::BTreeSet, fs, path::Path, process::Command};

const NAME: &str = "pleat-compat-probe";

fn write_skill(source: &Path, version: &str) {
    fs::write(
        source.join("SKILL.md"),
        format!("---\nname: {NAME}\ndescription: Compatibility {version}\n---\nRead-only fixture.\n"),
    )
    .unwrap();
}

// Capture files and directories so refusals cannot silently leave rules, links,
// or new source/local-data entries behind.
#[cfg(windows)]
fn snapshot_tree(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Option<Vec<u8>>> {
    fn visit(root: &Path, path: &Path, entries: &mut std::collections::BTreeMap<std::path::PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            assert!(!kind.is_symlink(), "unexpected fixture link: {}", path.display());
            entries.insert(path.strip_prefix(root).unwrap().to_path_buf(),
                if kind.is_dir() { None } else { Some(fs::read(&path).unwrap()) });
            if kind.is_dir() { visit(root, &path, entries); }
        }
    }
    let mut entries = std::collections::BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

fn run_case(id: &str, directory: &str, test_name: &str) {
    if std::env::var("PLEAT_COMPAT_CHILD").as_deref() != Ok(id) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let home = root.join("home");
        fs::create_dir(&home).unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture"])
            .env_clear()
            .envs(if cfg!(windows) { std::env::var_os("SystemRoot").map(|value| ("SystemRoot", value)) } else { None })
            .env("PLEAT_COMPAT_CHILD", id)
            .env("PLEAT_COMPAT_ROOT", &root)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("CODEX_HOME", home.join(".codex"))
            .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
            .env("GROK_HOME", home.join(".grok"))
            .env("PI_CODING_AGENT_DIR", home.join(".pi/agent"))
            .env("KITTER_PROMA_HOME", root.join("proma"))
            .env("DIMCODE_HOME", home.join(".dimcode/v2"))
            .env("OPENCLAW_STATE_DIR", home.join(".openclaw"))
            .env("OPENCLAW_CONFIG_PATH", home.join(".openclaw/openclaw.json"))
            .env_remove("OPENCLAW_HOME")
            .env("HERMES_HOME", home.join(".hermes"))
            .env("WORKBUDDY_CONFIG_DIR", home.join(".workbuddy"))
            .env("WORKBUDDY_DATA_FOLDER_NAME", if id == "workbuddy" { directory.strip_suffix("/skills").unwrap() } else { ".workbuddy" })
            .env_remove("WORKBUDDY_INSTANCE_NUMBER")
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env_remove("OMP_PROFILE")
            .env_remove("PI_PROFILE")
            .output()
            .unwrap();
        assert!(output.status.success(), "{id}:\n{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        println!("{id}: isolated Pleat contract passed (native Agent not launched)");
        return;
    }

    let root = std::path::PathBuf::from(std::env::var_os("PLEAT_COMPAT_ROOT").unwrap());
    let home = root.join("home");
    let project = home.join("project");
    let source = root.join("library").join(NAME);
    fs::create_dir_all(project.join(".git")).unwrap();
    fs::create_dir_all(source.join("local-data")).unwrap();
    fs::write(source.join("local-data/state.txt"), "keep state").unwrap();
    write_skill(&source, "v1");
    if id == "openclaw" {
        let state = home.join(".openclaw");
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("openclaw.json"), serde_json::to_vec(&serde_json::json!({
            "agents":{"entries":{"probe":{"workspace":project}}},
            "skills":{"load":{"allowSymlinkTargets":[source]}}
        })).unwrap()).unwrap();
    }
    if id == "hermes" {
        let profile = home.join(".hermes");
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("config.yaml"), serde_yaml::to_string(&serde_json::json!({
            "skills":{"project_discovery":true,"trusted_project_dirs":[project]}
        })).unwrap()).unwrap();
    }
    let agent = *AGENT_ICON_ORDER.iter().find(|a| a.id == id).unwrap();
    let proma_root = root.join("proma");
    let workspace = proma_root.join("agent-workspaces/probe");
    if id == "proma" {
        fs::create_dir_all(&workspace).unwrap();
        fs::write(proma_root.join("agent-workspaces.json"), serde_json::to_vec(&serde_json::json!({
            "version": 2, "workspaces": [{"id":"probe", "slug":"probe", "name":"Probe", "projectRootPath":project}]
        })).unwrap()).unwrap();
    }
    let skill = Skill {
        key: proma_batch::source_key(&source.join("SKILL.md")), name: NAME.into(), source: source.clone(),
        provider: "library", install_name: Some(NAME.into()),
    };
    let entry = ProjectEntry {
        path: project.clone(), name: "Probe".into(), agents: vec![],
        proma_workspaces: if id == "proma" { vec![workspace.clone()] } else { vec![] },
        native_workspaces: Vec::new(), archive_states: Vec::new(),
        archived: false, hidden: false, retained: false,
    };
    let catalog = || Catalog::scan(vec![skill.clone()], vec![entry.clone()], vec![agent],
        (id == "proma").then_some(proma_root.as_path()));
    let skill_ids = BTreeSet::from([skill.key.clone()]);
    let agent_ids = BTreeSet::from([id.to_string()]);
    let projects = BTreeSet::from([project.clone()]);
    let workspaces = if id == "proma" { BTreeSet::from(["probe".into()]) } else { BTreeSet::new() };

    if agent.global_only {
        assert!(catalog().plan(Action::Install, &skill_ids, &agent_ids, &projects, &workspaces).is_err());
        let installed = home.join(directory).join(NAME);
        fs::create_dir_all(&installed).unwrap();
        write_skill(&installed, "v1");
        let inspect = || estimate_project_with_home(&home, &home).into_iter().find(|e| e.agent.id() == id).unwrap();
        assert!(inspect().skills.iter().any(|s| s.name == NAME && s.description.ends_with("v1")));
        write_skill(&installed, "v2");
        assert!(inspect().skills.iter().any(|s| s.name == NAME && s.description.ends_with("v2")));
        fs::remove_file(installed.join("SKILL.md")).unwrap();
        assert!(!inspect().skills.iter().any(|s| s.name == NAME));
        return;
    }

    #[cfg(windows)]
    let before = snapshot_tree(&root);
    let install = catalog().plan(Action::Install, &skill_ids, &agent_ids, &projects, &workspaces).unwrap();
    #[cfg(windows)]
    if matches!(id, "claude-code" | "opencode" | "pi" | "omp" | "grok" | "antigravity"
        | "droid" | "copilot" | "trae" | "workbuddy" | "openclaw" | "hermes") {
        let unsupported = if matches!(id, "hermes" | "openclaw") {
            "此平台尚未核验原生配置 ACL"
        } else { "此平台尚未支持原生适配受管写入" };
        assert_eq!(install.items.len(), 1, "{id}");
        assert_eq!(install.count(Disposition::Protected), 1, "{id}");
        assert_eq!(install.count(Disposition::Change), 0, "{id}");
        assert!(install.items[0].reason.as_deref().unwrap().contains(unsupported), "{id}: {install:?}");
        // A protected batch item is a no-op; the direct API reports the refusal.
        assert_eq!(install.execute().changed(), 0, "{id}");
        assert_eq!(snapshot_tree(&root), before, "{id}: blocked preview/execute changed fixture");
        let target = agent.install_targets.iter().copied()
            .find(|target| agent.owns_target(*target))
            .unwrap();
        let error = kitter::project::install_from_path(&project, &source, NAME, &[target]).unwrap_err();
        assert!(format!("{error:#}").contains(unsupported), "{id}: {error:#}");
        assert!(project.join(directory).join(NAME).symlink_metadata().is_err(), "{id}: link created on refusal");
        assert_eq!(snapshot_tree(&root), before, "{id}: refused install changed source/local-data or native entries");
        return;
    }
    assert_eq!(install.count(Disposition::Change), 1, "{id}");
    let result = install.execute();
    assert_eq!((result.changed(), result.failed()), (1, 0), "{id}: {result:?}");
    let installed = if id == "proma" { workspace.join("skills").join(NAME) } else { project.join(directory).join(NAME) };
    assert!(fs::read_to_string(installed.join("SKILL.md")).unwrap().contains("v1"));
    if id != "proma" { assert_eq!(installed.canonicalize().unwrap(), source); }
    let inspect = || estimate_project_with_home(&project, &home).into_iter().find(|e| e.agent.id() == id);
    if id == "grok" {
        // No Agent is launched by this contract suite. A missing native
        // inspector is unknown, never a successful filesystem-only scan.
        let estimate = inspect().unwrap();
        assert!(estimate.diagnostic.is_some() && estimate.skills.is_empty());
        assert!(serde_json::to_value(estimate).unwrap()["discovered_count"].is_null());
        assert_eq!(fs::read_dir(project.join(".grok/rules")).unwrap().count(), 1);
    } else {
        assert!(inspect().unwrap().skills.iter().any(|s| s.name == NAME && s.description.ends_with("v1")), "{id}: installed skill not discovered");
    }
    write_skill(&source, "v2");
    if id == "proma" {
        let update = proma::plan_install(&proma_root, &workspace, &source, NAME).unwrap();
        proma::apply_plan(&update).unwrap();
    }
    assert!(fs::read_to_string(installed.join("SKILL.md")).unwrap().contains("v2"));
    if id != "grok" {
        assert!(inspect().unwrap().skills.iter().any(|s| s.name == NAME && s.description.ends_with("v2")), "{id}: update not discovered");
    }
    let remove = catalog().plan(Action::Remove, &skill_ids, &agent_ids, &projects, &workspaces).unwrap();
    let result = remove.execute();
    assert_eq!((result.changed(), result.failed()), (1, 0), "{id}: {result:?}");
    assert!(installed.symlink_metadata().is_err(), "{id}: install still present");
    assert!(fs::read_to_string(source.join("SKILL.md")).unwrap().contains("v2"));
    assert_eq!(fs::read_to_string(source.join("local-data/state.txt")).unwrap(), "keep state");
    if id == "grok" {
        assert_eq!(fs::read_dir(project.join(".grok/rules")).unwrap().count(), 0);
    }
    let residual = inspect().unwrap().skills.into_iter().filter(|s| s.name == NAME).map(|s| s.path).collect::<Vec<_>>();
    assert!(residual.is_empty(), "{id}: still discovered after uninstall: {residual:?}");
}

macro_rules! agent_case {
    ($test:ident, $id:literal, $directory:literal) => {
        #[test]
        fn $test() { run_case($id, $directory, stringify!($test)); }
    };
}

agent_case!(codex, "codex", ".codex/skills");
agent_case!(claude_code, "claude-code", ".claude/skills");
agent_case!(cursor, "cursor", ".cursor/skills");
agent_case!(opencode, "opencode", ".opencode/skills");
agent_case!(pi, "pi", ".pi/skills");
#[cfg(unix)]
agent_case!(proma, "proma", "skills");
agent_case!(omp, "omp", ".omp/skills");
agent_case!(dimagent, "dimagent", ".agents/skills");
agent_case!(grok, "grok", ".grok/skills");
agent_case!(antigravity, "antigravity", ".agent/skills");
agent_case!(droid, "droid", ".factory/skills");
agent_case!(copilot, "copilot", ".github/skills");
agent_case!(amp, "amp", ".agents/skills");
agent_case!(trae, "trae", ".trae/skills");
agent_case!(workbuddy, "workbuddy", ".workbuddy/skills");
agent_case!(workbuddy_custom_project_root, "workbuddy", ".workbuddy-preview/skills");
agent_case!(openclaw_workspace, "openclaw", "skills");
agent_case!(hermes_project, "hermes", ".hermes/skills");

#[test]
fn explicit_agent_inspection_does_not_require_or_create_project_affiliation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let home = root.join("home");
    let project = home.join("project");
    let skill = project.join(".trae/skills").join(NAME);
    fs::create_dir_all(&skill).unwrap();
    fs::create_dir_all(project.join(".git")).unwrap();
    write_skill(&skill, "explicit");
    let run = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_kitter"));
        command.env_clear();
        for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(key) { command.env(key, value); }
        }
        let output = command.args(args).current_dir(&project)
            .env("HOME", &home).env("USERPROFILE", &home)
            .env("KITTER_HOME", root.join("kitter"))
            .env("KITTER_PROMA_HOME", root.join("proma"))
            .env("PATH", "").output().unwrap();
        assert!(output.status.success(), "isolated inspection failed: status={}, stderr={}",
            output.status, String::from_utf8_lossy(&output.stderr));
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let selected = run(&["project", project.to_str().unwrap(), "--agent", "trae", "--json"]);
    assert_eq!(selected["agents"].as_array().unwrap().len(), 1);
    assert_eq!(selected["agents"][0]["agent"], "trae");
    assert!(selected["agents"][0]["skills"].as_array().unwrap().iter().any(|s| s["name"] == NAME));
    let projects = run(&["projects", "--json"]);
    assert!(!projects.as_array().unwrap().iter().any(|p| p["path"] == serde_json::json!(project)));
}

// No native instruction entry is needed for an isolated Codex skill root.
// Keep the direct API's pure directory-link path working on both platforms.
#[test]
fn plain_codex_direct_install_round_trip_on_unix_and_windows() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let source = root.join(NAME);
    let project = root.join("project");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&project).unwrap();
    fs::create_dir(source.join("local-data")).unwrap();
    fs::write(source.join("local-data/state.txt"), "keep state").unwrap();
    write_skill(&source, "v1");
    let original = fs::read(source.join("SKILL.md")).unwrap();
    kitter::project::install_from_path(&project, &source, NAME, &[kitter::InstallTarget::Codex]).unwrap();
    let installed = project.join(".codex/skills").join(NAME);
    assert_eq!(installed.canonicalize().unwrap(), source);
    assert_eq!(fs::read(installed.join("SKILL.md")).unwrap(), original);
    kitter::project::uninstall(&project, &root, NAME, &[kitter::InstallTarget::Codex]).unwrap();
    assert!(installed.symlink_metadata().is_err());
    assert_eq!(fs::read(source.join("SKILL.md")).unwrap(), original);
    assert_eq!(fs::read_to_string(source.join("local-data/state.txt")).unwrap(), "keep state");
}

#[test]
fn replaced_project_identity_rejects_install_on_unix_and_windows() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let source = root.join("source");
    let project = root.join("project");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&project).unwrap();
    write_skill(&source, "v1");
    let skill = Skill { key: proma_batch::source_key(&source.join("SKILL.md")), name: NAME.into(),
        source: source.clone(), provider: "library", install_name: Some(NAME.into()) };
    let entry = ProjectEntry { path: project.clone(), name: "Probe".into(), agents: vec![],
        proma_workspaces: vec![], native_workspaces: Vec::new(), archive_states: Vec::new(), archived: false, hidden: false, retained: false };
    let agent = *AGENT_ICON_ORDER.iter().find(|a| a.id == "codex").unwrap();
    let catalog = Catalog::scan(vec![skill.clone()], vec![entry], vec![agent], None);
    let plan = catalog.plan(Action::Install, &BTreeSet::from([skill.key]), &BTreeSet::from(["codex".into()]),
        &BTreeSet::from([project.clone()]), &BTreeSet::new()).unwrap();
    assert_eq!(plan.count(Disposition::Change), 1);
    fs::rename(&project, root.join("retained-project")).unwrap();
    fs::create_dir(&project).unwrap();
    assert_eq!(plan.execute().failed(), 1);
    assert!(!project.join(".codex/skills").exists());
    assert!(source.join("SKILL.md").is_file());
}

#[test]
fn workbuddy_global_directory_validation_precedes_any_write() {
    const FLAG: &str = "PLEAT_WORKBUDDY_ROOT_CHILD";
    if std::env::var_os(FLAG).is_none() {
        for mode in ["empty", "whitespace", "relative", "absolute", "absolute-spaced"] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let home = root.join("home");
            let cwd = root.join("working");
            fs::create_dir(&home).unwrap();
            fs::create_dir(&cwd).unwrap();
            fs::write(cwd.join("sentinel"), "keep").unwrap();
            let configured = match mode {
                "empty" => std::ffi::OsString::new(),
                "whitespace" => std::ffi::OsString::from(" \t \n"),
                "relative" => std::ffi::OsString::from("relative"),
                "absolute-spaced" => std::ffi::OsString::from(format!(" \t{} \n", root.join("configured").display())),
                _ => root.join("configured").into_os_string(),
            };
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "workbuddy_global_directory_validation_precedes_any_write"])
                .env_clear().env("HOME", &home).env("USERPROFILE", &home)
                .env(FLAG, mode).env("WORKBUDDY_CONFIG_DIR", configured)
                .current_dir(&cwd).output().unwrap();
            assert!(output.status.success(), "{mode}: {} {}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            assert_eq!(fs::read_to_string(cwd.join("sentinel")).unwrap(), "keep");
            assert!(!cwd.join("skills").exists());
            assert!(!cwd.join("relative").exists());
        }
        return;
    }
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let source = home.join("source");
    fs::create_dir(&source).unwrap();
    write_skill(&source, "v1");
    let result = kitter::project::install_from_path(&home, &source, NAME, &[kitter::InstallTarget::WorkBuddy]);
    if std::env::var(FLAG).as_deref() != Ok("relative") {
        let raw = std::env::var("WORKBUDDY_CONFIG_DIR").unwrap();
        let configured = if raw.trim().is_empty() { home.join(".workbuddy") }
            else { std::path::PathBuf::from(raw.trim()) };
        #[cfg(unix)]
        {
            result.unwrap();
            assert_eq!(configured.join("skills").join(NAME).canonicalize().unwrap(), source);
        }
        #[cfg(windows)]
        {
            let error = result.unwrap_err();
            assert!(format!("{error:#}").contains("此平台尚未支持原生适配受管写入"), "{error:#}");
            assert!(!configured.exists());
        }
    } else {
        assert!(result.is_err());
        assert!(!home.join(".workbuddy").exists());
    }
    assert!(source.join("SKILL.md").is_file());
}
