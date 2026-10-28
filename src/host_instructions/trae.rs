//! TRAE CN's supported project rules and independent global user rules.
//! Rule publication and recovery use the common managed file transaction.
use std::path::Path;
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use super::ManagedFileRule;

pub(super) fn plan(config: &Path, installation: &Path) -> Result<ManagedFileRule> {
    let rules = match config.file_name().and_then(|name| name.to_str()) {
        Some(".trae") => "rules",
        Some(".trae-cn") => "user_rules",
        _ => anyhow::bail!("无法确认 Trae 原生规则目录，已保留"),
    };
    let (config, installation) = super::normalize_file_rule_paths(config, installation)?;
    let installed = installation.to_str().context("Trae 安装路径编码无效")?;
    ensure!(installation.is_absolute() && !installed.chars().any(char::is_control),
        "Trae 原生规则需要无控制字符的绝对安装路径");
    let name = installation.file_name().and_then(|name| name.to_str()).context("技能名称编码无效")?;
    crate::library::validate_name(name)?;
    let quoted_name = serde_json::to_string(name)?;
    let quoted_installation = serde_json::to_string(installed)?;
    let adaptation = installation.join("local-data/ADAPTATION.md");
    let quoted_adaptation = serde_json::to_string(adaptation.to_str().context("本机适配路径编码无效")?)?;
    // Two installations with the same name can belong to distinct sources.
    let digest = format!("{:x}", Sha256::digest(installed.as_bytes()));
    let logical = config.join(rules).join(format!("pleat-adaptation-{}.md", &digest[..24]));
    let path = crate::cross_project::physical_entry(&logical)?;
    let body = format!(
        "---\nalwaysApply: true\n---\n<!-- Pleat managed TRAE adaptation v1 -->\n\
        Apply this entry only when this request actually invokes the installed skill {quoted_name} \
        from {quoted_installation}, or its resolved equivalent. A matching name from another source \
        does not match this entry. Before following that skill, use the native file reading tool to \
        read its optional local adaptation at {quoted_adaptation} and apply the relevant instructions. \
        Read each resolved adaptation file at most once per request. If this skill is not installed \
        or has not been invoked, do not read it. If the optional file does not exist, continue with \
        the skill. Respect native permission failures and approval requirements; never change trust \
        or permissions to read it. Do not scan other skills, user directories, or run commands.\n"
    ).into_bytes();
    ManagedFileRule::new(logical, path, body, 16 * 1024)
}

pub(super) fn affected(project: &Path, installation: &Path) -> Result<bool> {
    let Some(name) = installation.file_name() else { return Ok(false); };
    let physical = crate::cross_project::physical_entry(installation)?;
    for target in [crate::InstallTarget::Trae, crate::InstallTarget::Universal] {
        let candidate = crate::agents::installation_root(project, target)?.join(name);
        if crate::cross_project::physical_entry(&candidate)? == physical { return Ok(true); }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(unix)]
    #[test]
    fn project_and_global_entries_preserve_sources_and_existing_rules() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = root.join("source");
        fs::create_dir_all(source.join("local-data")).unwrap();
        fs::write(source.join("SKILL.md"), "source body").unwrap();
        fs::write(source.join("local-data/ADAPTATION.md"), "private local canary").unwrap();
        for (folder, rules) in [(".trae", "rules"), (".trae-cn", "user_rules")] {
            let config = root.join(folder);
            fs::create_dir_all(config.join(rules)).unwrap();
            fs::write(config.join(rules).join("personal.md"), "personal rule").unwrap();
            fs::write(config.join("user_rules.md"), "legacy rule").unwrap();
            let installed = config.join("skills/demo");
            let mut entry = plan(&config, &installed).unwrap();
            assert_eq!(entry.path.parent().unwrap(), config.join(rules));
            assert!(entry.install().unwrap());
            let bytes = fs::read(&entry.path).unwrap();
            let text = String::from_utf8(bytes.clone()).unwrap();
            assert!(text.starts_with("---\nalwaysApply: true\n---\n"));
            assert!(text.contains(installed.join("local-data/ADAPTATION.md").to_str().unwrap()));
            assert!(!text.contains("private local canary"));
            fs::write(source.join("local-data/ADAPTATION.md"), "updated local canary").unwrap();
            let mut repeated = plan(&config, &installed).unwrap();
            assert!(!repeated.install().unwrap());
            assert_eq!(fs::read(&entry.path).unwrap(), bytes);
            assert!(repeated.detach().unwrap());
            assert!(!entry.path.exists());
            assert_eq!(fs::read_to_string(source.join("SKILL.md")).unwrap(), "source body");
            assert_eq!(fs::read_to_string(source.join("local-data/ADAPTATION.md")).unwrap(), "updated local canary");
            assert_eq!(fs::read_to_string(config.join(rules).join("personal.md")).unwrap(), "personal rule");
            assert_eq!(fs::read_to_string(config.join("user_rules.md")).unwrap(), "legacy rule");
        }
    }

    #[test]
    fn same_named_installations_keep_distinct_rule_identity() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("project/.trae");
        fs::create_dir_all(&config).unwrap();
        let first = plan(&config, &config.join("skills/demo")).unwrap();
        let second = plan(&config, &temp.path().join("project/.agents/skills/demo")).unwrap();
        assert_ne!(first.path, second.path);
        assert_ne!(first.body, second.body);
    }

    #[cfg(unix)]
    #[test]
    fn conflict_or_changed_rule_is_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join(".trae");
        fs::create_dir_all(&config).unwrap();
        let installed = config.join("skills/demo");
        let mut entry = plan(&config, &installed).unwrap();
        entry.install().unwrap();
        fs::write(&entry.path, "user changed this rule").unwrap();
        assert!(entry.detach().is_err());
        assert!(plan(&config, &installed).is_err());
        assert_eq!(fs::read_to_string(&entry.path).unwrap(), "user changed this rule");
    }

    #[test]
    fn malformed_native_paths_cannot_create_rules() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join(".trae");
        assert!(plan(&config, Path::new("relative/skills/demo")).is_err());
        assert!(plan(&config, &config.join("skills/demo\nrule")).is_err());
        assert!(plan(&temp.path().join("unknown"), &config.join("skills/demo")).is_err());
        assert!(!config.exists());
    }

    #[test]
    fn relative_project_input_has_the_same_rule_as_its_absolute_form() {
        let temp = tempfile::Builder::new().prefix(".trae-path-test-")
            .tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let relative = Path::new(temp.path().file_name().unwrap());
        let config = relative.join(".trae");
        let installation = config.join("skills/demo");
        let first = plan(&config, &installation).unwrap();
        let absolute = temp.path().join(".trae");
        let second = plan(&absolute, &absolute.join("skills/demo")).unwrap();
        assert_eq!(first.path, second.path);
        assert_eq!(first.body, second.body);
    }

    #[cfg(unix)]
    #[test]
    fn removal_keeps_rule_when_the_installation_preimage_changes() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path();
        let config = project.join(".trae");
        let installation = config.join("skills/demo");
        fs::create_dir_all(&installation).unwrap();
        let mut rule = plan(&config, &installation).unwrap();
        rule.install().unwrap();
        let mut removals = crate::host_instructions::RulePlan::trae_removals(project, &installation).unwrap();
        fs::rename(&installation, project.join("original-slot")).unwrap();
        fs::create_dir(&installation).unwrap();
        assert!(removals.iter_mut().any(|entry| entry.detach().is_err()));
        assert!(rule.path.is_file());
        fs::remove_dir(&installation).unwrap();
        let mut absent = crate::host_instructions::RulePlan::trae_removals(project, &installation).unwrap();
        fs::create_dir(&installation).unwrap();
        assert!(absent.iter_mut().any(|entry| entry.detach().is_err()));
        assert!(rule.path.is_file());
    }
}
