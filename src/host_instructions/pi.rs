//! Pi and OMP discover JavaScript extensions in each native scope. Each
//! installed Skill contributes one identity-bound descriptor to the shared hook.
use std::path::{Path, PathBuf};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use crate::InstallTarget;
use super::{ManagedFileRule, RuleKind, RulePlan};

const MAX_EXTENSION: u64 = 256 * 1024;
const SUFFIX: &str = ");\n}\n";

fn prefix(target: InstallTarget) -> String {
    let helper = include_str!("../../assets/host-adapters/pi-adaptation.js")
        .replacen("export default function piAdaptation", "function piAdaptation", 1);
    if target == InstallTarget::Omp {
        // Both protocol facades use the same Node built-ins and FD reader.
        // Bundle one native file; no sibling dependency or package install.
        let facade = include_str!("../../assets/host-adapters/omp-adaptation.js")
            .lines().filter(|line| !line.starts_with("import ")).collect::<Vec<_>>().join("\n")
            .replacen("export default function ompAdaptation", "function ompAdaptation", 1);
        return format!("// Pleat managed OMP adaptation extension v1\n{helper}\n{facade}\n\
            export default function pleatManagedOmpExtension(omp) {{\n\
            ompAdaptation(omp, ");
    }
    format!("// Pleat managed Pi adaptation extension v1\n{helper}\n\
        export default function pleatManagedPiExtension(pi) {{\n\
        piAdaptation(pi, ")
}

fn manifest(installation: &Path, source: &Path, identity: &str, name: &str) -> serde_json::Value {
    serde_json::json!({"schemaVersion":1,"skills":[{
        "installationSkillPath": installation.join("SKILL.md"),
        "canonicalSourceRoot": source,
        "sourceIdentity": identity,
        "nativeName": name
    }]})
}

fn config_encodings(value: &serde_json::Value) -> Result<(String, String)> {
    let entry = &value["manifest"]["skills"][0];
    let scalar = |key: &str| serde_json::to_string(&entry[key]);
    let canonical = super::canonical_json(value.clone())?;
    // Exact original json! insertion order, including the nested manifest.
    // Pi and OMP differ only in their immutable bundled JavaScript prefix.
    let legacy = format!(concat!("{{\"manifest\":{{\"schemaVersion\":1,\"skills\":[{{",
        "\"installationSkillPath\":{},\"canonicalSourceRoot\":{},\"sourceIdentity\":{},",
        "\"nativeName\":{}}}]}},\"readerExecutable\":{}}}"),
        scalar("installationSkillPath")?, scalar("canonicalSourceRoot")?, scalar("sourceIdentity")?,
        scalar("nativeName")?, serde_json::to_string(&value["readerExecutable"])?);
    Ok((canonical, legacy))
}

/// Reuse only this exact bundled helper and descriptor. An old source locator
/// may now be a storage compatibility alias to the very same directory inode.
fn compatible_existing(path: &Path, prefix: &str, installation: &Path, source: &Path,
    identity: &str, name: &str) -> Result<Option<Vec<u8>>> {
    let Some((bytes, _)) = super::snapshot(path, MAX_EXTENSION)? else { return Ok(None); };
    let Some(value) = std::str::from_utf8(&bytes).ok().and_then(|text| text.strip_prefix(prefix))
        .and_then(|text| text.strip_suffix(SUFFIX)).and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok()) else { return Ok(None); };
    let Some(locator) = value.pointer("/manifest/skills/0/canonicalSourceRoot").and_then(|v| v.as_str()).map(PathBuf::from)
        else { return Ok(None); };
    let Some(reader) = value.get("readerExecutable").and_then(|v| v.as_str()).map(PathBuf::from)
        else { return Ok(None); };
    // Preserve a different CLI/desktop reader only at a canonical file path.
    if !locator.is_absolute() || locator.canonicalize().ok().as_deref() != Some(source)
        || !reader.is_absolute() || reader.to_str().is_none_or(|s| s.chars().any(char::is_control))
        || !reader.is_file() || reader.canonicalize().ok().as_deref() != Some(reader.as_path())
        || value != serde_json::json!({"manifest":manifest(installation, &locator, identity, name),"readerExecutable":reader}) {
        return Ok(None);
    }
    // A parsed equivalent JSON document alone is insufficient ownership proof.
    let (canonical, legacy) = config_encodings(&value)?;
    Ok((bytes == format!("{prefix}{canonical}{SUFFIX}").as_bytes()
        || bytes == format!("{prefix}{legacy}{SUFFIX}").as_bytes()).then_some(bytes))
}

fn paths(target: InstallTarget, installation: &Path, config: Option<&Path>) -> Result<(PathBuf, PathBuf, String)> {
    let installation = if installation.is_absolute() { installation.to_path_buf() }
        else { std::env::current_dir()?.join(installation) };
    let name = installation.file_name().and_then(|value| value.to_str())
        .context("Pi 技能目录名称编码无效")?.to_owned();
    crate::library::validate_name(&name)?;
    ensure!(!name.chars().any(char::is_control), "Pi 原生扩展不支持控制字符名称");
    let config = config.or_else(|| installation.parent().and_then(Path::parent)).context("缺少原生扩展配置目录")?;
    // Keep Pi's established native basename locator. Compatibility roots need
    // distinct descriptors in the native config, even with equal basenames.
    let native_pi = target == InstallTarget::Pi && installation.parent() == Some(config.join("skills").as_path());
    let digest = format!("{:x}", Sha256::digest(if !native_pi {
        installation.as_os_str().as_encoded_bytes()
    } else { name.as_bytes() }));
    let logical = config.join("extensions").join(format!("pleat-adaptation-{}.js", &digest[..24]));
    Ok((logical, installation, name))
}

pub(super) fn plan_for(target: InstallTarget, installation: &Path, source: &Path) -> Result<RulePlan> {
    plan_at(target, installation, source, None)
}

pub(super) fn plan_at(target: InstallTarget, installation: &Path, source: &Path, config: Option<&Path>) -> Result<RulePlan> {
    let (logical, installation, name) = paths(target, installation, config)?;
    let source = source.canonicalize().context("Pi 技能源不存在")?;
    ensure!(source.is_dir() && source.join("SKILL.md").is_file(), "Pi 技能源缺少 SKILL.md");
    let identity = crate::library::filesystem_identity(&source)?;
    // One generated file makes publication/removal atomic. JSON serialization
    // protects literal paths; no shell, user environment or Pi settings edits.
    let path = crate::cross_project::physical_entry(&logical)?;
    let prefix = prefix(target);
    let reader = std::env::current_exe()?.canonicalize()?;
    let config = serde_json::json!({"manifest":manifest(&installation, &source, &identity, &name),"readerExecutable":reader});
    let body = if let Some(bytes) = compatible_existing(&path, &prefix, &installation, &source, &identity, &name)? {
        bytes
    } else {
        let canonical = super::canonical_json(config)?;
        format!("{prefix}{canonical}{SUFFIX}").into_bytes()
    };
    let mut file = ManagedFileRule::new(logical, path, body, MAX_EXTENSION)?;
    file.source_guard = Some((source, identity));
    Ok(RulePlan { path: file.path.clone(), kind: RuleKind::File(file) })
}

pub(super) fn existing_for(target: InstallTarget, installation: &Path) -> Result<Option<RulePlan>> {
    let (logical, _, _) = paths(target, installation, None)?;
    let path = crate::cross_project::physical_entry(&logical)?;
    match installation.canonicalize() {
        Ok(source) => plan_for(target, installation, &source).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && path.symlink_metadata().is_err() => Ok(None),
        Err(_) => anyhow::bail!("Pi 安装来源不可验证，原生扩展已保留，请刷新后重试"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    fn plan(installation: &Path, source: &Path) -> Result<RulePlan> {
        plan_for(InstallTarget::Pi, installation, source)
    }
    fn existing(installation: &Path) -> Result<Option<RulePlan>> {
        existing_for(InstallTarget::Pi, installation)
    }
    #[cfg(unix)]
    #[test]
    fn pi_native_locator_is_stable_and_compatibility_roots_have_distinct_descriptors() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("SKILL.md"), "Owned fixture").unwrap();
        let config = root.join(".pi");
        let native = config.join("skills/probe");
        let shared = root.join(".agents/skills/probe");
        let compat = root.join("compat/skills/probe");
        let legacy = plan(&native, &source).unwrap();
        let mut native_plan = plan_at(InstallTarget::Pi, &native, &source, Some(&config)).unwrap();
        let mut shared_plan = plan_at(InstallTarget::Pi, &shared, &source, Some(&config)).unwrap();
        let compat_plan = plan_at(InstallTarget::Pi, &compat, &source, Some(&config)).unwrap();
        assert_eq!(legacy.path, native_plan.path);
        let digest = format!("{:x}", Sha256::digest(b"probe"));
        assert_eq!(native_plan.path, config.join(format!("extensions/pleat-adaptation-{}.js", &digest[..24])));
        assert_ne!(native_plan.path, shared_plan.path);
        assert_ne!(compat_plan.path, shared_plan.path);
        assert_eq!(compat_plan.path.parent(), shared_plan.path.parent());
        native_plan.install().unwrap();
        shared_plan.install().unwrap();
        let body = fs::read_to_string(&shared_plan.path).unwrap();
        assert!(body.contains(&serde_json::to_string(&shared.join("SKILL.md")).unwrap()));
        assert!(!body.contains(&serde_json::to_string(&native.join("SKILL.md")).unwrap()));
        assert!(shared_plan.detach().unwrap());
        assert!(native_plan.path.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn shared_root_descriptor_preserves_established_source_locator_after_move() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let old = root.join("library");
        let source = old.join("probe");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("SKILL.md"), "Owned fixture").unwrap();
        let link = root.join(".agents/skills/probe");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&source, &link).unwrap();
        let config = root.join(".pi");
        let mut plan = plan_at(InstallTarget::Pi, &link, &source, Some(&config)).unwrap();
        plan.install().unwrap();
        let before = fs::read(&plan.path).unwrap();
        let moved = root.join("moved");
        fs::rename(&old, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &old).unwrap();
        let mut refreshed = plan_at(InstallTarget::Pi, &link, &moved.join("probe"), Some(&config)).unwrap();
        assert!(!refreshed.needs_install());
        assert!(!refreshed.install().unwrap());
        assert_eq!(fs::read(&plan.path).unwrap(), before);
        assert!(refreshed.detach().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn generated_extension_has_scope_and_source_identity_and_keeps_conflicts() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("SKILL.md"), "---\nname: probe\ndescription: owned\n---\nBody\n").unwrap();
        let link = root.join(".pi/skills/probe");
        let mut p = plan(&link, &source).unwrap();
        assert!(p.install().unwrap());
        let body = fs::read_to_string(&p.path).unwrap();
        assert!(body.contains("\"manifest\":") && body.contains("sourceIdentity") && body.contains("readerExecutable"));
        assert!(body.contains(&link.join("SKILL.md").to_string_lossy().to_string()));
        assert_eq!(body.matches("export default function").count(), 1);
        assert!(!plan(&link, &source).unwrap().needs_install());
        fs::write(&p.path, "// user replacement\n").unwrap();
        assert!(p.detach().is_err());
        assert!(plan(&link, &source).is_err());
        assert_eq!(fs::read_to_string(p.path).unwrap(), "// user replacement\n");
    }
    #[test]
    fn source_replacement_after_preview_does_not_publish_extension() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("SKILL.md"), "Owned fixture").unwrap();
        let mut p = plan(&root.join(".pi/skills/probe"), &source).unwrap();
        fs::rename(&source, root.join("old-source")).unwrap();
        fs::create_dir(&source).unwrap();
        fs::write(source.join("SKILL.md"), "Concurrent source").unwrap();
        assert!(p.install().is_err());
        assert!(!p.path.exists());
    }
    #[cfg(unix)]
    #[test]
    fn library_move_compatibility_locator_retains_exact_owned_extension() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let old = root.join("library");
        let source = old.join("probe");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("SKILL.md"), "Owned fixture").unwrap();
        let link = root.join(".pi/skills/probe");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&source, &link).unwrap();
        let mut p = plan(&link, &source).unwrap();
        p.install().unwrap();
        let before = fs::read(&p.path).unwrap();
        let next = root.join("moved-library");
        fs::rename(&old, &next).unwrap();
        std::os::unix::fs::symlink(&next, &old).unwrap();
        let mut refreshed = existing(&link).unwrap().unwrap();
        assert!(!refreshed.needs_install());
        assert!(!refreshed.install().unwrap());
        assert_eq!(fs::read(&p.path).unwrap(), before);
        assert!(refreshed.detach().unwrap());
        assert!(!p.path.exists());
        assert_eq!(link.canonicalize().unwrap(), next.join("probe"));
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn inherited_acl_does_not_expose_extension_or_recovery() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("SKILL.md"), "Owned fixture").unwrap();
        fs::create_dir(root.join(".pi")).unwrap();
        let status = std::process::Command::new("/bin/chmod").args(["+a", "everyone allow read,readattr,readextattr,readsecurity,file_inherit,directory_inherit"])
            .arg(root.join(".pi")).status().unwrap();
        assert!(status.success());
        let mut p = plan(&root.join(".pi/skills/probe"), &source).unwrap();
        p.install().unwrap();
        let access = crate::file_access::Access::read(&fs::File::open(&p.path).unwrap()).unwrap();
        assert_eq!(access.mode(), 0o600);
        assert!(!access.has_acl());
        p.detach().unwrap();
        for entry in fs::read_dir(root.join(".pi")).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(".pleat-removed-rule-")) {
            let access = crate::file_access::Access::read(&fs::File::open(entry.path()).unwrap()).unwrap();
            assert_eq!(access.mode(), 0o700);
            assert!(!access.has_acl());
        }
    }
}
