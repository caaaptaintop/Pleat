//! Mock native registrations and links only; never starts an Agent or installer.
#![cfg(unix)]

use kitter::SkillLibrary;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    process::Command,
};

struct Fixture {
    root: PathBuf,
    data: PathBuf,
    projects: [PathBuf; 2],
    sources: [PathBuf; 2],
    names: [&'static str; 2],
}

fn write_json(path: &Path, value: Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

fn link(source: &Path, path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    symlink(source, path).unwrap();
}

impl Fixture {
    fn new(root: PathBuf, names: [&'static str; 2]) -> Self {
        let data = root.join("data");
        let projects = [
            root.join("home/project"),
            root.join("home/project-secondary"),
        ];
        let sources = [root.join("source/a"), root.join("source/b")];
        for path in &projects {
            fs::create_dir_all(path).unwrap();
        }
        let mut skills = serde_json::Map::new();
        for (index, source) in sources.iter().enumerate() {
            fs::create_dir_all(source).unwrap();
            fs::write(
                source.join("SKILL.md"),
                format!(
                    "---\nname: {}\ndescription: mock fixture\n---\n",
                    names[index]
                ),
            )
            .unwrap();
            let storage = format!("source-{index}");
            link(source, &data.join("skills").join(&storage));
            skills.insert(
                storage.clone(),
                json!({"name": names[index], "storage_name": storage,
                "origin": {"type": "local", "path": source}}),
            );
        }
        write_json(&data.join("registry.json"), json!({"skills": skills}));
        write_json(
            &data.join("config.json"),
            json!({"library_dir": data.join("skills"),
            "recent_projects": [&projects[0]]}),
        );
        let fixture = Self {
            root,
            data,
            projects,
            sources,
            names,
        };
        fixture.openclaw(true);
        fixture
    }

    fn openclaw(&self, secondary: bool) {
        let mut entries = json!({"first": {"workspace": self.projects[0]},
            "same-project": {"workspace": self.projects[0]}});
        if secondary {
            entries["second"] = json!({"workspace": self.projects[1]});
        }
        write_json(
            &self.root.join("home/.openclaw/openclaw.json"),
            json!({"agents": {"entries": entries}}),
        );
    }

    fn library(&self) -> SkillLibrary {
        SkillLibrary::open_in(&self.data).unwrap()
    }

    fn counts(&self, library: &SkillLibrary) -> [usize; 2] {
        let counts = library
            .list()
            .unwrap()
            .into_iter()
            .map(|skill| (skill.record.storage_name, skill.installed_projects))
            .collect::<BTreeMap<_, _>>();
        [counts["source-0"], counts["source-1"]]
    }

    fn cli_counts(&self) -> [usize; 2] {
        let output = Command::new(env!("CARGO_BIN_EXE_kitter"))
            .args(["list", "--json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rows: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
        let counts = rows
            .iter()
            .map(|row| {
                (
                    row["id"].as_str().unwrap(),
                    row["installed_projects"].as_u64().unwrap() as usize,
                )
            })
            .collect::<BTreeMap<_, _>>();
        [counts["id:source-0"], counts["id:source-1"]]
    }
}

// Process-local HOME and an empty search PATH keep discovery off real catalogs.
// Only the test binary and the built public CLI may be spawned.
fn isolated(name: &str, names: [&'static str; 2], run: impl FnOnce(Fixture)) {
    if std::env::var("PLEAT_INSTALL_COUNTS_CHILD").as_deref() == Ok(name) {
        run(Fixture::new(
            PathBuf::from(std::env::var_os("PLEAT_INSTALL_COUNTS_ROOT").unwrap()),
            names,
        ));
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .env_clear()
        .args(["--exact", name, "--nocapture"])
        .env("PLEAT_INSTALL_COUNTS_CHILD", name)
        .env("PLEAT_INSTALL_COUNTS_ROOT", &root)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("PATH", root.join("empty-bin"))
        .env("KITTER_HOME", root.join("data"))
        .env("CODEX_HOME", home.join(".codex"))
        .env("KITTER_CODEX_PROJECTS_HOME", home.join(".codex"))
        .env("KITTER_PROMA_HOME", home.join(".proma"))
        .env("CLAUDE_CONFIG_DIR", &home)
        .env("OPENCLAW_STATE_DIR", home.join(".openclaw"))
        .env("OPENCLAW_CONFIG_PATH", home.join(".openclaw/openclaw.json"))
        .env("HERMES_HOME", home.join(".hermes"))
        .env("GROK_HOME", home.join(".grok"))
        .env("WORKBUDDY_CONFIG_DIR", home.join(".workbuddy"))
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"));
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{name}:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn native_install_counts_include_secondary_and_deduplicate_agents_and_aliases() {
    isolated(
        "native_install_counts_include_secondary_and_deduplicate_agents_and_aliases",
        ["global-probe", "project-probe"],
        |f| {
            let alias = f.root.join("secondary-alias");
            symlink(&f.projects[1], &alias).unwrap();
            write_json(
                &f.root.join("home/.codex/.codex-global-state.json"),
                json!({"electron-saved-workspace-roots": [alias, f.projects[1]]}),
            );
            write_json(
                &f.root.join("home/.claude.json"),
                json!({"projects": {(f.projects[1].to_str().unwrap()): {}}}),
            );
            for project in &f.projects {
                for (source, name) in f.sources.iter().zip(f.names) {
                    link(source, &project.join("skills").join(name));
                }
            }
            // A link in another target is still the same project installation.
            link(
                &f.sources[1],
                &f.projects[1].join(".codex/skills/other-install-name"),
            );
            let library = f.library();
            assert_eq!(f.counts(&library), [2, 2]);
            assert_eq!(f.cli_counts(), [2, 2]);
            fs::remove_file(f.projects[0].join("skills/global-probe")).unwrap();
            assert_eq!(f.counts(&library), [1, 2]);
            assert_eq!(f.cli_counts(), [1, 2]);
            assert!(f.projects[0].join("skills/project-probe").is_dir());
            assert!(f.projects[1].join("skills/global-probe").is_dir());
            assert!(f.projects[1].join("skills/project-probe").is_dir());
        },
    );
}

#[test]
fn native_install_counts_keep_same_name_sources_separate_without_inferring_projects() {
    isolated(
        "native_install_counts_keep_same_name_sources_separate_without_inferring_projects",
        ["probe", "probe"],
        |f| {
            link(&f.sources[0], &f.projects[0].join("skills/probe"));
            link(&f.sources[1], &f.projects[1].join("skills/probe"));
            link(&f.sources[0], &f.projects[1].join(".codex/skills/alias-a"));
            link(&f.sources[0], &f.root.join("unregistered/skills/probe"));
            let library = f.library();
            assert_eq!(f.counts(&library), [2, 1]);
            assert_eq!(f.cli_counts(), [2, 1]);
            fs::remove_file(f.projects[1].join(".codex/skills/alias-a")).unwrap();
            assert_eq!(f.counts(&library), [1, 1]);
        },
    );
}

#[test]
fn native_install_counts_keep_hidden_archived_and_inactive_actual_installations() {
    isolated(
        "native_install_counts_keep_hidden_archived_and_inactive_actual_installations",
        ["global-probe", "project-probe"],
        |f| {
            f.openclaw(false);
            let workspace = f.root.join("home/.proma/agent-workspaces/archived");
            fs::create_dir_all(&workspace).unwrap();
            write_json(
                &f.root.join("home/.proma/agent-workspaces.json"),
                json!({"version": 2,
            "workspaces": [{"id": "archived", "slug": "archived", "name": "Archived",
                "projectRootPath": f.projects[1], "archived": true}]}),
            );
            let copy = workspace.join("skills-inactive/project-probe");
            fs::create_dir_all(&copy).unwrap();
            fs::copy(f.sources[1].join("SKILL.md"), copy.join("SKILL.md")).unwrap();
            // Only provenance is under test: existing is_managed also counts an
            // edited copy, independently of clean/update status or enablement.
            write_json(
                &copy.join(".kitter-proma.json"),
                json!({"schema": 1,
            "workspace_id": "archived", "workspace_slug": "archived", "name": "project-probe",
            "source": f.sources[1], "source_hash": "fixture", "content_hash": "fixture", "local_data": null}),
            );
            link(
                &f.sources[0],
                &f.projects[1].join(".codex/skills/global-probe"),
            );
            let mut library = f.library();
            library
                .config
                .hidden_discovered_projects
                .insert(f.projects[1].clone());
            library.save().unwrap();
            assert_eq!(f.counts(&library), [1, 1]);
            assert_eq!(f.cli_counts(), [1, 1]);
        },
    );
}

#[test]
fn native_install_counts_retain_failed_observations_but_clear_successfully_removed_projects() {
    isolated(
        "native_install_counts_retain_failed_observations_but_clear_successfully_removed_projects",
        ["global-probe", "project-probe"],
        |f| {
            link(&f.sources[0], &f.projects[1].join("skills/global-probe"));
            let library = f.library();
            assert_eq!(f.counts(&library), [1, 0]);
            let config = f.root.join("home/.openclaw/openclaw.json");
            fs::write(&config, b"{invalid fixture").unwrap();
            assert_eq!(f.counts(&library), [1, 0]);
            fs::remove_file(f.projects[1].join("skills/global-probe")).unwrap();
            assert_eq!(f.counts(&library), [0, 0]);
            link(&f.sources[0], &f.projects[1].join("skills/global-probe"));
            assert_eq!(f.counts(&library), [1, 0]);
            f.openclaw(false);
            assert_eq!(f.counts(&library), [0, 0]);
            assert_eq!(f.cli_counts(), [0, 0]);
        },
    );
}
