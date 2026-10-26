//! WorkBuddy 5.8.2 desktop first-turn rules: a conditional native Read request.
//! This is a static entry, not a native per-skill hook or verified model load.
//! Caller supplies ONE verified runtime-config/rules or exact-cwd/.codebuddy/rules.
//! Skill roots are separate; never infer the rules root from an installed skill.
use std::path::{Component, Path};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use super::ManagedFileRule;

const MAX_RULE_BYTES: u64 = 16 * 1024;
const MAX_NATIVE_BODY_UTF16: usize = 40_000;

/// Prepare only; shared ManagedFileRule owns publication, CAS and recovery.
/// The caller must bind the root to the reviewed desktop runtime and version.
/// No source/adaptation contents, environment or user configuration are read.
pub(super) fn plan_at(rules_dir: &Path, installation: &Path) -> Result<ManagedFileRule> {
    for path in [rules_dir, installation] { validate_path(path)?; }
    ensure!(rules_dir.file_name().and_then(|name| name.to_str()) == Some("rules"),
        "WorkBuddy 需要已确认的原生 rules 目录");
    let config = rules_dir.parent().context("缺少 WorkBuddy 配置目录")?;
    // Reuse the common parent normalization without resolving the skill link.
    let (config, installation) = super::normalize_file_rule_paths(config, installation)?;
    for path in [config.as_path(), installation.as_path()] { validate_path(path)?; }
    let installed = installation.to_str().context("WorkBuddy 安装路径编码无效")?;
    let name = installation.file_name().and_then(|name| name.to_str())
        .context("技能名称编码无效")?;
    crate::library::validate_name(name)?;
    let quoted_name = quoted_path(name)?;
    let quoted_installation = quoted_path(installed)?;
    let adaptation = installation.join("local-data/ADAPTATION.md");
    let quoted_adaptation = quoted_path(adaptation.to_str().context("本机适配路径编码无效")?)?;
    // Full path identity: same name in another installation is a different rule.
    let digest = format!("{:x}", Sha256::digest(installed.as_bytes()));
    let logical = config.join("rules").join(format!("pleat-adaptation-workbuddy-{digest}.mdc"));
    let path = crate::cross_project::physical_entry(&logical)?;
    let content = format!(
        "<!-- pleat-managed:workbuddy-adaptation:v1 installation-sha256={digest} installation={quoted_installation} -->\n\
        This entry applies only when this request actually invokes the installed skill {quoted_name} \
        from the exact installed directory {quoted_installation}, or that directory's resolved equivalent. \
        A matching name from another installation or source does not match. Do not select or invoke \
        any skill merely because this rule exists; respect the user's trigger choice and native skill selection. \
        Before executing that selected skill's instructions, use the native Read tool to read the current \
        optional local adaptation at {quoted_adaptation}, through this exact installation link, and apply \
        its relevant instructions. Read each resolved adaptation file at most once per request. If this \
        installation does not exist or this skill is not invoked, do not read the adaptation. If only the \
        optional adaptation file is absent, continue with the skill. If Read is denied or otherwise fails, \
        report the failure and stop work depending on that adaptation; respect native permissions, trust, \
        hooks and approval requirements. Never change them or use another tool to bypass a refusal. \
        Do not scan other skills or user directories, and do not run commands to load this adaptation.\n"
    );
    // Native parseRuleFile counts JS UTF-16 units after frontmatter and trim.
    ensure!(content.trim().encode_utf16().count() <= MAX_NATIVE_BODY_UTF16,
        "WorkBuddy 原生规则正文超过 UTF-16 大小上限");
    let body = format!("---\nenabled: true\nalwaysApply: true\n---\n{content}").into_bytes();
    ManagedFileRule::new(logical, path, body, MAX_RULE_BYTES)
}

fn validate_path(path: &Path) -> Result<()> {
    let text = path.to_str().context("WorkBuddy 路径编码无效")?;
    ensure!(path.is_absolute() && !text.chars().any(|ch|
        ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}')),
        "WorkBuddy 原生规则需要无控制字符的绝对路径");
    // Reject traversal before common normalization could discard it.
    ensure!(!text.split('/').any(|part| matches!(part, "." | ".."))
        && !path.components().any(|part| matches!(part, Component::ParentDir | Component::CurDir)),
        "WorkBuddy 原生规则路径不支持点路径分量");
    Ok(())
}

// JSON strings preserve spaces, quotes and backslashes. Escaping HTML delimiters
// keeps even a path containing "-->" inside the exact-installation marker.
fn quoted_path(value: &str) -> Result<String> {
    Ok(serde_json::to_string(value)?.replace('<', "\\u003c")
        .replace('>', "\\u003e").replace('&', "\\u0026"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn explicit_roots_have_native_header_exact_identity_and_no_planning_writes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for folder in ["runtime-config/rules", "project/.codebuddy/rules"] {
            let rules = root.join(folder);
            let installed = root.join("project/.workbuddy/skills/demo");
            let rule = plan_at(&rules, &installed).unwrap();
            let digest = format!("{:x}", Sha256::digest(installed.to_str().unwrap().as_bytes()));
            assert_eq!(rule.logical, rules.join(format!("pleat-adaptation-workbuddy-{digest}.mdc")));
            assert_eq!(rule.path, rule.logical);
            let text = std::str::from_utf8(&rule.body).unwrap();
            assert!(text.starts_with("---\nenabled: true\nalwaysApply: true\n---\n"));
            assert!(text.contains(&format!("installation-sha256={digest} installation={} -->",
                quoted_path(installed.to_str().unwrap()).unwrap())));
            assert!(text.contains("native Read tool"));
            assert!(text.contains(&quoted_path(installed.join("local-data/ADAPTATION.md").to_str().unwrap()).unwrap()));
            assert!(!text.contains("\npaths:"));
            assert_eq!(rule.max_bytes, MAX_RULE_BYTES);
            assert!(rule.body.len() as u64 <= MAX_RULE_BYTES);
            let content = text.strip_prefix("---\nenabled: true\nalwaysApply: true\n---\n").unwrap();
            assert!(content.trim().encode_utf16().count() <= MAX_NATIVE_BODY_UTF16);
            assert!(!rules.exists());
            assert!(!installed.exists());
        }
    }

    #[test]
    fn same_name_different_installation_has_distinct_identity() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let rules = root.join("runtime/rules");
        let first = plan_at(&rules, &root.join("one/skills/demo")).unwrap();
        let second = plan_at(&rules, &root.join("two/skills/demo")).unwrap();
        assert_ne!(first.path, second.path);
        assert_ne!(first.body, second.body);
    }

    #[test]
    fn quoted_unicode_paths_cannot_break_the_installation_marker() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let installed = root.join("中文 space \"quoted\" \\ --> & <tag>/skills/demo");
        let rule = plan_at(&root.join("runtime/rules"), &installed).unwrap();
        let text = std::str::from_utf8(&rule.body).unwrap();
        let marker = text.lines().nth(4).unwrap();
        assert_eq!(marker.matches("-->").count(), 1);
        assert_eq!(marker.matches("<!--").count(), 1);
        let encoded = marker.split_once(" installation=").unwrap().1.strip_suffix(" -->").unwrap();
        assert_eq!(serde_json::from_str::<String>(encoded).unwrap(), installed.to_str().unwrap());
        assert!(text.contains(&quoted_path(installed.join("local-data/ADAPTATION.md").to_str().unwrap()).unwrap()));
    }

    #[cfg(unix)]
    #[test]
    fn installed_symlink_stays_logical_and_bodies_are_not_projected() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = root.join("source");
        fs::create_dir_all(source.join("local-data")).unwrap();
        fs::write(source.join("SKILL.md"), "SKILL_BODY_CANARY").unwrap();
        fs::write(source.join("local-data/ADAPTATION.md"), "ADAPTATION_BODY_CANARY").unwrap();
        let installed = root.join("project/.workbuddy/skills/demo");
        fs::create_dir_all(installed.parent().unwrap()).unwrap();
        symlink(&source, &installed).unwrap();
        let rules = root.join("project/.codebuddy/rules");
        let first = plan_at(&rules, &installed).unwrap();
        fs::write(source.join("local-data/ADAPTATION.md"), "CHANGED_ADAPTATION_CANARY").unwrap();
        let second = plan_at(&rules, &installed).unwrap();
        assert_eq!(first.body, second.body);
        let text = std::str::from_utf8(&first.body).unwrap();
        for canary in ["SKILL_BODY_CANARY", "ADAPTATION_BODY_CANARY", "CHANGED_ADAPTATION_CANARY"] {
            assert!(!text.contains(canary));
        }
        assert!(text.contains(&quoted_path(installed.to_str().unwrap()).unwrap()));
        assert!(!text.contains(source.to_str().unwrap()));
        assert_eq!(fs::read_link(&installed).unwrap(), source);
        assert!(!rules.exists());
    }

    #[test]
    fn existing_user_rule_and_preview_race_are_rejected_by_shared_cas() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let rules = root.join("runtime/rules");
        let installed = root.join("skills/demo");
        let preview = plan_at(&rules, &installed).unwrap();
        fs::create_dir_all(&rules).unwrap();
        fs::write(&preview.path, "user rule").unwrap();
        assert!(preview.validate().is_err());
        assert!(plan_at(&rules, &installed).is_err());
        assert_eq!(fs::read_to_string(&preview.path).unwrap(), "user rule");
        assert_eq!(fs::read_dir(&rules).unwrap().count(), 1);
    }

    #[test]
    fn exact_existing_body_is_idempotent_but_user_edits_conflict() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let rules = root.join("runtime/rules");
        let installed = root.join("skills/demo");
        let preview = plan_at(&rules, &installed).unwrap();
        fs::create_dir_all(&rules).unwrap();
        fs::write(&preview.path, &preview.body).unwrap();
        let repeated = plan_at(&rules, &installed).unwrap();
        assert!(!repeated.needs_install());
        fs::write(&preview.path, "edited by user").unwrap();
        assert!(repeated.validate().is_err());
        assert!(plan_at(&rules, &installed).is_err());
    }

    #[test]
    fn malformed_paths_and_oversize_body_fail_without_writes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let rules = root.join("runtime/rules");
        let installed = root.join("skills/demo");
        assert!(plan_at(Path::new("relative/rules"), &installed).is_err());
        assert!(plan_at(&rules, Path::new("relative/skills/demo")).is_err());
        assert!(plan_at(&root.join("runtime/user_rules"), &installed).is_err());
        assert!(plan_at(&root.join("runtime/../other/rules"), &installed).is_err());
        assert!(plan_at(&rules, &root.join("skills/./demo")).is_err());
        for control in ['\n', '\r', '\t', '\0', '\u{7f}', '\u{2028}', '\u{2029}'] {
            assert!(plan_at(&rules, &root.join(format!("skills/de{control}mo"))).is_err());
            assert!(plan_at(&root.join(format!("run{control}time/rules")), &installed).is_err());
        }
        // Missing directory components are never created by planning.
        let mut long = root.join("long");
        for _ in 0..25 { long = long.join("x".repeat(200)); }
        assert!(plan_at(&rules, &long.join("demo")).is_err());
        assert!(!root.join("long").exists());
        assert!(!root.join("runtime").exists());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_input_is_rejected() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let invalid = OsString::from_vec(vec![b'x', 0xff]);
        assert!(plan_at(&root.join("runtime/rules"), &root.join(&invalid)).is_err());
        assert!(plan_at(&root.join(&invalid).join("rules"), &root.join("skills/demo")).is_err());
        assert!(!root.join("runtime").exists());
    }
}
