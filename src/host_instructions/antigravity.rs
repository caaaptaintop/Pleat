//! Antigravity native rules refer to the logical installed skill, never source text.
//! Global modular rule discovery still requires fixed-version native acceptance.
use std::path::Path;
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use super::ManagedFileRule;

pub(super) fn plan(config: &Path, installation: &Path) -> Result<ManagedFileRule> {
    // Reject malformed input before normalization can discard a component.
    for path in [config, installation] {
        let text = path.to_str().context("Antigravity 路径编码无效")?;
        ensure!(!text.chars().any(char::is_control), "Antigravity 路径不支持控制字符");
    }
    // Validate the host's logical root before resolving a user directory alias.
    // A .gemini symlink may legitimately point to a differently named folder.
    match config.file_name().and_then(|name| name.to_str()) {
        Some(".agent" | ".agents") => {},
        Some("config") if config.parent().and_then(Path::file_name)
            .and_then(|name| name.to_str()) == Some(".gemini") => {},
        _ => anyhow::bail!("无法确认 Antigravity 原生规则目录，已保留"),
    }
    let (config, installation) = super::normalize_file_rule_paths(config, installation)?;
    ensure!(config.is_absolute() && installation.is_absolute(),
        "Antigravity 原生规则需要绝对路径");
    // The shared helper resolves config prefixes, not the logical skill link.
    for path in [&config, &installation] {
        let text = path.to_str().context("Antigravity 路径编码无效")?;
        ensure!(!text.chars().any(char::is_control), "Antigravity 路径不支持控制字符");
    }
    let installed = installation.to_str().context("Antigravity 安装路径编码无效")?;
    let name = installation.file_name().and_then(|name| name.to_str()).context("技能名称编码无效")?;
    crate::library::validate_name(name)?;
    let quoted_name = serde_json::to_string(name)?;
    let quoted_installation = serde_json::to_string(installed)?;
    let skill = installation.join("SKILL.md");
    let quoted_skill = serde_json::to_string(skill.to_str().context("技能路径编码无效")?)?;
    let adaptation = installation.join("local-data/ADAPTATION.md");
    let quoted_adaptation = serde_json::to_string(adaptation.to_str().context("本机适配路径编码无效")?)?;
    let digest = format!("{:x}", Sha256::digest(installed.as_bytes()));
    let logical = config.join("rules").join(format!("pleat-adaptation-{}.md", &digest[..24]));
    let path = crate::cross_project::physical_entry(&logical)?;
    let body = format!(
        "---\ntrigger: always_on\n---\n<!-- Pleat managed Antigravity adaptation v1 -->\n\
        Apply this entry only when this request actually invokes the installed skill {quoted_name} \
        from the exact installed path {quoted_installation}, or its resolved equivalent. A matching \
        name from another source does not match this entry. Before following that skill, use the \
        native file reading tool to read its current instructions at {quoted_skill}, then its \
        optional local adaptation at {quoted_adaptation}, and apply the relevant instructions. \
        Read each resolved file at most once per request. If this skill \
        is not installed or has not been invoked in this request, do not read the adaptation. If \
        the optional file does not exist, continue with the skill. Respect native permissions, \
        trust, permission failures and approval requirements; never change trust or permissions \
        to read it. Do not scan other skills or user directories, or run commands.\n"
    ).into_bytes();
    ManagedFileRule::new(logical, path, body, 16 * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn supported_roots_have_exact_native_header_and_logical_path_identity() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for folder in ["project/.agents", "project/.agent", "home/.gemini/config"] {
            let config = root.join(folder);
            if folder != "home/.gemini/config" {
                fs::create_dir_all(&config).unwrap();
            }
            let installed = config.join("skills/demo");
            let entry = plan(&config, &installed).unwrap();
            let digest = format!("{:x}", Sha256::digest(installed.to_str().unwrap().as_bytes()));
            assert_eq!(entry.logical, config.join("rules").join(format!("pleat-adaptation-{}.md", &digest[..24])));
            assert_eq!(entry.path, entry.logical);
            let text = std::str::from_utf8(&entry.body).unwrap();
            assert_eq!(text.lines().take(3).collect::<Vec<_>>(), ["---", "trigger: always_on", "---"]);
            assert!(text.starts_with("---\ntrigger: always_on\n---\n"));
            assert!(text.contains("<!-- Pleat managed Antigravity adaptation v1 -->"));
            assert!(text.contains(&serde_json::to_string("demo").unwrap()));
            assert!(text.contains(&serde_json::to_string(installed.to_str().unwrap()).unwrap()));
            assert!(text.contains(&serde_json::to_string(installed.join("local-data/ADAPTATION.md").to_str().unwrap()).unwrap()));
            assert_eq!(entry.max_bytes, 16 * 1024);
            assert!(!config.join("rules").exists());
        }
    }

    #[test]
    fn fresh_global_publication_creates_missing_gemini_config_through_common_transaction() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let config = root.join(".gemini/config");
        let installed = config.join("skills/demo");
        assert!(!root.join(".gemini").exists());
        let mut entry = plan(&config, &installed).unwrap();
        assert!(!root.join(".gemini").exists());
        assert_eq!(entry.path.parent().unwrap(), config.join("rules"));
        assert!(entry.install().unwrap());
        let bytes = fs::read(&entry.path).unwrap();
        assert!(bytes.starts_with(b"---\ntrigger: always_on\n---\n"));
        assert!(!installed.exists());
        assert!(!root.join(".gemini/GEMINI.md").exists());
        let mut repeated = plan(&config, &installed).unwrap();
        assert!(!repeated.install().unwrap());
        assert_eq!(fs::read(&entry.path).unwrap(), bytes);
        assert!(repeated.detach().unwrap());
        assert!(!entry.path.exists());
        assert!(!root.join(".gemini/GEMINI.md").exists());
    }

    #[test]
    fn common_transaction_is_idempotent_and_preserves_personal_and_skill_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = root.join("source");
        fs::create_dir_all(source.join("local-data")).unwrap();
        fs::write(source.join("SKILL.md"), "SOURCE_BODY_CANARY").unwrap();
        fs::write(source.join("local-data/ADAPTATION.md"), "SOURCE_ADP_CANARY").unwrap();
        for folder in ["project/.agents", "project/.agent", "home/.gemini/config"] {
            let config = root.join(folder);
            let installed = config.join("skills/demo");
            fs::create_dir_all(installed.join("local-data")).unwrap();
            fs::write(installed.join("SKILL.md"), "INSTALLED_BODY_CANARY").unwrap();
            fs::write(installed.join("local-data/ADAPTATION.md"), "INSTALLED_ADP_CANARY").unwrap();
            fs::create_dir(config.join("rules")).unwrap();
            let personal = config.join("rules/personal.md");
            let legacy = config.join("GEMINI.md");
            let parent_legacy = config.parent().unwrap().join("GEMINI.md");
            for file in [&personal, &legacy, &parent_legacy] { fs::write(file, "personal content").unwrap(); }
            let mut entry = plan(&config, &installed).unwrap();
            assert!(entry.install().unwrap());
            let bytes = fs::read(&entry.path).unwrap();
            let text = std::str::from_utf8(&bytes).unwrap();
            for canary in ["SOURCE_BODY_CANARY", "SOURCE_ADP_CANARY", "INSTALLED_BODY_CANARY", "INSTALLED_ADP_CANARY"] {
                assert!(!text.contains(canary));
            }
            fs::write(installed.join("local-data/ADAPTATION.md"), "UPDATED_ADP_CANARY").unwrap();
            let mut repeated = plan(&config, &installed).unwrap();
            assert!(!repeated.install().unwrap());
            assert_eq!(fs::read(&entry.path).unwrap(), bytes);
            assert!(repeated.detach().unwrap());
            assert!(!repeated.detach().unwrap());
            assert!(!entry.path.exists());
            assert_eq!(fs::read_to_string(source.join("SKILL.md")).unwrap(), "SOURCE_BODY_CANARY");
            assert_eq!(fs::read_to_string(source.join("local-data/ADAPTATION.md")).unwrap(), "SOURCE_ADP_CANARY");
            assert_eq!(fs::read_to_string(installed.join("SKILL.md")).unwrap(), "INSTALLED_BODY_CANARY");
            assert_eq!(fs::read_to_string(installed.join("local-data/ADAPTATION.md")).unwrap(), "UPDATED_ADP_CANARY");
            for file in [&personal, &legacy, &parent_legacy] {
                assert_eq!(fs::read_to_string(file).unwrap(), "personal content");
            }
            assert!(fs::read_dir(&config).unwrap().flatten().any(|item|
                item.file_name().to_string_lossy().starts_with(".pleat-removed-rule-")
                && fs::read(item.path().join("instruction.md")).is_ok_and(|saved| saved == bytes)));
        }
    }

    #[test]
    fn existing_content_and_changes_after_preview_are_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().canonicalize().unwrap().join(".agents");
        fs::create_dir_all(config.join("rules")).unwrap();
        let installed = config.join("skills/demo");
        let mut preview = plan(&config, &installed).unwrap();
        fs::write(&preview.path, "personal conflict").unwrap();
        assert!(preview.install().is_err());
        assert!(plan(&config, &installed).is_err());
        assert_eq!(fs::read_to_string(&preview.path).unwrap(), "personal conflict");
        let mut managed = plan(&config, &config.join("skills/other")).unwrap();
        managed.install().unwrap();
        fs::write(&managed.path, "user changed this rule").unwrap();
        assert!(managed.detach().is_err());
        assert!(plan(&config, &config.join("skills/other")).is_err());
        assert_eq!(fs::read_to_string(&managed.path).unwrap(), "user changed this rule");
    }

    #[test]
    fn same_name_in_different_logical_installations_has_distinct_identity() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let config = root.join(".agents");
        let first = plan(&config, &config.join("skills/demo")).unwrap();
        let second = plan(&config, &root.join(".agent/skills/demo")).unwrap();
        assert_ne!(first.path, second.path);
        assert_ne!(first.body, second.body);
    }

    #[cfg(unix)]
    #[test]
    fn skill_symlinks_keep_logical_identity_and_do_not_require_adaptation() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        let config = root.join(".agents");
        fs::create_dir_all(config.join("skills")).unwrap();
        let first_link = config.join("skills/demo");
        let second_link = root.join(".agent/skills/demo");
        fs::create_dir_all(second_link.parent().unwrap()).unwrap();
        symlink(&source, &first_link).unwrap();
        symlink(&source, &second_link).unwrap();
        let first = plan(&config, &first_link).unwrap();
        let second = plan(&config, &second_link).unwrap();
        assert_ne!(first.path, second.path);
        assert!(std::str::from_utf8(&first.body).unwrap().contains(&serde_json::to_string(first_link.to_str().unwrap()).unwrap()));
        assert!(!std::str::from_utf8(&first.body).unwrap().contains(source.to_str().unwrap()));
        assert_eq!(fs::read_link(&first_link).unwrap(), source);
        assert_eq!(fs::read_dir(&source).unwrap().count(), 0);
    }

    #[test]
    fn removal_preserves_the_rule_when_its_installation_changes_after_preview() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path();
        let config = project.join(".agent");
        let installation = config.join("skills/demo");
        fs::create_dir_all(&installation).unwrap();
        let mut rule = plan(&config, &installation).unwrap();
        rule.install().unwrap();
        let mut removals = crate::host_instructions::RulePlan::file_host_removals(project, &installation).unwrap();
        fs::rename(&installation, project.join("original-slot")).unwrap();
        fs::create_dir(&installation).unwrap();
        assert!(removals.iter_mut().any(|entry| entry.detach().is_err()));
        assert!(rule.path.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn global_gemini_directory_alias_keeps_anchored_publication_and_recovery() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let actual = root.join("preferences");
        fs::create_dir(&actual).unwrap();
        fs::write(actual.join("GEMINI.md"), "personal instructions").unwrap();
        symlink(&actual, root.join(".gemini")).unwrap();
        let config = root.join(".gemini/config");
        let installed = config.join("skills/demo");
        let mut entry = plan(&config, &installed).unwrap();
        assert_eq!(entry.path.parent().unwrap(), actual.join("config/rules"));
        assert!(entry.install().unwrap());
        let bytes = fs::read(&entry.path).unwrap();
        let mut repeated = plan(&config, &installed).unwrap();
        assert!(!repeated.install().unwrap());
        assert_eq!(fs::read(&entry.path).unwrap(), bytes);
        assert!(repeated.detach().unwrap());
        assert!(!entry.path.exists());
        assert_eq!(fs::read_link(root.join(".gemini")).unwrap(), actual);
        assert_eq!(fs::read_to_string(actual.join("GEMINI.md")).unwrap(), "personal instructions");
        assert!(!installed.exists());
    }

    #[test]
    fn quoted_path_components_are_json_strings() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().canonicalize().unwrap().join("project \"quoted\" \\ path");
        fs::create_dir(&parent).unwrap();
        let config = parent.join(".agents");
        let installed = config.join("skills/demo");
        let entry = plan(&config, &installed).unwrap();
        let text = std::str::from_utf8(&entry.body).unwrap();
        assert!(text.contains(&serde_json::to_string(installed.to_str().unwrap()).unwrap()));
        assert!(text.contains(&serde_json::to_string(installed.join("local-data/ADAPTATION.md").to_str().unwrap()).unwrap()));
    }

    #[test]
    fn invalid_roots_and_control_characters_are_rejected_without_writes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let installed = root.join(".agents/skills/demo");
        fs::create_dir(root.join("other")).unwrap();
        fs::create_dir(root.join(".gemini")).unwrap();
        for folder in ["unknown", "config", "other/config", ".gemini", ".gemini/other"] {
            assert!(plan(&root.join(folder), &installed).is_err());
        }
        for control in ['\n', '\r', '\t', '\0', '\u{7f}'] {
            assert!(plan(&root.join(".agents"), &root.join(format!("skills/de{control}mo"))).is_err());
            assert!(plan(&root.join(format!("project{control}/.agents")), &installed).is_err());
        }
        assert!(!root.join(".agents").exists());
        assert!(!root.join("config").exists());
        assert!(!root.join("other/config").exists());
        assert!(!root.join(".gemini/rules").exists());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_are_rejected_without_writes() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let invalid = OsString::from_vec(vec![b'd', 0xff]);
        let config = root.join(".agents");
        assert!(plan(&config, &root.join(&invalid)).is_err());
        assert!(plan(&root.join(&invalid).join(".agents"), &config.join("skills/demo")).is_err());
        assert!(!config.exists());
    }
}
