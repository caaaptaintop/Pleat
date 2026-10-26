//! Grok's native user/project rule directory. Only the installed-path pointer
//! is published; local adaptation text remains in the original skill source.
use std::path::{Path, PathBuf};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use super::{ManagedFileRule, RuleKind, RulePlan};

pub(super) fn plan(config: &Path, installation: &Path) -> Result<ManagedFileRule> {
    let (config, installation) = super::normalize_file_rule_paths(config, installation)?;
    let installed = installation.to_str().context("Grok 安装路径编码无效")?;
    ensure!(installation.is_absolute() && !installed.chars().any(char::is_control),
        "Grok 原生规则需要无控制字符的绝对安装路径");
    let name = installation.file_name().and_then(|name| name.to_str()).context("技能名称编码无效")?;
    crate::library::validate_name(name)?;
    let adaptation = installation.join("local-data/ADAPTATION.md");
    let quoted_installation = serde_json::to_string(installed)?;
    let quoted_adaptation = serde_json::to_string(adaptation.to_str().context("本机适配路径编码无效")?)?;
    let digest = format!("{:x}", Sha256::digest(installed.as_bytes()));
    let logical = config.join("rules").join(format!("pleat-grok-adaptation-{digest}.md"));
    let path = crate::cross_project::physical_entry(&logical)?;
    let body = format!(
        "<!-- Pleat managed Grok adaptation v1 -->\n\
        When this request actually invokes the installed skill from {quoted_installation}, or its \
        resolved equivalent, use the native file reading tool to read its optional local adaptation \
        at {quoted_adaptation} before following the skill, and apply relevant instructions. \
        A same-named skill from another source does not match. Resolve upstream relative references \
        against the skill directory, and local adaptation relative references against the directory \
        containing ADAPTATION.md. Read each resolved adaptation at most once per request. If the skill \
        was not invoked or the optional file does not exist, continue without reading it. Respect \
        native permission failures and approval requirements. Do not change trust, permissions or \
        security settings, scan other skills or user directories, or run commands to obtain it.\n"
    ).into_bytes();
    ManagedFileRule::new(logical, path, body, 16 * 1024)
}

const MAX_HOOK: u64 = 64 * 1024;
const HOOK_CONFLICT: &str = "Grok Hook 归属或内容冲突，已保留";

fn absolute_path(path: &Path) -> Result<&str> {
    let text = path.to_str().context(HOOK_CONFLICT)?;
    ensure!(path.is_absolute() && !text.chars().any(char::is_control)
        && !path.components().any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::CurDir)), HOOK_CONFLICT);
    Ok(text)
}

fn source_id(value: &str) -> bool {
    let parts = value.split(':').collect::<Vec<_>>();
    parts.len() == 2 && parts.iter().all(|part|
        part.parse::<u64>().is_ok_and(|number| number.to_string() == *part))
}

fn shell_quote(value: &str) -> String {
    // Grok also expands $VAR/${VAR} before spawning the shell. Adjacent
    // single-quoted segments keep each dollar literal at both layers.
    format!("'{}'", value.replace('\'', "'\"'\"'").replace('$', "$''"))
}

fn hook_body(installation: &Path, identity: &str, reader: &Path) -> Result<Vec<u8>> {
    let installed = absolute_path(installation)?;
    let reader = absolute_path(reader)?;
    ensure!(source_id(identity), HOOK_CONFLICT);
    let command = format!("{} --pleat-native-grok-adaptation-v1 --installation {} --source-identity {}",
        shell_quote(reader), shell_quote(installed), shell_quote(identity));
    // Exactly these four nonsecret names; no user environment/config capture.
    let env = std::collections::BTreeMap::from([
        ("PLEAT_GROK_BINDING_VERSION", "1"),
        ("PLEAT_GROK_INSTALLATION", installed),
        ("PLEAT_GROK_SOURCE_ID", identity),
        ("PLEAT_GROK_READER", reader),
    ]);
    let value = serde_json::json!({"hooks":{"PostToolUse":[{"matcher":"read_file","hooks":[{
        "type":"command", "command":command, "timeout":5, "env":env
    }]}]}});
    Ok(format!("{}\n", super::canonical_json(value)?).into_bytes())
}

fn hook_paths(config: &Path, installation: &Path) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let (config, installation) = super::normalize_file_rule_paths(config, installation)?;
    let installed = absolute_path(&installation)?;
    let name = installation.file_name().and_then(|name| name.to_str()).context(HOOK_CONFLICT)?;
    crate::library::validate_name(name).map_err(|_| anyhow::anyhow!(HOOK_CONFLICT))?;
    let digest = format!("{:x}", Sha256::digest(installed.as_bytes()));
    let logical = config.join("hooks").join(format!("pleat-grok-reader-{digest}.json"));
    let path = crate::cross_project::physical_entry(&logical)?;
    Ok((logical, path, installation))
}

// Parsing is only a bounded hint for regenerating our own exact byte template.
// Never return a serde error: an untrusted command/env may contain secrets.
fn owned_hook(bytes: &[u8], installation: &Path) -> Result<(String, PathBuf)> {
    let value = serde_json::from_slice::<serde_json::Value>(bytes)
        .map_err(|_| anyhow::anyhow!(HOOK_CONFLICT))?;
    let env = value.pointer("/hooks/PostToolUse/0/hooks/0/env").context(HOOK_CONFLICT)?;
    let identity = env.get("PLEAT_GROK_SOURCE_ID").and_then(|v| v.as_str()).context(HOOK_CONFLICT)?;
    let reader = PathBuf::from(env.get("PLEAT_GROK_READER").and_then(|v| v.as_str()).context(HOOK_CONFLICT)?);
    ensure!(bytes == hook_body(installation, identity, &reader)?.as_slice(), HOOK_CONFLICT);
    Ok((identity.to_owned(), reader))
}

fn file_plan(file: ManagedFileRule) -> RulePlan {
    RulePlan { path: file.path.clone(), kind: RuleKind::File(file) }
}

pub(super) fn hook_install(config: &Path, installation: &Path, source: &Path) -> Result<Vec<RulePlan>> {
    // The existing FD-bound reader is Unix-only. Retain all legacy/link plans
    // elsewhere instead of registering a hook which can never read safely.
    #[cfg(not(unix))]
    { let _ = (config, installation, source); return Ok(Vec::new()); }
    #[cfg(unix)]
    {
        let (logical, path, installation) = hook_paths(config, installation)?;
        let source = source.canonicalize().context("Grok Hook 技能源不存在")?;
        ensure!(source.is_dir() && source.join("SKILL.md").is_file(), "Grok Hook 技能源无效");
        let identity = crate::library::filesystem_identity(&source)?;
        let reader = std::env::current_exe()?.canonicalize()?;
        let body = hook_body(&installation, &identity, &reader)?;
        let previous = super::snapshot(&path, MAX_HOOK)?;
        let mut file = if let Some((bytes, _)) = &previous {
            let (old_identity, _) = owned_hook(bytes, &installation)?;
            ensure!(old_identity == identity, HOOK_CONFLICT);
            // Build with the old template first, preserving the original CAS.
            let mut file = ManagedFileRule::new(logical, path, bytes.clone(), MAX_HOOK)?;
            ensure!(file.expected == previous, HOOK_CONFLICT);
            file.body = body;
            file
        } else {
            ManagedFileRule::new(logical, path, body, MAX_HOOK)?
        };
        if installation.symlink_metadata().is_ok() {
            ensure!(crate::directory_link::inspect(&installation)?.is_some()
                && installation.canonicalize().ok().as_ref() == Some(&source),
                "安装位置已被占用：{}", installation.display());
            file.installation_guard = Some((installation.clone(), Some(crate::library::filesystem_identity(&installation)?)));
        }
        file.source_installation = Some(installation);
        file.source_guard = Some((source, identity));
        Ok(vec![file_plan(file)])
    }
}

pub(super) fn hook_removals(config: &Path, installation: &Path) -> Result<Vec<RulePlan>> {
    let (logical, path, installation) = hook_paths(config, installation)?;
    let Some(previous) = super::snapshot(&path, MAX_HOOK)? else { return Ok(Vec::new()); };
    let (identity, _) = owned_hook(&previous.0, &installation)?;
    // A disappeared source is not required for ownership proof. If the slot
    // resolves, however, a different source inode must never be adopted.
    match installation.canonicalize() {
        Ok(source) => ensure!(source.is_dir() && crate::library::filesystem_identity(&source)? == identity, HOOK_CONFLICT),
        Err(_) => {},
    }
    let mut file = ManagedFileRule::new(logical, path, previous.0.clone(), MAX_HOOK)?;
    ensure!(file.expected.as_ref() == Some(&previous), HOOK_CONFLICT);
    let expected = match installation.symlink_metadata() {
        Ok(_) => {
            ensure!(crate::directory_link::inspect(&installation)?.is_some(), HOOK_CONFLICT);
            Some(crate::library::filesystem_identity(&installation)?)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => anyhow::bail!(HOOK_CONFLICT),
    };
    file.installation_guard = Some((installation.clone(), expected));
    file.removal_source_guard = Some((installation, identity));
    Ok(vec![file_plan(file)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn project_and_custom_user_rules_keep_source_and_unmanaged_files() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(source.join("local-data")).unwrap();
        fs::write(source.join("SKILL.md"), "source body").unwrap();
        fs::write(source.join("local-data/ADAPTATION.md"), "private adaptation canary").unwrap();
        for relative in ["project/.grok", "custom-user-grok"] {
            let config = temp.path().join(relative);
            fs::create_dir_all(config.join("rules")).unwrap();
            fs::write(config.join("rules/personal.md"), "personal rule").unwrap();
            fs::write(config.join("AGENTS.md"), "personal instructions").unwrap();
            let installation = config.join("skills/demo");
            let mut first = plan(&config, &installation).unwrap();
            assert!(first.install().unwrap());
            let body = fs::read_to_string(&first.path).unwrap();
            assert!(body.contains(installation.to_str().unwrap()));
            assert!(!body.contains("private adaptation canary"));
            let mut repeated = plan(&config, &installation).unwrap();
            assert!(!repeated.install().unwrap());
            fs::write(source.join("local-data/ADAPTATION.md"), "updated canary").unwrap();
            assert!(repeated.detach().unwrap());
            assert_eq!(fs::read_to_string(config.join("rules/personal.md")).unwrap(), "personal rule");
            assert_eq!(fs::read_to_string(config.join("AGENTS.md")).unwrap(), "personal instructions");
            assert_eq!(fs::read_to_string(source.join("local-data/ADAPTATION.md")).unwrap(), "updated canary");
            assert_eq!(fs::read_to_string(source.join("SKILL.md")).unwrap(), "source body");
        }
    }

    #[test]
    fn same_name_source_identity_and_conflict_protection_are_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join(".grok");
        fs::create_dir_all(&config).unwrap();
        let a = config.join("skills/demo");
        let b = temp.path().join(".agents/skills/demo");
        let mut first = plan(&config, &a).unwrap();
        let second = plan(&config, &b).unwrap();
        assert_ne!(first.path, second.path);
        assert_ne!(first.body, second.body);
        first.install().unwrap();
        fs::write(&first.path, "user changed this rule").unwrap();
        assert!(first.detach().is_err());
        assert!(plan(&config, &a).is_err());
        assert_eq!(fs::read_to_string(&first.path).unwrap(), "user changed this rule");
        assert!(plan(&config, Path::new("relative/skills/demo")).is_err());
        assert!(plan(&config, &config.join("skills/demo\nrule")).is_err());
    }
    #[cfg(unix)]
    struct HookFixture {
        _temp: tempfile::TempDir, root: PathBuf, config: PathBuf, source: PathBuf, installation: PathBuf,
    }
    #[cfg(unix)]
    impl HookFixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let config = root.join("project/.grok");
            let source = root.join("library/demo");
            let installation = config.join("skills/demo");
            fs::create_dir_all(source.join("local-data")).unwrap();
            fs::create_dir_all(installation.parent().unwrap()).unwrap();
            fs::write(source.join("SKILL.md"), "SOURCE_A").unwrap();
            fs::write(source.join("local-data/ADAPTATION.md"), "LOCAL_CANARY").unwrap();
            std::os::unix::fs::symlink(&source, &installation).unwrap();
            Self { _temp: temp, root, config, source, installation }
        }
        fn install_plan(&self) -> RulePlan {
            hook_install(&self.config, &self.installation, &self.source).unwrap().remove(0)
        }
        fn old_reader(&self) -> (PathBuf, Vec<u8>) {
            let plan = self.install_plan();
            let identity = crate::library::filesystem_identity(&self.source).unwrap();
            let bytes = hook_body(&self.installation, &identity, &self.root.join("retired CLI's binary")).unwrap();
            fs::create_dir_all(plan.path.parent().unwrap()).unwrap();
            fs::write(&plan.path, &bytes).unwrap();
            (plan.path, bytes)
        }
    }

    #[cfg(unix)]
    #[test]
    fn grok_hook_lifecycle_has_exact_schema_and_no_source_body() {
        let f = HookFixture::new();
        for installation in [&f.installation, &f.root.join("project/.agents/skills/demo")] {
            if installation.symlink_metadata().is_err() {
                fs::create_dir_all(installation.parent().unwrap()).unwrap();
                std::os::unix::fs::symlink(&f.source, installation).unwrap();
            }
            let mut hook = hook_install(&f.config, installation, &f.source).unwrap().remove(0);
            assert!(hook.needs_install());
            assert!(hook.install().unwrap());
            let bytes = fs::read(&hook.path).unwrap();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["hooks"]["PostToolUse"][0]["matcher"], "read_file");
            let handler = &value["hooks"]["PostToolUse"][0]["hooks"][0];
            assert_eq!(handler["timeout"], 5);
            assert_eq!(handler["env"].as_object().unwrap().len(), 4);
            let command = handler["command"].as_str().unwrap();
            assert!(command.contains("--pleat-native-grok-adaptation-v1 --installation "));
            assert!(command.contains(" --source-identity "));
            assert!(!command.contains(f.source.to_str().unwrap()));
            assert!(!String::from_utf8(bytes.clone()).unwrap().contains("LOCAL_CANARY"));
            let mut repeated = hook_install(&f.config, installation, &f.source).unwrap().remove(0);
            assert!(!repeated.needs_install());
            assert!(!repeated.install().unwrap());
            let digest = format!("{:x}", Sha256::digest(installation.to_str().unwrap().as_bytes()));
            assert_eq!(hook.path.file_name().unwrap(), format!("pleat-grok-reader-{digest}.json").as_str());
            let mut removal = hook_removals(&f.config, installation).unwrap().remove(0);
            assert!(removal.detach().unwrap());
            assert!(!hook.path.exists());
        }
        assert_eq!(fs::read(f.source.join("local-data/ADAPTATION.md")).unwrap(), b"LOCAL_CANARY");
    }

    #[cfg(unix)]
    #[test]
    fn grok_hook_update_and_inode_preserving_move_keep_registration() {
        let f = HookFixture::new();
        let mut hook = f.install_plan(); hook.install().unwrap();
        let before = super::super::snapshot(&hook.path, MAX_HOOK).unwrap();
        fs::write(f.source.join("SKILL.md"), "SOURCE_B").unwrap();
        let moved = f.root.join("moved-library");
        fs::rename(f.source.parent().unwrap(), &moved).unwrap();
        std::os::unix::fs::symlink(&moved, f.source.parent().unwrap()).unwrap();
        let mut repeated = f.install_plan();
        assert!(!repeated.install().unwrap());
        assert_eq!(super::super::snapshot(&hook.path, MAX_HOOK).unwrap(), before);
        assert_eq!(fs::read(f.installation.join("SKILL.md")).unwrap(), b"SOURCE_B");
        hook_removals(&f.config, &f.installation).unwrap()[0].detach().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn grok_hook_old_reader_upgrade_and_rollback_reuse_file_transaction() {
        let f = HookFixture::new(); let (path, old) = f.old_reader();
        let mut hook = f.install_plan();
        assert!(hook.needs_install());
        assert!(hook.install().unwrap());
        assert!(fs::read(&path).unwrap() != old);
        assert!(hook.rollback_install().unwrap());
        assert!(fs::read(&path).unwrap() == old);
        assert!(hook.install().unwrap());
        assert!(!hook.needs_install());
    }

    #[cfg(unix)]
    #[test]
    fn grok_hook_upgrade_publish_failure_restores_old_bytes() {
        let f = HookFixture::new(); let (path, old) = f.old_reader();
        let mut hook = f.install_plan();
        super::super::FAIL_AFTER_PUBLISH.with(|flag| flag.set(true));
        assert!(hook.install().is_err());
        assert!(fs::read(&path).unwrap() == old);
        assert!(hook.install().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn grok_hook_upgrade_detach_sync_failure_restores_old_bytes_and_retries() {
        let f = HookFixture::new(); let (path, old) = f.old_reader();
        let source_id = crate::library::filesystem_identity(&f.source).unwrap();
        let slot_id = crate::library::filesystem_identity(&f.installation).unwrap();
        let mut hook = f.install_plan();
        super::super::BEFORE_DETACH_SYNC.with(|callback| *callback.borrow_mut() = Some(Box::new(|| {
            anyhow::bail!("injected detach durability failure")
        })));
        let error = hook.install().unwrap_err();
        assert!(format!("{error:#}").contains("已恢复预览前状态"));
        assert!(fs::read(&path).unwrap() == old);
        assert_eq!(crate::library::filesystem_identity(&f.source).unwrap(), source_id);
        assert_eq!(crate::library::filesystem_identity(&f.installation).unwrap(), slot_id);
        assert!(hook.install().unwrap());
        assert!(fs::read(&path).unwrap() != old);
    }

    #[cfg(unix)]
    #[test]
    fn grok_hook_upgrade_detach_sync_failure_preserves_concurrent_user_entry() {
        let f = HookFixture::new(); let (path, old) = f.old_reader();
        let mut hook = f.install_plan(); let replaced = path.clone();
        super::super::BEFORE_DETACH_SYNC.with(|callback| *callback.borrow_mut() = Some(Box::new(move || {
            fs::write(replaced, b"USER_HOOK_AFTER_DETACH").unwrap();
            anyhow::bail!("injected detach durability failure")
        })));
        let error = hook.install().unwrap_err();
        assert!(format!("{error:#}").contains("恢复受阻，已保留恢复记录"));
        assert!(fs::read(&path).unwrap() == b"USER_HOOK_AFTER_DETACH");
        assert!(fs::read_dir(&f.config).unwrap().filter_map(Result::ok).any(|entry| {
            entry.file_name().to_string_lossy().starts_with(".pleat-removed-rule-")
                && fs::read(entry.path().join("instruction.md")).is_ok_and(|bytes| bytes == old)
        }));
        assert_eq!(fs::read(f.source.join("SKILL.md")).unwrap(), b"SOURCE_A");
    }

    #[cfg(unix)]
    #[test]
    fn grok_hook_upgrade_rejects_inode_cas_replacement_and_user_edits() {
        let f = HookFixture::new(); let (path, old) = f.old_reader();
        let mut hook = f.install_plan();
        fs::rename(&path, f.root.join("old-hook")).unwrap();
        fs::write(&path, &old).unwrap();
        assert!(hook.install().is_err());
        fs::write(&path, "{invalid-secret-canary").unwrap();
        let error = hook_install(&f.config, &f.installation, &f.source).unwrap_err();
        assert_eq!(format!("{error:#}"), HOOK_CONFLICT);
        assert!(fs::read(&path).unwrap() == b"{invalid-secret-canary");
    }

    #[cfg(unix)]
    #[test]
    fn grok_hook_extra_env_foreign_json_and_symlink_are_preserved() {
        let f = HookFixture::new(); let (path, old) = f.old_reader();
        let mut value: serde_json::Value = serde_json::from_slice(&old).unwrap();
        value["hooks"]["PostToolUse"][0]["hooks"][0]["env"]["UNOWNED"] = "secret-canary".into();
        let extra = format!("{}\n", super::super::canonical_json(value).unwrap()).into_bytes();
        for bytes in [extra, b"{}\n".to_vec(), [old.as_slice(), b" "].concat()] {
            fs::write(&path, &bytes).unwrap();
            assert_eq!(format!("{:#}", hook_removals(&f.config, &f.installation).unwrap_err()), HOOK_CONFLICT);
            assert!(hook_install(&f.config, &f.installation, &f.source).is_err());
            assert!(fs::read(&path).unwrap() == bytes);
        }
        fs::rename(&path, f.root.join("foreign-hook")).unwrap();
        std::os::unix::fs::symlink(f.root.join("foreign-hook"), &path).unwrap();
        assert!(hook_removals(&f.config, &f.installation).is_err());
        assert!(path.symlink_metadata().unwrap().file_type().is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn grok_hook_alias_source_binds_directory_identity_and_slot_replacement_is_preserved() {
        let f = HookFixture::new();
        let alias = f.root.join(".shared-source");
        std::os::unix::fs::symlink(&f.source, &alias).unwrap();
        let mut hook = hook_install(&f.config, &f.installation, &alias).unwrap().remove(0);
        let RuleKind::File(file) = &hook.kind else { panic!("file plan required"); };
        assert_eq!(file.source_guard.as_ref().unwrap().0, f.source);
        assert_eq!(file.source_guard.as_ref().unwrap().1, crate::library::filesystem_identity(&f.source).unwrap());
        assert_ne!(file.source_guard.as_ref().unwrap().1, crate::library::filesystem_identity(&alias).unwrap());
        hook.install().unwrap();
        let mut removal = hook_removals(&f.config, &f.installation).unwrap().remove(0);
        fs::rename(&f.installation, f.root.join("retained-slot")).unwrap();
        std::os::unix::fs::symlink(&f.source, &f.installation).unwrap();
        assert!(removal.detach().is_err());
        assert!(hook.path.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn grok_hook_source_guard_rejects_replacement_before_publish() {
        let f = HookFixture::new(); let mut hook = f.install_plan();
        fs::rename(&f.source, f.root.join("retained-source")).unwrap();
        fs::create_dir(&f.source).unwrap();
        fs::write(f.source.join("SKILL.md"), "SOURCE_A").unwrap();
        assert!(hook.install().is_err()); assert!(!hook.path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn grok_hook_removal_uses_exact_old_template_without_live_source_or_reader() {
        let f = HookFixture::new(); let (path, _) = f.old_reader();
        fs::rename(&f.source, f.root.join("retained-source")).unwrap();
        let mut hook = hook_removals(&f.config, &f.installation).unwrap().remove(0);
        assert!(hook.detach().unwrap()); assert!(!path.exists());
        assert!(f.root.join("retained-source/SKILL.md").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn grok_hook_changed_source_or_slot_is_not_adopted_for_removal() {
        let f = HookFixture::new(); let (path, _) = f.old_reader();
        let mut removal = hook_removals(&f.config, &f.installation).unwrap().remove(0);
        fs::rename(&f.source, f.root.join("retained-source")).unwrap();
        fs::create_dir(&f.source).unwrap();
        fs::write(f.source.join("SKILL.md"), "SOURCE_A").unwrap();
        assert!(removal.detach().is_err());
        assert!(hook_removals(&f.config, &f.installation).is_err());
        assert!(hook_install(&f.config, &f.installation, &f.source).is_err());
        assert!(path.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn grok_hook_parent_replacement_preserves_user_directory() {
        let f = HookFixture::new(); let (path, _) = f.old_reader();
        let mut hook = f.install_plan();
        let parent = path.parent().unwrap();
        fs::rename(parent, f.root.join("retained-hooks")).unwrap();
        fs::create_dir(parent).unwrap();
        assert!(hook.install().is_err()); assert_eq!(fs::read_dir(parent).unwrap().count(), 0);
        assert_eq!(fs::read_dir(f.root.join("retained-hooks")).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn grok_hook_fixed_arguments_quote_shell_metacharacters_and_validate_metadata() {
        let reader = Path::new("/tmp/CLI 'quoted';$(id)");
        let installation = Path::new("/tmp/skills/a 'quoted';$(id)");
        let bytes = hook_body(installation, "12:34", reader).unwrap();
        assert!(owned_hook(&bytes, installation).is_ok());
        assert!(owned_hook(&bytes, Path::new("/tmp/other")).is_err());
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
        assert_eq!(shell_quote("$HOME/${PATH}"), "'$''HOME/$''{PATH}'");
        for identity in ["1:2:3", "01:2", "-1:2", "1:2\n", "1:$(id)"] {
            assert!(hook_body(installation, identity, reader).is_err());
        }
        assert!(hook_body(installation, "1:2", Path::new("relative")).is_err());
        assert!(hook_body(installation, "1:2", Path::new("/tmp/bad\nreader")).is_err());
    }

}
