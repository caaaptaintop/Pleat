//! Controller-only product driver; SDK discovery/trigger evidence belongs to the controller.
#![cfg(unix)]

use kitter::{
    InstallTarget,
    agents::AGENT_ICON_ORDER,
    cross_project::{Action, Catalog, Disposition, Plan, Skill},
    effective_skills::AgentKind,
    project,
    project_discovery::ProjectEntry,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

const NAME: &str = "pleat-pi-project-probe";
type Result<T> = std::result::Result<T, &'static str>;

fn require(ok: bool, code: &'static str) -> Result<()> {
    if ok { Ok(()) } else { Err(code) }
}

fn directory(key: &str) -> Result<PathBuf> {
    let path = PathBuf::from(std::env::var_os(key).ok_or("MISSING_PARAMETER")?);
    require(
        path.is_absolute()
            && path
                .components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_))),
        "INVALID_PATH",
    )?;
    require(
        path.canonicalize().ok().as_ref() == Some(&path) && path.is_dir(),
        "NON_CANONICAL_DIRECTORY",
    )?;
    Ok(path)
}

// Snapshots stay in memory: never serialize fixture bodies or filesystem errors.
#[derive(PartialEq, Eq)]
struct Entry {
    identity: (u64, u64),
    mode: u32,
    bytes: Vec<u8>,
    link: Option<PathBuf>,
}
type Snapshot = BTreeMap<PathBuf, Entry>;

fn snapshot(path: &Path, fixture: &Path, allow_links: bool) -> Result<Snapshot> {
    fn visit(path: &Path, fixture: &Path, links: bool, out: &mut Snapshot) -> Result<()> {
        let meta = fs::symlink_metadata(path).map_err(|_| "SNAPSHOT_METADATA_FAILED")?;
        let mut entry = Entry {
            identity: (meta.dev(), meta.ino()),
            mode: meta.mode(),
            bytes: Vec::new(),
            link: None,
        };
        if meta.is_symlink() {
            require(links, "SOURCE_SYMLINK_REFUSED")?;
            let resolved = path.canonicalize().map_err(|_| "UNRESOLVED_FIXTURE_LINK")?;
            require(resolved.starts_with(fixture), "LINK_OUTSIDE_FIXTURE")?;
            entry.link = Some(fs::read_link(path).map_err(|_| "LINK_READ_FAILED")?);
        } else if meta.is_file() {
            entry.bytes = fs::read(path).map_err(|_| "SNAPSHOT_READ_FAILED")?;
        } else {
            require(meta.is_dir(), "SPECIAL_FILE_REFUSED")?;
        }
        out.insert(path.to_path_buf(), entry);
        if meta.is_dir() {
            for child in fs::read_dir(path).map_err(|_| "SNAPSHOT_DIRECTORY_FAILED")? {
                visit(
                    &child.map_err(|_| "SNAPSHOT_ENTRY_FAILED")?.path(),
                    fixture,
                    links,
                    out,
                )?;
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    visit(path, fixture, allow_links, &mut out)?;
    Ok(out)
}

fn absent(path: &Path) -> bool {
    fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

fn report_path(reports: &Path, op: &str) -> PathBuf {
    reports.join(format!("pi-shared-native-{op}.json"))
}

fn prior(fixture: &Path, reports: &Path, op: &str, project: &Path, source: &Path) -> Result<Value> {
    let path = report_path(reports, op);
    require(
        fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()),
        "NOT_RERUNNABLE",
    )?;
    let value: Value = serde_json::from_slice(&fs::read(path).map_err(|_| "NOT_RERUNNABLE")?)
        .map_err(|_| "NOT_RERUNNABLE")?;
    require(
        value["code"] == "PRODUCT_OPERATION_VERIFIED"
            && value["operation"] == op
            && value["fixture"] == json!(fixture)
            && value["project"] == json!(project)
            && value["source"] == json!(source),
        "NOT_RERUNNABLE",
    )?;
    Ok(value)
}

fn catalog(project: &Path, source: &Path) -> Catalog {
    Catalog::scan(
        vec![Skill {
            key: format!("file:{}", source.join("SKILL.md").display()),
            name: NAME.into(),
            source: source.into(),
            provider: "library",
            install_name: Some(NAME.into()),
        }],
        vec![ProjectEntry {
            path: project.into(),
            name: "controller Pi shared fixture".into(),
            agents: vec![AgentKind::Pi],
            proma_workspaces: vec![],
            native_workspaces: vec![],
            archive_states: vec![],
            archived: false,
            hidden: false,
            retained: false,
        }],
        AGENT_ICON_ORDER
            .iter()
            .filter(|a| ["pi", "dimagent"].contains(&a.id))
            .copied()
            .collect(),
        None,
    )
}

fn plan(project: &Path, source: &Path, action: Action, targets: &[&str]) -> Result<Plan> {
    catalog(project, source)
        .plan(
            action,
            &BTreeSet::from([format!("file:{}", source.join("SKILL.md").display())]),
            &targets.iter().map(|s| s.to_string()).collect(),
            &BTreeSet::from([project.to_path_buf()]),
            &BTreeSet::new(),
        )
        .map_err(|_| "PRODUCT_PLAN_FAILED")
}

fn link_ok(path: &Path, source: &Path) -> bool {
    path.is_symlink() && path.canonicalize().ok().as_deref() == Some(source)
}

fn pi_descriptor(path: &Path, project: &Path) -> bool {
    path.parent() == Some(project.join(".pi/extensions").as_path())
        && path.file_name().is_some_and(|n| {
            let n = n.to_string_lossy();
            n.starts_with("pleat-adaptation-")
        })
}

fn operate(fixture: &Path, reports: &Path, op: &str, report: &mut Value) -> Result<()> {
    let project = directory("PLEAT_PI_SHARED_PROJECT")?;
    let source = directory("PLEAT_PI_SHARED_SOURCE")?;
    require(
        project.starts_with(fixture)
            && project != fixture
            && source == fixture.join("source").join(NAME)
            && !source.starts_with(&project)
            && !project.starts_with(&source)
            && !reports.starts_with(&source)
            && !reports.starts_with(&project),
        "INVALID_FIXTURE_LAYOUT",
    )?;
    report["project"] = json!(project);
    report["source"] = json!(source);
    let source_before = snapshot(&source, fixture, false)?;
    let body = fs::read_to_string(source.join("SKILL.md")).map_err(|_| "INVALID_FIXTURE_SKILL")?;
    let mut lines = body.lines();
    require(lines.next() == Some("---"), "INVALID_FIXTURE_SKILL")?;
    let mut front = Vec::new();
    let mut closed = false;
    for line in lines {
        if line == "---" {
            closed = true;
            break;
        }
        front.push(line);
    }
    require(
        closed
            && front
                .iter()
                .filter(|line| line.starts_with("name:"))
                .copied()
                .collect::<Vec<_>>()
                == vec!["name: pleat-pi-project-probe"]
            && source.join("local-data/ADAPTATION.md").is_file(),
        "INVALID_FIXTURE_SKILL",
    )?;
    // Capture source, ADAPTATION, all local-data bytes and every directory dev:ino.
    let project_before = snapshot(&project, fixture, true)?;
    let fixture_meta = fs::metadata(fixture).map_err(|_| "FIXTURE_METADATA_FAILED")?;
    let native = project.join(".pi/skills").join(NAME);
    let shared = project.join(".agents/skills").join(NAME);
    report["link_paths"] = json!({"native": native, "shared": shared});
    let mut removed = BTreeSet::new();

    let result = (|| -> Result<()> {
        let install = plan(&project, &source, Action::Install, &["pi", "dimagent"])?;
        require(
            install.items.len() == 2
                && install
                    .items
                    .iter()
                    .map(|i| i.path.clone())
                    .collect::<BTreeSet<_>>()
                    == BTreeSet::from([native.clone(), shared.clone()]),
            "UNEXPECTED_INSTALL_PLAN",
        )?;
        let descriptors = install
            .items
            .iter()
            .flat_map(|i| &i.instruction_paths)
            .cloned()
            .collect::<BTreeSet<_>>();
        require(
            descriptors.len() == 2 && descriptors.iter().all(|p| pi_descriptor(p, &project)),
            "UNEXPECTED_DESCRIPTOR_PLAN",
        )?;
        report["instruction_paths"] = json!(descriptors);
        for path in &descriptors {
            // Parent components may be absent, but must never be aliases.
            let mut parent = path.parent();
            while let Some(p) = parent {
                if p == project {
                    break;
                }
                if !absent(p) {
                    require(!p.is_symlink() && p.is_dir(), "ALIASED_TARGET_PARENT")?;
                }
                parent = p.parent();
            }
        }
        for root in [project.join(".pi/skills"), project.join(".agents/skills")] {
            if !absent(&root) {
                require(!root.is_symlink() && root.is_dir(), "ALIASED_TARGET_PARENT")?;
            }
        }
        if op == "install" {
            for root in [project.join(".pi/skills"), project.join(".agents/skills")] {
                require(
                    absent(&root)
                        || fs::read_dir(root).is_ok_and(|mut entries| entries.next().is_none()),
                    "NOT_RERUNNABLE",
                )?;
            }
            require(
                absent(&report_path(reports, "remove-native"))
                    && absent(&report_path(reports, "remove-shared"))
                    && absent(&native)
                    && absent(&shared)
                    && descriptors.iter().all(|p| absent(p))
                    && !project_before.keys().any(|p| {
                        p.file_name().is_some_and(|n| {
                            let n = n.to_string_lossy();
                            n.starts_with("pleat-adaptation-")
                                || n.starts_with(".pleat-removed-")
                                || n.starts_with(".kitter-removed-")
                        })
                    })
                    && install
                        .items
                        .iter()
                        .all(|i| i.disposition == Disposition::Change),
                "NOT_RERUNNABLE",
            )?;
            let applied = install.execute();
            require(
                applied.changed() == 2 && applied.failed() == 0,
                "PRODUCT_INSTALL_FAILED",
            )?;
            report["operation_instruction_paths"] = json!(descriptors);
        } else {
            let previous = prior(fixture, reports, "install", &project, &source)?;
            require(
                previous["instruction_paths"] == json!(descriptors),
                "NOT_RERUNNABLE",
            )?;
            let mapping = previous["descriptors_by_link"]
                .as_object()
                .ok_or("NOT_RERUNNABLE")?;
            let descriptor_for = |link: &Path| -> Result<PathBuf> {
                let path = PathBuf::from(
                    mapping
                        .get(link.to_str().ok_or("INVALID_PATH")?)
                        .and_then(Value::as_str)
                        .ok_or("NOT_RERUNNABLE")?,
                );
                require(descriptors.contains(&path), "NOT_RERUNNABLE")?;
                Ok(path)
            };
            let native_descriptor = descriptor_for(&native)?;
            let shared_descriptor = descriptor_for(&shared)?;
            require(native_descriptor != shared_descriptor, "NOT_RERUNNABLE")?;
            require(
                link_ok(&shared, &source)
                    && shared_descriptor.is_file()
                    && !shared_descriptor.is_symlink(),
                "NOT_RERUNNABLE",
            )?;
            if op == "remove-native" {
                require(
                    absent(&report_path(reports, "remove-shared"))
                        && link_ok(&native, &source)
                        && native_descriptor.is_file()
                        && !native_descriptor.is_symlink(),
                    "NOT_RERUNNABLE",
                )?;
                let listed = project::list(&project, source.parent().ok_or("INVALID_PATH")?)
                    .map_err(|_| "PRODUCT_LIST_FAILED")?;
                let selected = listed
                    .iter()
                    .flat_map(|s| &s.installations)
                    .filter(|i| i.path == native && i.target == InstallTarget::Pi)
                    .collect::<Vec<_>>();
                require(
                    selected.len() == 1
                        && selected[0].managed
                        && link_ok(&selected[0].path, &source),
                    "UNEXPECTED_NATIVE_SELECTION",
                )?;
                removed.extend([native.clone(), native_descriptor.clone()]);
                report["operation_instruction_paths"] = json!([native_descriptor]);
                let applied = project::remove_project_skills(selected);
                require(
                    applied.removed == 1 && applied.failures.is_empty(),
                    "PRODUCT_REMOVE_NATIVE_FAILED",
                )?;
                require(
                    absent(&native)
                        && absent(&native_descriptor)
                        && link_ok(&shared, &source)
                        && shared_descriptor.is_file(),
                    "NATIVE_REMOVAL_POSTCONDITION_FAILED",
                )?;
                report["controller_next_step"] = json!(
                    "Run actual Pi SDK default discovery; selected SKILL.md must be shared .agents path"
                );
            } else {
                prior(fixture, reports, "remove-native", &project, &source)?;
                require(
                    absent(&native) && absent(&native_descriptor),
                    "NOT_RERUNNABLE",
                )?;
                let removal = plan(&project, &source, Action::Remove, &["dimagent"])?;
                require(
                    removal.items.len() == 1
                        && removal.items[0].path == shared
                        && removal.items[0].disposition == Disposition::Change
                        && removal.items[0].instruction_paths == vec![shared_descriptor.clone()],
                    "UNEXPECTED_SHARED_SELECTION",
                )?;
                removed.extend([shared.clone(), shared_descriptor.clone()]);
                report["operation_instruction_paths"] = json!([shared_descriptor]);
                let applied = removal.execute();
                require(
                    applied.changed() == 1 && applied.failed() == 0,
                    "PRODUCT_REMOVE_SHARED_FAILED",
                )?;
                require(
                    absent(&native) && absent(&shared) && descriptors.iter().all(|p| absent(p)),
                    "SHARED_REMOVAL_POSTCONDITION_FAILED",
                )?;
            }
        }
        if op == "install" {
            require(
                link_ok(&native, &source) && link_ok(&shared, &source),
                "INSTALL_LINK_POSTCONDITION_FAILED",
            )?;
            let mut mapping = BTreeMap::new();
            for link in [&native, &shared] {
                let needle =
                    serde_json::to_string(&link.join("SKILL.md")).map_err(|_| "INVALID_PATH")?;
                let matches = descriptors
                    .iter()
                    .filter(|p| {
                        !p.is_symlink() && fs::read_to_string(p).is_ok_and(|s| s.contains(&needle))
                    })
                    .collect::<Vec<_>>();
                require(matches.len() == 1, "DESCRIPTOR_BINDING_FAILED")?;
                mapping.insert(link.clone(), matches[0].clone());
            }
            require(
                mapping.values().collect::<BTreeSet<_>>().len() == 2,
                "DESCRIPTOR_BINDING_FAILED",
            )?;
            report["descriptors_by_link"] = json!(mapping);
        }
        Ok(())
    })();
    // Always verify preservation, including a partially failed product operation.
    let source_preserved = snapshot(&source, fixture, false).is_ok_and(|s| s == source_before);
    let project_after = snapshot(&project, fixture, true);
    let others_preserved = project_after.as_ref().is_ok_and(|after| {
        project_before
            .iter()
            .filter(|(p, _)| !removed.contains(*p))
            .all(|(p, entry)| after.get(p) == Some(entry))
    });
    let fixture_preserved = fs::metadata(fixture)
        .is_ok_and(|m| (m.dev(), m.ino()) == (fixture_meta.dev(), fixture_meta.ino()));
    report["preservation"] = json!({
        "source_skill_adaptation_local_data_bytes_and_directory_dev_ino": source_preserved,
        "all_other_existing_project_entries_bytes_mode_dev_ino": others_preserved,
        "fixture_directory_dev_ino": fixture_preserved,
    });
    report["actual_links"] =
        json!({"native": link_ok(&native, &source), "shared": link_ok(&shared, &source)});
    require(
        source_preserved && others_preserved && fixture_preserved,
        "PRESERVATION_FAILED",
    )?;
    result
}

pub(super) fn operate_isolated_native_pi_shared_root() -> Result<()> {
    let fixture = directory("PLEAT_PI_SHARED_FIXTURE")?;
    require(
        fixture.parent() == Some(Path::new("/private/tmp"))
            && fixture.file_name().is_some_and(|n| {
                n.to_string_lossy().starts_with("pleat-native-pi-")
                    && n.to_string_lossy().len() > "pleat-native-pi-".len()
            }),
        "INVALID_FIXTURE",
    )?;
    let op = std::env::var("PLEAT_PI_SHARED_OP").map_err(|_| "MISSING_PARAMETER")?;
    require(
        matches!(op.as_str(), "install" | "remove-native" | "remove-shared"),
        "INVALID_OPERATION",
    )?;
    let reports = directory("PLEAT_PI_SHARED_REPORT_DIR")?;
    require(
        reports != fixture && reports.starts_with(&fixture),
        "INVALID_REPORT_DIRECTORY",
    )?;
    // Exclusive create is a one-shot guard, not an installation transaction.
    let mut output: File = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(report_path(&reports, &op))
        .map_err(|_| "NOT_RERUNNABLE")?;
    let mut report = json!({"operation": op, "fixture": fixture,
        "native_sdk_run": false, "model_route_verified": false});
    let result = operate(&fixture, &reports, &op, &mut report);
    report["code"] = json!(
        result
            .as_ref()
            .err()
            .copied()
            .unwrap_or("PRODUCT_OPERATION_VERIFIED")
    );
    report["rerunnable"] = json!(false);
    output
        .write_all(&serde_json::to_vec_pretty(&report).map_err(|_| "REPORT_SERIALIZATION_FAILED")?)
        .map_err(|_| "REPORT_WRITE_FAILED")?;
    output.sync_all().map_err(|_| "REPORT_WRITE_FAILED")?;
    result
}
