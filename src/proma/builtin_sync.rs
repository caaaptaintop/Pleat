//! Opt-in following of existing native Proma presets. No registry or package
//! markers are written. A preview is a read-only, reproducible frozen request.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub fn bundled_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(PathBuf::from(
            "/Applications/Proma.app/Contents/Resources/default-skills",
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::fs;
    use std::os::unix::fs::symlink;

    struct Fixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        bundle: PathBuf,
        state: PathBuf,
    }
    fn package(path: &Path, body: &str) {
        fs::create_dir_all(path.join("scripts")).unwrap();
        fs::write(
            path.join("SKILL.md"),
            format!("---\nname: example\nversion: 1.0\n---\n{body}\n"),
        )
        .unwrap();
        fs::write(path.join("scripts/tool.py"), "print('original')\n").unwrap();
    }
    impl Fixture {
        fn new(old: &str, app: &str) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let base = temp.path().canonicalize().unwrap();
            let fixture = Self {
                root: base.join("proma"),
                bundle: base.join("app/default-skills"),
                state: base.join("follow-state"),
                _temp: temp,
            };
            fs::create_dir_all(fixture.root.join("agent-workspaces")).unwrap();
            fs::write(
                fixture.root.join("agent-workspaces.json"),
                serde_json::to_vec(&json!({
                    "version": 2,
                    "workspaces": [
                        {"id":"one", "slug":"one", "name":"One"},
                        {"id":"two", "slug":"two", "name":"Two", "archived":true}
                    ]
                }))
                .unwrap(),
            )
            .unwrap();
            package(&fixture.bundle.join("example"), app);
            for path in fixture.copies() {
                package(&path, old);
            }
            fixture
        }
        fn copies(&self) -> [PathBuf; 3] {
            [
                self.root.join("default-skills/example"),
                self.root.join("agent-workspaces/one/skills/example"),
                self.root
                    .join("agent-workspaces/two/skills-inactive/example"),
            ]
        }
        fn preview(&self) -> FollowPlan {
            preview(&self.root, &self.bundle, &self.state).unwrap()
        }
        fn apply(&self) -> super::super::LocalSyncReport {
            apply(&self.root, &self.bundle, &self.state, &self.preview()).unwrap()
        }
        fn poll(&self, follower: &mut BuiltinFollower) -> super::super::LocalSyncReport {
            follower
                .poll(&self.root, &self.bundle, &self.state)
                .unwrap()
        }
        fn contents(&self) -> Vec<String> {
            self.copies()
                .iter()
                .map(|p| fs::read_to_string(p.join("SKILL.md")).unwrap())
                .collect()
        }
        fn records(&self, index: usize) -> Vec<PathBuf> {
            fs::read_dir(
                self.copies()[index]
                    .parent()
                    .unwrap()
                    .join(".pleat-builtin-recovery"),
            )
            .unwrap()
            .map(|p| p.unwrap().path().join("transaction.json"))
            .collect()
        }
    }

    #[test]
    fn preview_and_disabled_poll_are_read_only_then_explicit_apply_updates_all_slots() {
        let f = Fixture::new("old", "new");
        let plan = f.preview();
        assert!(!f.state.exists());
        assert!(!f.root.join(".kitter-proma.lock").exists());
        let mut follower = BuiltinFollower::default();
        assert_eq!(f.poll(&mut follower).updated, 0);
        assert!(!f.state.exists());
        let report = apply(&f.root, &f.bundle, &f.state, &plan).unwrap();
        assert_eq!(report.updated, 3, "{:?}", report.warnings);
        assert_eq!(report.inactive_updated, 1);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert!(f.contents().iter().all(|s| s.ends_with("new\n")));
        assert!(!f.root.join("agent-workspaces/two/skills").exists());
        for copy in f.copies() {
            assert!(!copy.join(".kitter-proma.json").exists());
        }
        for index in 0..3 {
            let record: Value =
                serde_json::from_slice(&fs::read(&f.records(index)[0]).unwrap()).unwrap();
            assert_eq!(record["phase"], "committed");
            let backup = f.records(index)[0]
                .parent()
                .unwrap()
                .join("previous/SKILL.md");
            assert!(fs::read_to_string(backup).unwrap().ends_with("old\n"));
        }
    }

    #[test]
    fn stable_same_version_script_changes_follow_after_restart_and_app_replacement() {
        let f = Fixture::new("old", "new");
        assert_eq!(f.apply().updated, 3);
        let moved = f.bundle.with_file_name("old-default-skills");
        fs::rename(&f.bundle, moved).unwrap();
        package(&f.bundle.join("example"), "same-version-change");
        fs::write(
            f.bundle.join("example/scripts/tool.py"),
            "print('changed')\n",
        )
        .unwrap();
        let mut restarted = BuiltinFollower::default();
        assert_eq!(f.poll(&mut restarted).updated, 0);
        assert_eq!(f.poll(&mut restarted).updated, 3);
        for path in f.copies() {
            assert_eq!(
                fs::read_to_string(path.join("scripts/tool.py")).unwrap(),
                "print('changed')\n"
            );
        }
        // Script-only changes count even without editing SKILL.md or its version.
        fs::write(
            f.bundle.join("example/scripts/tool.py"),
            "print('script only')\n",
        )
        .unwrap();
        assert_eq!(f.poll(&mut restarted).updated, 0);
        assert_eq!(f.poll(&mut restarted).updated, 3);
        assert_eq!(f.poll(&mut restarted).updated, 0);
    }

    #[test]
    fn changing_source_requires_two_identical_observations() {
        let f = Fixture::new("old", "old");
        f.apply();
        let mut follower = BuiltinFollower::default();
        package(&f.bundle.join("example"), "first");
        assert_eq!(f.poll(&mut follower).updated, 0);
        package(&f.bundle.join("example"), "second");
        assert_eq!(f.poll(&mut follower).updated, 0);
        assert!(f.contents().iter().all(|s| s.ends_with("old\n")));
        assert_eq!(f.poll(&mut follower).updated, 3);
    }

    #[test]
    fn unchanged_poll_keeps_the_existing_state_file() {
        use std::os::unix::fs::MetadataExt;
        let f = Fixture::new("old", "new");
        f.apply();
        let state_file = f.state.join("builtin-follow.json");
        let before = fs::metadata(&state_file).unwrap().ino();
        let mut follower = BuiltinFollower::default();
        assert_eq!(f.poll(&mut follower).updated, 0);
        assert_eq!(f.poll(&mut follower).updated, 0);
        assert_eq!(
            fs::metadata(&state_file).unwrap().ino(),
            before,
            "an unchanged poll must not rewrite the durable authorization state"
        );
    }

    #[test]
    fn native_agent_updates_refresh_catalog_without_rewriting_copies() {
        let f = Fixture::new("old", "new");
        f.apply();
        let mut follower = BuiltinFollower::default();
        // B, then the already approved A: history membership must not suppress
        // a notification for a different current catalog.
        for body in ["agent-updated", "new"] {
            package(&f.bundle.join("example"), body);
            for copy in f.copies() {
                package(&copy, body);
            }
            assert_eq!(f.poll(&mut follower).updated, 0);
            let report = f.poll(&mut follower);
            assert_eq!(report.updated, 0);
            assert!(report.catalog_changed);
            let report = f.poll(&mut follower);
            assert_eq!(report.updated, 0);
            assert!(!report.catalog_changed);
        }
        // Native removal is observed without Pleat deleting or injecting copies.
        fs::remove_dir_all(f.bundle.join("example")).unwrap();
        for copy in f.copies() {
            fs::remove_dir_all(copy).unwrap();
        }
        assert_eq!(f.poll(&mut follower).updated, 0);
        let report = f.poll(&mut follower);
        assert_eq!(report.updated, 0);
        assert!(report.catalog_changed);
        let report = f.poll(&mut follower);
        assert_eq!(report.updated, 0);
        assert!(!report.catalog_changed);
    }

    #[test]
    fn unknown_local_edits_and_new_cache_matches_are_not_adopted_by_poll() {
        let f = Fixture::new("old", "old");
        f.apply();
        // Existing tracked copy changed by the user.
        fs::write(f.copies()[1].join("scripts/tool.py"), "user edit").unwrap();
        // A newly registered workspace matches the local template, but not App.
        let new_copy = f.root.join("agent-workspaces/three/skills/example");
        package(&new_copy, "old");
        let mut index: Value =
            serde_json::from_slice(&fs::read(f.root.join("agent-workspaces.json")).unwrap())
                .unwrap();
        index["workspaces"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"three","slug":"three","name":"Three"}));
        fs::write(
            f.root.join("agent-workspaces.json"),
            serde_json::to_vec(&index).unwrap(),
        )
        .unwrap();
        package(&f.bundle.join("example"), "updated");
        let mut follower = BuiltinFollower::default();
        f.poll(&mut follower);
        let report = f.poll(&mut follower);
        assert_eq!(report.updated, 2);
        assert!(report.warnings.len() >= 2);
        assert_eq!(
            fs::read_to_string(f.copies()[1].join("scripts/tool.py")).unwrap(),
            "user edit"
        );
        assert!(
            fs::read_to_string(new_copy.join("SKILL.md"))
                .unwrap()
                .ends_with("old\n")
        );
    }

    #[test]
    fn unknown_initial_copy_is_conflict_and_exact_bundle_copy_can_join_later() {
        let f = Fixture::new("old", "new");
        package(&f.copies()[1], "custom");
        let report = f.apply();
        assert_eq!(report.updated, 2);
        assert_eq!(report.warnings.len(), 1);
        package(&f.copies()[1], "new");
        let mut follower = BuiltinFollower::default();
        f.poll(&mut follower);
        assert_eq!(f.poll(&mut follower).updated, 0); // baseline adoption
        package(&f.bundle.join("example"), "next");
        f.poll(&mut follower);
        assert_eq!(f.poll(&mut follower).updated, 3);
    }

    #[test]
    fn private_data_links_and_unknown_cache_are_preserved() {
        for private in [
            "local-data/secret",
            ".env",
            ".git/config",
            "__pycache__/user.txt",
            "custom-link",
        ] {
            let f = Fixture::new("old", "new");
            let path = f.copies()[1].join(private);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            if private == "custom-link" {
                symlink(f.bundle.join("example/SKILL.md"), &path).unwrap();
            } else {
                fs::write(&path, "fixture-private-do-not-print").unwrap();
            }
            let report = f.apply();
            assert_eq!(report.updated, 2, "{private}: {:?}", report.warnings);
            assert!(
                fs::read_to_string(f.copies()[1].join("SKILL.md"))
                    .unwrap()
                    .ends_with("old\n")
            );
            assert!(fs::symlink_metadata(path).is_ok());
            assert!(!format!("{:?}", report).contains("fixture-private-do-not-print"));
        }
        let f = Fixture::new("old", "new");
        let cache = f.copies()[1].join("scripts/__pycache__");
        fs::create_dir(&cache).unwrap();
        fs::write(
            cache.join("tool.cpython-313.pyc"),
            [0xf3, 0x0d, 13, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        )
        .unwrap();
        assert_eq!(f.apply().updated, 3);
        assert!(!cache.exists());
    }

    #[test]
    fn frozen_plan_rejects_target_parent_registry_and_bundle_replacements() {
        for changed in ["target", "parent", "registry", "bundle", "root", "symlink"] {
            let f = Fixture::new("old", "new");
            let plan = f.preview();
            match changed {
                "target" => {
                    fs::rename(&f.copies()[1], f.copies()[1].with_file_name("saved")).unwrap();
                    package(&f.copies()[1], "old");
                }
                "parent" => {
                    let p = f.copies()[1].parent().unwrap().to_owned();
                    fs::rename(&p, p.with_file_name("saved")).unwrap();
                    package(&f.copies()[1], "old");
                }
                "registry" => {
                    let p = f.root.join("agent-workspaces.json");
                    let data = fs::read(&p).unwrap();
                    fs::rename(&p, p.with_extension("saved")).unwrap();
                    fs::write(p, data).unwrap();
                }
                "bundle" => {
                    fs::rename(&f.bundle, f.bundle.with_file_name("saved")).unwrap();
                    package(&f.bundle.join("example"), "new");
                }
                "root" => {
                    fs::rename(&f.root, f.root.with_file_name("saved")).unwrap();
                    fs::create_dir(&f.root).unwrap();
                }
                "symlink" => {
                    let p = f.copies()[1].parent().unwrap().to_owned();
                    let saved = p.with_file_name("saved");
                    fs::rename(&p, &saved).unwrap();
                    symlink(saved, p).unwrap();
                }
                _ => unreachable!(),
            }
            assert!(
                apply(&f.root, &f.bundle, &f.state, &plan).is_err(),
                "{changed}"
            );
            assert!(!f.state.exists(), "{changed}");
        }
    }

    #[test]
    fn serialized_plan_actions_paths_and_baselines_cannot_authorize_writes() {
        for changed in ["path", "action", "before", "source", "state"] {
            let f = Fixture::new("old", "new");
            package(&f.copies()[1], "custom");
            let mut plan = serde_json::to_value(f.preview()).unwrap();
            match changed {
                "path" => plan["targets"][0]["slot"]["path"] = json!(f.bundle),
                "action" => plan["targets"][1]["action"] = json!("Update"),
                "before" => plan["targets"][0]["before"]["content"] = json!("forged"),
                "source" => plan["bundled"] = json!(f.root),
                "state" => plan["state"]["parent"] = json!("forged"),
                _ => unreachable!(),
            }
            let plan: FollowPlan = serde_json::from_value(plan).unwrap();
            assert!(
                apply(&f.root, &f.bundle, &f.state, &plan).is_err(),
                "{changed}"
            );
            assert!(!f.state.exists());
        }
    }

    #[test]
    fn failure_after_backup_restores_original_and_records_evidence() {
        let f = Fixture::new("old", "new");
        let source = f.bundle.join("example/SKILL.md");
        platform::AFTER_BACKUP.with(|h| {
            *h.borrow_mut() = Some(Box::new(move || {
                fs::write(source, "changing source").unwrap()
            }))
        });
        let report = f.apply();
        assert_eq!(report.updated, 0);
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("已回退") && w.contains("恢复证据"))
        );
        assert!(f.contents().iter().all(|s| s.ends_with("old\n")));
        let record: Value = serde_json::from_slice(&fs::read(&f.records(0)[0]).unwrap()).unwrap();
        assert_eq!(record["phase"], "rolled_back");
    }

    #[test]
    fn review_staging_change_or_replacement_never_reaches_native_slot() {
        for replace_root in [false, true] {
            let f = Fixture::new("old", "new");
            let recovery = f.copies()[0]
                .parent()
                .unwrap()
                .join(".pleat-builtin-recovery");
            platform::BEFORE_PUBLISH.with(|h| {
                *h.borrow_mut() = Some(Box::new(move || {
                    let operation = fs::read_dir(&recovery)
                        .unwrap()
                        .next()
                        .unwrap()
                        .unwrap()
                        .path();
                    let staging = operation.join("staging");
                    if replace_root {
                        fs::rename(&staging, operation.join("staging-before-race")).unwrap();
                        package(&staging, "unverified replacement");
                    } else {
                        fs::write(staging.join("SKILL.md"), "unverified staging edit\n").unwrap();
                    }
                }));
            });
            let report = f.apply();
            assert_eq!(report.updated, 2, "{:?}", report.warnings);
            assert!(f.contents()[0].ends_with("old\n"), "{:?}", report.warnings);
            let record: Value =
                serde_json::from_slice(&fs::read(&f.records(0)[0]).unwrap()).unwrap();
            assert_eq!(record["phase"], "rolled_back");
        }
    }

    #[test]
    fn review_modified_published_staging_is_quarantined_before_rollback() {
        let f = Fixture::new("old", "new");
        let path = f.copies()[0].clone();
        platform::AFTER_PUBLISH.with(|h| {
            *h.borrow_mut() = Some(Box::new(move || {
                fs::write(path.join("SKILL.md"), "unverified published edit\n").unwrap();
            }));
        });
        let report = f.apply();
        assert_eq!(report.updated, 2, "{:?}", report.warnings);
        assert!(f.contents()[0].ends_with("old\n"));
        let operation = f.records(0)[0].parent().unwrap().to_owned();
        assert_eq!(
            fs::read_to_string(operation.join("failed/SKILL.md")).unwrap(),
            "unverified published edit\n"
        );
        let record: Value =
            serde_json::from_slice(&fs::read(operation.join("transaction.json")).unwrap()).unwrap();
        assert_eq!(record["phase"], "rolled_back");
    }

    #[test]
    fn review_modified_previous_is_restored_and_not_committed() {
        for after_publication in [false, true] {
            let f = Fixture::new("old", "new");
            let recovery = f.copies()[0]
                .parent()
                .unwrap()
                .join(".pleat-builtin-recovery");
            let modify: Box<dyn FnOnce()> = Box::new(move || {
                let operation = fs::read_dir(recovery)
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path();
                fs::write(
                    operation.join("previous/SKILL.md"),
                    "concurrent original edit\n",
                )
                .unwrap();
            });
            if after_publication {
                platform::AFTER_PUBLISH.with(|h| *h.borrow_mut() = Some(modify));
            } else {
                platform::AFTER_BACKUP.with(|h| *h.borrow_mut() = Some(modify));
            }
            let report = f.apply();
            assert_eq!(report.updated, 2, "{:?}", report.warnings);
            assert_eq!(f.contents()[0], "concurrent original edit\n");
            assert!(report.warnings.iter().any(|w| w.contains("已回退")));
            let record: Value =
                serde_json::from_slice(&fs::read(&f.records(0)[0]).unwrap()).unwrap();
            assert_eq!(record["phase"], "rolled_back");
            assert_eq!(record["restored"]["identity"], record["before"]["identity"]);
            assert_ne!(record["restored"]["content"], record["before"]["content"]);
        }
    }

    #[test]
    fn failed_publication_preserves_concurrent_object_and_blocks_pending_recovery() {
        let f = Fixture::new("old", "new");
        let path = f.copies()[0].clone();
        platform::BEFORE_PUBLISH
            .with(|h| *h.borrow_mut() = Some(Box::new(move || package(&path, "concurrent"))));
        let report = f.apply();
        assert_eq!(report.updated, 2, "{:?}", report.warnings);
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("回退未完成") && w.contains("恢复证据"))
        );
        assert!(f.contents()[0].ends_with("concurrent\n"));
        let record = &f.records(0)[0];
        assert!(
            fs::read_to_string(record.parent().unwrap().join("previous/SKILL.md"))
                .unwrap()
                .ends_with("old\n")
        );
        // Even removing that occupant cannot inject a missing copy or bypass the record.
        fs::rename(&f.copies()[0], f.root.join("concurrent-saved")).unwrap();
        let mut follower = BuiltinFollower::default();
        f.poll(&mut follower);
        let report = f.poll(&mut follower);
        assert!(report.warnings.iter().any(|w| w.contains("未完成恢复记录")));
        assert!(!f.copies()[0].exists());
    }

    #[test]
    fn rollback_race_never_overwrites_new_object() {
        let f = Fixture::new("old", "new");
        let source = f.bundle.join("example/SKILL.md");
        platform::AFTER_PUBLISH.with(|h| {
            *h.borrow_mut() = Some(Box::new(move || {
                fs::write(source, "source changed").unwrap()
            }))
        });
        let path = f.copies()[0].clone();
        platform::BEFORE_ROLLBACK.with(|h| {
            *h.borrow_mut() = Some(Box::new(move || {
                fs::rename(&path, path.with_file_name("published-saved")).unwrap();
                package(&path, "concurrent rollback object");
            }))
        });
        let report = f.apply();
        assert_eq!(report.updated, 0);
        assert!(f.contents()[0].ends_with("concurrent rollback object\n"));
        assert!(report.warnings.iter().any(|w| w.contains("回退未完成")));
        assert!(
            fs::read_to_string(f.records(0)[0].parent().unwrap().join("previous/SKILL.md"))
                .unwrap()
                .ends_with("old\n")
        );
    }

    #[test]
    fn retired_core_missing_and_unregistered_copies_are_not_changed() {
        let f = Fixture::new("old", "new");
        for name in ["proma-coach", "knowledge-maintenance", "retired"] {
            package(
                &f.root.join("default-skills").join(name),
                "custom preserved",
            );
            if name != "retired" {
                package(&f.bundle.join(name), "app");
            }
        }
        let unregistered = f.root.join("agent-workspaces/unregistered/skills/example");
        package(&unregistered, "old");
        fs::rename(&f.copies()[2], f.root.join("removed-copy")).unwrap();
        assert_eq!(f.apply().updated, 2);
        assert!(!f.copies()[2].exists());
        assert!(
            fs::read_to_string(unregistered.join("SKILL.md"))
                .unwrap()
                .ends_with("old\n")
        );
        for name in ["proma-coach", "knowledge-maintenance", "retired"] {
            assert!(
                fs::read_to_string(f.root.join("default-skills").join(name).join("SKILL.md"))
                    .unwrap()
                    .ends_with("custom preserved\n")
            );
        }
    }

    #[test]
    fn persisted_state_is_bound_to_root_identity_and_rejects_replacement() {
        let f = Fixture::new("old", "old");
        f.apply();
        fs::rename(&f.root, f.root.with_file_name("original-root")).unwrap();
        fs::create_dir(&f.root).unwrap();
        let mut follower = BuiltinFollower::default();
        assert!(follower.poll(&f.root, &f.bundle, &f.state).is_err());
    }
}

#[cfg(unix)]
pub use platform::{BuiltinFollower, FollowPlan, apply, preview};

#[cfg(not(unix))]
#[derive(Default)]
pub struct BuiltinFollower;
#[cfg(not(unix))]
#[derive(Serialize, Deserialize)]
pub struct FollowPlan {
    unsupported: bool,
}
#[cfg(not(unix))]
pub fn preview(_: &Path, _: &Path, _: &Path) -> Result<FollowPlan> {
    anyhow::bail!("此平台尚未支持锚定目录的内置技能跟随")
}
#[cfg(not(unix))]
pub fn apply(_: &Path, _: &Path, _: &Path, _: &FollowPlan) -> Result<super::LocalSyncReport> {
    anyhow::bail!("此平台尚未支持锚定目录的内置技能跟随")
}
#[cfg(not(unix))]
impl BuiltinFollower {
    pub fn poll(&mut self, _: &Path, _: &Path, state: &Path) -> Result<super::LocalSyncReport> {
        if !state.try_exists()? {
            return Ok(super::LocalSyncReport::default());
        }
        anyhow::bail!("此平台尚未支持锚定目录的内置技能跟随")
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    use crate::package_fs::{Directory, is_not_found};
    use crate::proma::{LocalSyncReport, workspaces};
    use anyhow::{Context, bail};
    use rustix::fs::{AtFlags, FileType};
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::MetadataExt;
    use std::{
        collections::{BTreeMap, BTreeSet},
        io::{Read, Write},
    };

    const STATE: &str = "builtin-follow.json";
    const RECOVERY: &str = ".pleat-builtin-recovery";
    const RECORD: &str = "transaction.json";
    const LIMIT: u64 = 16 * 1024 * 1024;

    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }
    fn hash(value: &impl Serialize) -> Result<String> {
        Ok(digest(&serde_json::to_vec(value)?))
    }
    fn bytes(dir: &Directory, name: &Path) -> Result<Vec<u8>> {
        let mut data = Vec::new();
        dir.read(name)?.take(LIMIT + 1).read_to_end(&mut data)?;
        if data.len() as u64 > LIMIT {
            bail!("文件超过内置跟随大小限制");
        }
        Ok(data)
    }
    fn json<T: serde::de::DeserializeOwned>(dir: &Directory, name: &str) -> Result<T> {
        serde_json::from_slice(&bytes(dir, Path::new(name))?)
            .map_err(|_| anyhow::anyhow!("内置跟随元数据格式无效"))
    }
    fn persist(dir: &Directory, name: &str, value: &impl Serialize) -> Result<()> {
        dir.write(Path::new(name), &serde_json::to_vec_pretty(value)?)?;
        dir.sync()
    }
    fn entries(dir: &Directory) -> Result<Vec<String>> {
        let mut iter = rustix::fs::Dir::read_from(dir)?;
        let mut out = Vec::new();
        while let Some(entry) = iter.read() {
            let entry = entry?;
            let name = entry
                .file_name()
                .to_str()
                .context("非 UTF-8 包名称")?
                .to_owned();
            if !matches!(name.as_str(), "." | "..") {
                out.push(name);
            }
        }
        out.sort();
        Ok(out)
    }
    fn exists(dir: &Directory, name: &str) -> Result<bool> {
        match rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => Ok(true),
            Err(rustix::io::Errno::NOENT) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
    fn optional_dir(path: &Path) -> Result<Option<Directory>> {
        match Directory::open_anchored(path) {
            Ok(dir) => Ok(Some(dir)),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }
    fn protected(name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        matches!(
            name.as_str(),
            "local-data" | ".git" | ".env" | ".kitter-proma.json"
        ) || name.starts_with(".env")
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Entry {
        directory: bool,
        mode: u32,
        hash: String,
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Stamp {
        identity: String,
        root_mode: u32,
        content: String,
        complete: String,
    }
    struct Tree {
        stamp: Stamp,
        nodes: BTreeMap<String, Entry>,
    }

    // Scan via fd only, rejecting private nodes before opening their contents.
    // The ONLY ignored content is regular CPython bytecode in __pycache__.
    fn tree(dir: &Directory) -> Result<Tree> {
        fn scan(
            dir: &Directory,
            prefix: &str,
            nodes: &mut BTreeMap<String, Entry>,
            total: &mut u64,
        ) -> Result<()> {
            for name in entries(dir)? {
                if nodes.len() >= 10000 || prefix.matches('/').count() > 32 {
                    bail!("包结构超限");
                }
                if protected(&name) {
                    bail!("包含私有数据或托管标记，保留整份副本");
                }
                let stat = rustix::fs::statat(dir, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)?;
                let kind = FileType::from_raw_mode(stat.st_mode);
                if name == "__pycache__" && kind != FileType::Directory {
                    bail!("__pycache__ 不是缓存目录，保留整份副本");
                }
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}/{name}")
                };
                let cache = prefix.split('/').any(|p| p == "__pycache__");
                if cache
                    && (kind != FileType::RegularFile
                        || !name.ends_with(".pyc")
                        || !name.contains(".cpython-"))
                {
                    bail!("__pycache__ 含非已知字节码，保留整份副本");
                }
                let node = match kind {
                    FileType::Directory => {
                        let child = dir.child(Path::new(&name))?;
                        if child.identity()? != format!("{}:{}", stat.st_dev, stat.st_ino) {
                            bail!("包目录发生替换");
                        }
                        scan(&child, &path, nodes, total)?;
                        Entry {
                            directory: true,
                            mode: stat.st_mode as u32 & 0o7777,
                            hash: String::new(),
                        }
                    }
                    FileType::RegularFile => {
                        // Hard links can alias an external mutable object; do not adopt them.
                        if stat.st_nlink != 1 {
                            bail!("包中含硬链接，保留整份副本");
                        }
                        let file = dir.read(Path::new(&name))?;
                        let meta = file.metadata()?;
                        if meta.dev() != stat.st_dev as u64 || meta.ino() != stat.st_ino as u64 {
                            bail!("包文件发生替换");
                        }
                        let mut data = Vec::new();
                        file.take(LIMIT + 1).read_to_end(&mut data)?;
                        *total += data.len() as u64;
                        if data.len() as u64 > LIMIT || *total > 256 * 1024 * 1024 {
                            bail!("包内容超限");
                        }
                        if cache
                            && (data.len() < 16
                                || data[2..4] != [13, 10]
                                || u32::from_le_bytes(data[4..8].try_into().unwrap()) > 3)
                        {
                            bail!("__pycache__ 文件不是可识别字节码，保留整份副本");
                        }
                        Entry {
                            directory: false,
                            mode: stat.st_mode as u32 & 0o7777,
                            hash: digest(&data),
                        }
                    }
                    _ => bail!("包含链接或特殊节点，保留整份副本"),
                };
                nodes.insert(path, node);
            }
            Ok(())
        }
        let mut nodes = BTreeMap::new();
        scan(dir, "", &mut nodes, &mut 0)?;
        for (path, node) in &nodes {
            let path = Path::new(path);
            if !node.directory
                && path
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|p| p == "__pycache__")
            {
                let (module, tag) = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.strip_suffix(".pyc"))
                    .and_then(|n| n.split_once(".cpython-"))
                    .context("无法识别字节码名称")?;
                let (version, optimization) = tag.split_once(".opt-").unwrap_or((tag, ""));
                if version.is_empty()
                    || !version.bytes().all(|b| b.is_ascii_digit())
                    || !matches!(optimization, "" | "1" | "2")
                {
                    bail!("无法识别字节码版本");
                }
                let source = path
                    .parent()
                    .and_then(Path::parent)
                    .unwrap_or(Path::new(""))
                    .join(format!("{module}.py"));
                if !source
                    .to_str()
                    .and_then(|p| nodes.get(p))
                    .is_some_and(|n| !n.directory)
                {
                    bail!("字节码缺少对应源码，保留整份副本");
                }
            }
        }
        if !nodes.get("SKILL.md").is_some_and(|n| !n.directory) {
            bail!("缺少实体 SKILL.md");
        }
        let public: BTreeMap<_, _> = nodes
            .iter()
            .filter(|(p, _)| !p.split('/').any(|s| s == "__pycache__"))
            .collect();
        Ok(Tree {
            stamp: Stamp {
                identity: dir.identity()?,
                root_mode: rustix::fs::fstat(dir)?.st_mode as u32 & 0o7777,
                content: hash(&public)?,
                complete: hash(&nodes)?,
            },
            nodes,
        })
    }
    fn stamp(dir: &Directory, name: &str) -> Result<Stamp> {
        Ok(tree(&dir.child(Path::new(name))?)?.stamp)
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Slot {
        path: PathBuf,
        owner: String,
        owner_path: PathBuf,
        owner_identity: String,
        identity: String,
        inactive: bool,
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Environment {
        root_identity: String,
        registry: String,
        owners: BTreeMap<PathBuf, String>,
        slots: Vec<Slot>,
    }
    fn environment(root: &Path) -> Result<Environment> {
        let dir = Directory::open_anchored(root)?;
        let registry = if exists(&dir, "agent-workspaces.json")? {
            let file = dir.read(Path::new("agent-workspaces.json"))?;
            let meta = file.metadata()?;
            hash(&(
                meta.dev(),
                meta.ino(),
                digest(&bytes(&dir, Path::new("agent-workspaces.json"))?),
            ))?
        } else {
            "absent".into()
        };
        let mut env = Environment {
            root_identity: dir.identity()?,
            registry,
            owners: BTreeMap::new(),
            slots: Vec::new(),
        };
        let mut add = |owner: String, base: &Path, names: &[&str]| -> Result<()> {
            let identity = Directory::open_anchored(base)?.identity()?;
            env.owners.insert(base.to_owned(), identity.clone());
            for name in names {
                let path = base.join(name);
                if let Some(slot) = optional_dir(&path)? {
                    env.slots.push(Slot {
                        path,
                        owner: owner.clone(),
                        owner_path: base.to_owned(),
                        owner_identity: identity.clone(),
                        identity: slot.identity()?,
                        inactive: *name == "skills-inactive",
                    });
                }
            }
            Ok(())
        };
        add("default".into(), root, &["default-skills"])?;
        for workspace in workspaces(root)? {
            add(
                format!("workspace:{}", workspace.id),
                &workspace.path,
                &["skills", "skills-inactive"],
            )?;
        }
        dir.verify_location(root)?;
        Ok(env)
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Bundle {
        identity: String,
        packages: BTreeMap<String, Stamp>,
        warnings: Vec<String>,
    }
    fn bundle(path: &Path) -> Result<Bundle> {
        let dir = Directory::open_anchored(path)?;
        let mut result = Bundle {
            identity: dir.identity()?,
            packages: BTreeMap::new(),
            warnings: Vec::new(),
        };
        for name in entries(&dir)? {
            if matches!(name.as_str(), "proma-coach" | "knowledge-maintenance") {
                continue;
            }
            match crate::library::validate_name(&name).and_then(|_| stamp(&dir, &name)) {
                Ok(value) => {
                    result.packages.insert(name, value);
                }
                Err(e) => result.warnings.push(format!("App / {name}：{e}")),
            }
        }
        dir.verify_location(path)?;
        Ok(result)
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Baseline {
        first_approved: String,
        accepted: BTreeSet<String>,
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct State {
        schema: u32,
        root: PathBuf,
        root_identity: String,
        enabled: bool,
        verified_app: BTreeMap<String, BTreeSet<String>>,
        baselines: BTreeMap<String, Baseline>,
    }
    fn baseline_key(slot: &Slot, name: &str) -> Result<String> {
        hash(&(&slot.owner, &slot.owner_path, &slot.owner_identity, name))
    }
    fn read_state(root: &Path, state_dir: &Path, env: &Environment) -> Result<Option<State>> {
        let Some(dir) = optional_dir(state_dir)? else {
            return Ok(None);
        };
        if !exists(&dir, STATE)? {
            return Ok(None);
        }
        let state: State = json(&dir, STATE)?;
        if state.schema != 1 || state.root != root || state.root_identity != env.root_identity {
            bail!("跟随状态不属于当前 Proma 根身份");
        }
        Ok(Some(state))
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct StateLocation {
        parent: String,
        directory: Option<String>,
        state: Option<State>,
    }
    fn state_location(root: &Path, path: &Path, env: &Environment) -> Result<StateLocation> {
        let parent =
            Directory::open_anchored(path.parent().context("状态目录缺少父目录")?)?.identity()?;
        Ok(StateLocation {
            parent,
            directory: optional_dir(path)?.map(|d| d.identity()).transpose()?,
            state: read_state(root, path, env)?,
        })
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    enum Action {
        Adopt,
        Update,
        Conflict,
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Target {
        slot: Slot,
        name: String,
        before: Stamp,
        action: Action,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct FollowPlan {
        schema: u32,
        root: PathBuf,
        bundled: PathBuf,
        state_dir: PathBuf,
        environment: Environment,
        bundle: Bundle,
        state: StateLocation,
        targets: Vec<Target>,
        warnings: Vec<String>,
    }

    /// No directory creation, locks, or writes. Actions are re-derived by apply.
    pub fn preview(root: &Path, bundled: &Path, state_dir: &Path) -> Result<FollowPlan> {
        plan(root, bundled, state_dir, true)
    }
    fn plan(root: &Path, bundled: &Path, state_dir: &Path, explicit: bool) -> Result<FollowPlan> {
        // Keep private follower state outside native packages and the application.
        for path in [root, bundled, state_dir] {
            if !path.is_absolute()
                || path.components().any(|p| {
                    matches!(
                        p,
                        std::path::Component::ParentDir | std::path::Component::CurDir
                    )
                })
            {
                bail!("跟随路径必须是规范绝对路径");
            }
        }
        if state_dir.starts_with(root)
            || state_dir.starts_with(bundled)
            || root.starts_with(state_dir)
            || bundled.starts_with(state_dir)
        {
            bail!("跟随状态必须使用独立目录");
        }
        let env = environment(root)?;
        let source = bundle(bundled)?;
        let location = state_location(root, state_dir, &env)?;
        let mut warnings = source.warnings.clone();
        let mut copies = Vec::new();
        for slot in &env.slots {
            let parent = Directory::open_anchored(&slot.path)?;
            if parent.identity()? != slot.identity {
                bail!("技能父目录发生替换");
            }
            if let Err(error) = no_pending(&parent, &slot.path) {
                warnings.push(format!("{}：{error}", slot.path.display()));
                continue;
            }
            for name in source.packages.keys() {
                if !exists(&parent, name)? {
                    continue;
                }
                match stamp(&parent, name) {
                    Ok(before) => copies.push(Target {
                        slot: slot.clone(),
                        name: name.clone(),
                        before,
                        action: Action::Conflict,
                    }),
                    Err(e) => warnings.push(format!("{} / {name}：{e}", slot.path.display())),
                }
            }
        }
        let templates: BTreeMap<_, _> = copies
            .iter()
            .filter(|t| t.slot.owner == "default")
            .map(|t| (t.name.clone(), t.before.content.clone()))
            .collect();
        let mut counts = BTreeMap::<String, usize>::new();
        for target in &copies {
            *counts
                .entry(baseline_key(&target.slot, &target.name)?)
                .or_default() += 1;
        }
        for target in &mut copies {
            let app = &source.packages[&target.name].content;
            let key = baseline_key(&target.slot, &target.name)?;
            let trusted = location
                .state
                .as_ref()
                .and_then(|s| s.baselines.get(&key))
                .is_some_and(|b| b.accepted.contains(&target.before.content));
            let same = target.before.content == *app;
            let approved_template =
                explicit && templates.get(&target.name) == Some(&target.before.content);
            if counts[&key] == 1 && (same || trusted || approved_template) {
                target.action = if same { Action::Adopt } else { Action::Update };
            } else {
                warnings.push(format!(
                    "{} / {}：未知或已修改的副本（或启停位置重复），保留并等待明确处理",
                    target.slot.path.display(),
                    target.name
                ));
            }
        }
        if environment(root)? != env || bundle(bundled)? != source {
            bail!("来源或登记在预览期间变化，请重新预览");
        }
        Ok(FollowPlan {
            schema: 1,
            root: root.into(),
            bundled: bundled.into(),
            state_dir: state_dir.into(),
            environment: env,
            bundle: source,
            state: location,
            targets: copies,
            warnings,
        })
    }

    #[derive(Default)]
    pub struct BuiltinFollower {
        observed: BTreeMap<(PathBuf, PathBuf, PathBuf), Bundle>,
        notified: BTreeMap<(PathBuf, PathBuf, PathBuf), BTreeMap<String, String>>,
    }
    impl BuiltinFollower {
        pub fn poll(
            &mut self,
            root: &Path,
            bundled: &Path,
            state_dir: &Path,
        ) -> Result<LocalSyncReport> {
            let key = (root.to_owned(), bundled.to_owned(), state_dir.to_owned());
            let Some(dir) = optional_dir(state_dir)? else {
                self.observed.remove(&key);
                self.notified.remove(&key);
                return Ok(LocalSyncReport::default());
            };
            if !exists(&dir, STATE)? {
                self.observed.remove(&key);
                self.notified.remove(&key);
                return Ok(LocalSyncReport::default());
            }
            let _lock = dir.lock(Path::new(".builtin-follow.lock"))?;
            let plan = match plan(root, bundled, state_dir, false) {
                Ok(plan) => plan,
                Err(e) => {
                    self.observed.remove(&key);
                    return Err(e);
                }
            };
            if !plan.state.state.as_ref().is_some_and(|s| s.enabled) {
                self.observed.remove(&key);
                self.notified.remove(&key);
                return Ok(LocalSyncReport::default());
            }
            let stable = self
                .observed
                .insert(key.clone(), plan.bundle.clone())
                .as_ref()
                == Some(&plan.bundle);
            if !stable {
                return Ok(LocalSyncReport {
                    warnings: plan.warnings,
                    ..Default::default()
                });
            }
            dir.verify_location(state_dir)?;
            let mut report = execute(&plan, &dir)?;
            recheck(&plan)?;
            let catalog = plan
                .bundle
                .packages
                .iter()
                .map(|(name, stamp)| (name.clone(), stamp.content.clone()))
                .collect::<BTreeMap<_, _>>();
            report.catalog_changed =
                self.notified.insert(key, catalog.clone()).as_ref() != Some(&catalog);
            Ok(report)
        }
    }

    pub fn apply(
        root: &Path,
        bundled: &Path,
        state_dir: &Path,
        frozen: &FollowPlan,
    ) -> Result<LocalSyncReport> {
        // Never execute caller-provided paths or actions before deriving the full
        // plan again from current registration, App contents and trusted state.
        if &preview(root, bundled, state_dir)? != frozen {
            bail!("冻结计划与当前目录、内容或登记不一致，请重新预览");
        }
        let parent_path = state_dir.parent().context("状态目录缺少父目录")?;
        let parent = Directory::open_anchored(parent_path)?;
        if parent.identity()? != frozen.state.parent {
            bail!("状态父目录已替换");
        }
        let name = Path::new(state_dir.file_name().context("状态目录缺少名称")?);
        let dir = match &frozen.state.directory {
            Some(identity) => {
                let dir = parent.child(name)?;
                if dir.identity()? != *identity {
                    bail!("状态目录已替换");
                }
                dir
            }
            None => {
                let dir = parent.mkdir_exclusive(name)?;
                parent.sync()?;
                dir
            }
        };
        let _lock = dir.lock(Path::new(".builtin-follow.lock"))?;
        let mut current = preview(root, bundled, state_dir)?;
        // Creating our own empty state directory changes only its identity.
        if frozen.state.directory.is_none() && current.state.state.is_none() {
            current.state.directory = None;
        }
        if &current != frozen {
            bail!("冻结计划在取得锁时已变化，请重新预览");
        }
        execute(frozen, &dir)
    }

    fn recheck(plan: &FollowPlan) -> Result<()> {
        if environment(&plan.root)? != plan.environment || bundle(&plan.bundled)? != plan.bundle {
            bail!("App、Proma 根或工作区登记发生变化，停止本批次");
        }
        Ok(())
    }
    fn save_state(plan: &FollowPlan, dir: &Directory, state: &State) -> Result<()> {
        dir.verify_location(&plan.state_dir)?;
        persist(dir, STATE, state)
    }
    fn execute(plan: &FollowPlan, state_dir: &Directory) -> Result<LocalSyncReport> {
        recheck(plan)?;
        let mut state = plan.state.state.clone().unwrap_or(State {
            schema: 1,
            root: plan.root.clone(),
            root_identity: plan.environment.root_identity.clone(),
            enabled: true,
            verified_app: BTreeMap::new(),
            baselines: BTreeMap::new(),
        });
        state.enabled = true;
        if plan.state.state.as_ref() != Some(&state) {
            save_state(plan, state_dir, &state)?;
        }
        let mut report = LocalSyncReport {
            warnings: plan.warnings.clone(),
            ..Default::default()
        };
        for target in &plan.targets {
            if target.action == Action::Conflict {
                continue;
            }
            let key = baseline_key(&target.slot, &target.name)?;
            let app = &plan.bundle.packages[&target.name].content;
            if target.action == Action::Adopt
                && state
                    .baselines
                    .get(&key)
                    .is_some_and(|b| b.accepted.contains(&target.before.content))
                && state
                    .verified_app
                    .get(&target.name)
                    .is_some_and(|hashes| hashes.contains(app))
            {
                // An unchanged observation requires no target lock, repeated
                // App scan, or rewrite of the durable authorization state.
                continue;
            }
            let result = (|| -> Result<bool> {
                recheck(plan)?;
                let owner = Directory::open_anchored(&target.slot.owner_path)?;
                let _lock = owner.lock(Path::new(".kitter-proma.lock"))?;
                let parent = Directory::open_anchored(&target.slot.path)?;
                if owner.identity()? != target.slot.owner_identity
                    || parent.identity()? != target.slot.identity
                    || stamp(&parent, &target.name)? != target.before
                {
                    bail!("副本或父目录已变化，保留现场");
                }
                no_pending(&parent, &target.slot.path)?;
                let key = baseline_key(&target.slot, &target.name)?;
                // This record is the durable first explicit authorization (or an
                // independently verified exact App match in automatic mode).
                let baseline = state
                    .baselines
                    .entry(key.clone())
                    .or_insert_with(|| Baseline {
                        first_approved: target.before.content.clone(),
                        accepted: BTreeSet::new(),
                    });
                baseline.accepted.insert(target.before.content.clone());
                let app = plan.bundle.packages[&target.name].content.clone();
                state
                    .verified_app
                    .entry(target.name.clone())
                    .or_default()
                    .insert(app.clone());
                save_state(plan, state_dir, &state)?;
                if target.action == Action::Adopt {
                    return Ok(false);
                }
                replace(plan, target, &parent, || {
                    state
                        .baselines
                        .get_mut(&key)
                        .context("基线缺失")?
                        .accepted
                        .insert(app.clone());
                    save_state(plan, state_dir, &state)
                })?;
                Ok(true)
            })();
            match result {
                Ok(true) => {
                    report.updated += 1;
                    report.inactive_updated += usize::from(target.slot.inactive);
                }
                Ok(false) => {}
                Err(e) => report.warnings.push(format!(
                    "{} / {}：{e}",
                    target.slot.path.display(),
                    target.name
                )),
            }
        }
        report.warnings.sort();
        report.warnings.dedup();
        Ok(report)
    }

    #[derive(Serialize, Deserialize)]
    struct Transaction {
        root_identity: String,
        parent_identity: String,
        target: String,
        before: Stamp,
        after: Option<Stamp>,
        phase: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        restored: Option<Stamp>,
    }
    fn no_pending(parent: &Directory, path: &Path) -> Result<()> {
        if !exists(parent, RECOVERY)? {
            return Ok(());
        }
        let recovery = parent.child(Path::new(RECOVERY))?;
        for name in entries(&recovery)? {
            let record = recovery
                .child(Path::new(&name))
                .and_then(|op| json::<Transaction>(&op, RECORD));
            if !record.is_ok_and(|r| matches!(r.phase.as_str(), "committed" | "rolled_back")) {
                bail!(
                    "未完成恢复记录阻断该目录更新；恢复证据：{}",
                    path.join(RECOVERY).join(name).display()
                );
            }
        }
        Ok(())
    }
    fn copy(source: &Directory, output: &Directory, expected: &Stamp) -> Result<()> {
        let original = tree(source)?;
        if original.stamp != *expected {
            bail!("App 包已变化");
        }
        for (path, node) in &original.nodes {
            if path.split('/').any(|s| s == "__pycache__") {
                continue;
            }
            if node.directory {
                output.mkdir(Path::new(path))?;
            } else {
                let data = bytes(source, Path::new(path))?;
                if digest(&data) != node.hash {
                    bail!("App 文件仍在变化");
                }
                let mut file = output.create_file(Path::new(path), node.mode)?;
                file.write_all(&data)?;
                file.sync_all()?;
            }
        }
        // Children are complete before permissions and directory entries flush.
        for (path, node) in original.nodes.iter().rev() {
            if node.directory && !path.split('/').any(|s| s == "__pycache__") {
                output.chmod_directory(Path::new(path), node.mode)?;
                output.child(Path::new(path))?.sync()?;
            }
        }
        output.sync()?;
        if tree(output)?.stamp.content != expected.content || tree(source)?.stamp != *expected {
            bail!("App 包复制校验失败");
        }
        Ok(())
    }

    #[cfg(test)]
    thread_local! {
        pub(super) static AFTER_BACKUP: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
        pub(super) static BEFORE_PUBLISH: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
        pub(super) static AFTER_PUBLISH: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
        pub(super) static BEFORE_ROLLBACK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
    }
    fn replace(
        plan: &FollowPlan,
        target: &Target,
        parent: &Directory,
        committed: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        parent.mkdir(Path::new(RECOVERY))?;
        parent.sync()?;
        let recovery = parent.child(Path::new(RECOVERY))?;
        let recovery_path = target.slot.path.join(RECOVERY);
        recovery.verify_location(&recovery_path)?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let name = format!("follow-{}-{nonce}", std::process::id());
        let output = recovery.mkdir_exclusive(Path::new(&name))?;
        recovery.sync()?;
        let operation = recovery_path.join(name);
        let mut transaction = Transaction {
            root_identity: plan.environment.root_identity.clone(),
            parent_identity: target.slot.identity.clone(),
            target: target.name.clone(),
            before: target.before.clone(),
            after: None,
            phase: "preparing".into(),
            restored: None,
        };
        persist(&output, RECORD, &transaction)?;
        let result = (|| -> Result<()> {
            let staging = output.mkdir_exclusive(Path::new("staging"))?;
            let source = Directory::open_anchored(&plan.bundled)?.child(Path::new(&target.name))?;
            copy(&source, &staging, &plan.bundle.packages[&target.name])?;
            rustix::fs::fchmod(
                &staging,
                rustix::fs::Mode::from_raw_mode(
                    target
                        .before
                        .root_mode
                        .try_into()
                        .context("目录权限超出平台范围")?,
                ),
            )?;
            staging.sync()?;
            transaction.after = Some(tree(&staging)?.stamp);
            transaction.phase = "prepared".into();
            persist(&output, RECORD, &transaction)?;
            recheck(plan)?;
            parent.verify_location(&target.slot.path)?;
            output.verify_location(&operation)?;
            if stamp(parent, &target.name)? != target.before {
                bail!("旧副本在替换前已变化");
            }
            parent.rename_to(Path::new(&target.name), &output, Path::new("previous"))?;
            parent.sync()?;
            output.sync()?;
            if !stamp(&output, "previous").is_ok_and(|actual| actual == target.before) {
                // A raced object is restored without overwriting any new occupant.
                output.rename_to(Path::new("previous"), parent, Path::new(&target.name))?;
                parent.sync()?;
                output.sync()?;
                bail!("原件发生并发修改，已归还原位置");
            }
            transaction.phase = "replacing".into();
            persist(&output, RECORD, &transaction)?;
            #[cfg(test)]
            AFTER_BACKUP.with(|h| {
                if let Some(f) = h.borrow_mut().take() {
                    f();
                }
            });
            recheck(plan)?;
            parent.verify_location(&target.slot.path)?;
            output.verify_location(&operation)?;
            #[cfg(test)]
            BEFORE_PUBLISH.with(|h| {
                if let Some(f) = h.borrow_mut().take() {
                    f();
                }
            });
            staging.verify_location(&operation.join("staging"))?;
            if Some(stamp(&output, "staging")?) != transaction.after {
                bail!("暂存包在发布前已变化，未发布");
            }
            if stamp(&output, "previous")? != transaction.before {
                bail!("原件在备份后已变化，保留用户修改并回退");
            }
            output.rename_to(Path::new("staging"), parent, Path::new(&target.name))?;
            parent.sync()?;
            output.sync()?;
            #[cfg(test)]
            AFTER_PUBLISH.with(|h| {
                if let Some(f) = h.borrow_mut().take() {
                    f();
                }
            });
            parent.verify_location(&target.slot.path)?;
            if Some(stamp(parent, &target.name)?) != transaction.after {
                bail!("发布对象发生变化");
            }
            recheck(plan)?;
            committed()?;
            if stamp(&output, "previous")? != transaction.before {
                bail!("原件在提交前已变化，保留用户修改并回退");
            }
            transaction.phase = "committed".into();
            persist(&output, RECORD, &transaction)
        })();
        if let Err(error) = result {
            let rollback = rollback(&transaction, parent, &output, &target.slot.path, &operation);
            if rollback.is_ok() {
                transaction.phase = "rolled_back".into();
                transaction.restored = stamp(parent, &target.name).ok();
                persist(&output, RECORD, &transaction)?;
            }
            let status = rollback
                .err()
                .map(|e| format!("回退未完成：{e}"))
                .unwrap_or_else(|| "已回退".into());
            bail!("{error}；{status}；恢复证据：{}", operation.display());
        }
        Ok(())
    }
    fn rollback(
        t: &Transaction,
        parent: &Directory,
        output: &Directory,
        parent_path: &Path,
        operation: &Path,
    ) -> Result<()> {
        parent.verify_location(parent_path)?;
        output.verify_location(operation)?;
        if parent.identity()? != t.parent_identity {
            bail!("恢复父目录身份变化");
        }
        if directory_identity_matches(parent, &t.target, &t.before.identity) {
            return Ok(());
        }
        if !exists(output, "previous")? {
            // Preparation failed before any rename; leave the user's current object.
            if matches!(t.phase.as_str(), "preparing" | "prepared") {
                return Ok(());
            }
            bail!("恢复原件缺失");
        }
        if exists(parent, &t.target)? {
            let published = t.after.as_ref().context("缺少本次发布对象身份")?;
            if !directory_identity_matches(parent, &t.target, &published.identity) {
                bail!("目标已有并发对象，未覆盖");
            }
            #[cfg(test)]
            BEFORE_ROLLBACK.with(|h| {
                if let Some(f) = h.borrow_mut().take() {
                    f();
                }
            });
            parent.rename_to(Path::new(&t.target), output, Path::new("failed"))?;
            parent.sync()?;
            output.sync()?;
            if !directory_identity_matches(output, "failed", &published.identity) {
                output.rename_to(Path::new("failed"), parent, Path::new(&t.target))?;
                parent.sync()?;
                output.sync()?;
                bail!("回退时出现并发对象，已归还原位置");
            }
        }
        // An open editor can change the original after it was moved. Return
        // that same directory with its edits; never substitute another inode.
        if !directory_identity_matches(output, "previous", &t.before.identity) {
            bail!("恢复原件身份变化，保留现场");
        }
        output.rename_to(Path::new("previous"), parent, Path::new(&t.target))?;
        parent.sync()?;
        output.sync()?;
        if !directory_identity_matches(parent, &t.target, &t.before.identity) {
            bail!("回退后原件身份变化，保留现场");
        }
        Ok(())
    }

    fn directory_identity_matches(parent: &Directory, name: &str, identity: &str) -> bool {
        parent
            .child(Path::new(name))
            .and_then(|dir| dir.identity())
            .is_ok_and(|current| current == identity)
    }
}
