//! Native profile plugin transactions, shared by Hermes and OpenClaw.
//! Immutable files, registration locks and config recovery are pooled per batch.
use super::{ManagedFileRule, opencode};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub(super) const PLUGIN_ID: &str = "pleat-native-skills";
const MAX_DESCRIPTOR: u64 = 16 * 1024;
type SharedFile = Arc<Mutex<ManagedFileRule>>;
pub(super) type Pool = BTreeMap<PathBuf, SharedFile>;

#[derive(Clone, Debug)]
pub(super) struct Plan {
    pub(super) path: PathBuf,
    descriptor: ManagedFileRule,
    core: Vec<SharedFile>,
    config: opencode::ConfigPlan,
    registrations: PathBuf,
    logical_plugin_root: PathBuf,
    plugin_root: PathBuf,
    internal_files: Vec<&'static str>,
    profile_guard: Option<(PathBuf, PathBuf)>,
}

fn internal_file(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "Hermes 内部适配登记不是普通文件，已保留"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                ensure!(
                    metadata.nlink() == 1,
                    "Hermes 内部适配登记存在硬链接，已保留"
                );
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => anyhow::bail!("Hermes 内部适配登记不可读，已保留"),
    }
}

fn internal_routes(logical: &Path, physical: &Path, record: &Path, files: &[&str]) -> Result<()> {
    ensure!(
        crate::agents::hermes::hermes_directory_identity(logical)? == physical,
        "Hermes 插件根目录已变化，请重新预览"
    );
    let registrations = logical.join("registrations");
    match fs::symlink_metadata(&registrations) {
        Ok(metadata) => ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "Hermes 内部登记目录不能是软链接，已保留"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => anyhow::bail!("Hermes 内部登记目录不可读，已保留"),
    }
    let name = record.file_name().context("缺少 Hermes 登记文件名")?;
    let logical_record = registrations.join(name);
    internal_file(&logical_record)?;
    ensure!(
        crate::cross_project::physical_entry(&logical_record)?
            == physical.join("registrations").join(name),
        "Hermes 登记落点超出实际插件目录，已保留"
    );
    for name in files {
        let file = logical.join(name);
        internal_file(&file)?;
        ensure!(
            crate::cross_project::physical_entry(&file)? == physical.join(name),
            "Hermes 共享适配文件落点已变化，已保留"
        );
    }
    Ok(())
}

fn record_path(profile: &Path, installation: &Path) -> PathBuf {
    let digest = format!(
        "{:x}",
        Sha256::digest(installation.as_os_str().as_encoded_bytes())
    );
    profile
        .join("plugins")
        .join(PLUGIN_ID)
        .join("registrations")
        .join(format!("{}.json", &digest[..24]))
}

fn rule(logical: PathBuf, physical: PathBuf, body: Vec<u8>, max: u64) -> Result<ManagedFileRule> {
    ManagedFileRule::new(logical, physical, body, max)
}

fn descriptor_encodings(value: &serde_json::Value) -> Result<(Vec<u8>, Vec<u8>)> {
    let entry = &value["skills"][0];
    let scalar = |key: &str| serde_json::to_string(&entry[key]);
    let canonical = (super::canonical_json(value.clone())? + "\n").into_bytes();
    // Exact insertion order of the original preserve_order producer. Never
    // derive this candidate by serializing a parsed map in the current build.
    let legacy = format!(concat!("{{\"schemaVersion\":1,\"manager\":\"pleat-native-hermes-v1\",\"skills\":[{{",
        "\"installationSkillPath\":{},\"canonicalSourceRoot\":{},\"sourceIdentity\":{},",
        "\"nativeName\":{},\"readerExecutable\":{}}}]}}\n"),
        scalar("installationSkillPath")?, scalar("canonicalSourceRoot")?, scalar("sourceIdentity")?,
        scalar("nativeName")?, scalar("readerExecutable")?).into_bytes();
    Ok((canonical, legacy))
}

fn compatible_descriptor(
    path: &Path,
    installation: &Path,
    source: &Path,
    identity: &str,
    name: &str,
) -> Result<Option<Vec<u8>>> {
    let Some((bytes, _)) = super::snapshot(path, MAX_DESCRIPTOR)? else {
        return Ok(None);
    };
    let Some(value) = serde_json::from_slice::<serde_json::Value>(&bytes).ok() else {
        return Ok(None);
    };
    let Some(locator) = value
        .pointer("/skills/0/canonicalSourceRoot")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
    else {
        return Ok(None);
    };
    let Some(reader) = value
        .pointer("/skills/0/readerExecutable")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
    else {
        return Ok(None);
    };
    // A different CLI/desktop reader may be retained only at a canonical file
    // path, matching current_exe().canonicalize() used by both generators.
    if !locator.is_absolute()
        || locator.canonicalize().ok().as_deref() != Some(source)
        || !reader.is_absolute()
        || !reader.is_file()
        || reader.canonicalize().ok().as_deref() != Some(reader.as_path())
        || reader
            .to_str()
            .is_none_or(|v| v.chars().any(char::is_control))
    {
        return Ok(None);
    }
    if value != descriptor(installation, &locator, identity, name, &reader) {
        return Ok(None);
    }
    let (canonical, legacy) = descriptor_encodings(&value)?;
    Ok((bytes == canonical || bytes == legacy).then_some(bytes))
}

fn descriptor(
    installation: &Path,
    source: &Path,
    identity: &str,
    name: &str,
    reader: &Path,
) -> serde_json::Value {
    serde_json::json!({"schemaVersion":1,"manager":"pleat-native-hermes-v1","skills":[{
        "installationSkillPath":installation.join("SKILL.md"),"canonicalSourceRoot":source,
        "sourceIdentity":identity,"nativeName":name,"readerExecutable":reader
    }]})
}

pub(super) fn plan(installation: &Path, source: &Path, install: bool) -> Result<Plan> {
    let home = dirs::home_dir().context("无法取得 Hermes 用户目录")?;
    let profile = crate::agents::hermes_home(&home)?;
    let mut plan = plan_at(&profile, installation, source, install)?;
    plan.profile_guard = Some((home, profile));
    Ok(plan)
}

fn plan_at(profile: &Path, installation: &Path, source: &Path, install: bool) -> Result<Plan> {
    let installation = if installation.is_absolute() {
        installation.to_owned()
    } else {
        std::env::current_dir()?.join(installation)
    };
    let name = installation
        .file_name()
        .and_then(|n| n.to_str())
        .context("Hermes 技能名称编码无效")?;
    crate::library::validate_name(name)?;
    ensure!(
        !name.chars().any(char::is_control),
        "Hermes 技能名称包含控制字符"
    );
    let source = source
        .canonicalize()
        .context("Hermes 技能源不可验证，已保留")?;
    ensure!(
        source.is_dir() && source.join("SKILL.md").is_file(),
        "Hermes 技能源缺少 SKILL.md"
    );
    let identity = crate::library::filesystem_identity(&source)?;
    let logical_plugin_root = profile.join("plugins").join(PLUGIN_ID);
    let plugin_root = crate::agents::hermes::hermes_directory_identity(&logical_plugin_root)?;
    let config_path = crate::cross_project::physical_entry(&profile.join("config.yaml"))?;
    ensure!(
        !plugin_root.starts_with(&source)
            && !source.starts_with(&plugin_root)
            && !config_path.starts_with(&source),
        "Hermes 原生适配入口与技能源重叠，已保留"
    );
    let logical = record_path(profile, &installation);
    internal_routes(&logical_plugin_root, &plugin_root, &logical, &["__init__.py", "adapter.py", "plugin.yaml"])?;
    let physical = plugin_root
        .join("registrations")
        .join(logical.file_name().context("缺少 Hermes 登记文件名")?);
    let reader = std::env::current_exe()?.canonicalize()?;
    let body = if let Some(existing) =
        compatible_descriptor(&physical, &installation, &source, &identity, name)?
    {
        existing
    } else {
        descriptor_encodings(&descriptor(
            &installation,
            &source,
            &identity,
            name,
            &reader,
        ))?.0
    };
    let files = [
        (
            "__init__.py",
            include_str!("../../assets/host-adapters/hermes-plugin-entry.py"),
        ),
        (
            "adapter.py",
            include_str!("../../assets/host-adapters/hermes-adaptation.py"),
        ),
        (
            "plugin.yaml",
            "name: pleat-native-skills\nversion: \"0.1.0\"\ndescription: Pleat native skill adaptation\nprovides_hooks:\n  - transform_tool_result\n  - on_session_delete\n",
        ),
    ];
    let config = opencode::ConfigPlan::new_hermes(profile, install)?;
    Plan::profile(logical_plugin_root, logical, body, &source, &files, config)
}

pub(super) fn existing(installation: &Path) -> Result<Option<Plan>> {
    let home = dirs::home_dir().context("无法取得 Hermes 用户目录")?;
    let profile = crate::agents::hermes_home(&home)?;
    let record = record_path(&profile, installation);
    match fs::symlink_metadata(&record) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => anyhow::bail!("Hermes 受管登记不可验证，已保留"),
    }
    match installation.canonicalize() {
        Ok(source) => {
            let mut plan = plan_at(&profile, installation, &source, false)?;
            plan.profile_guard = Some((home, profile));
            Ok(Some(plan))
        }
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && record_path(&profile, installation)
                    .symlink_metadata()
                    .is_err() =>
        {
            Ok(None)
        }
        Err(_) => anyhow::bail!("Hermes 安装来源不可验证，受管登记已保留"),
    }
}

impl Plan {
    /// Both hosts use exactly the same anchored publication, CAS, coalescing,
    /// last-registration removal and recovery behavior. Callers only supply
    /// their native file layout and metadata wire format.
    pub(super) fn profile(
        logical_plugin_root: PathBuf,
        logical_record: PathBuf,
        body: Vec<u8>,
        source: &Path,
        files: &[(&'static str, &str)],
        config: opencode::ConfigPlan,
    ) -> Result<Self> {
        let source = source.canonicalize().context("原生技能源不可验证，已保留")?;
        let plugin_root = crate::agents::hermes::hermes_directory_identity(&logical_plugin_root)?;
        ensure!(!plugin_root.starts_with(&source) && !source.starts_with(&plugin_root)
            && !config.path.starts_with(&source), "原生适配入口与技能源重叠，已保留");
        let internal_files = files.iter().map(|(name, _)| *name).collect::<Vec<_>>();
        ensure!(internal_files.len() <= 8 && internal_files.iter().all(|name| {
            let mut parts = Path::new(name).components();
            matches!(parts.next(), Some(std::path::Component::Normal(_))) && parts.next().is_none()
        }), "原生共享适配文件名无效");
        ensure!(logical_record.parent() == Some(logical_plugin_root.join("registrations").as_path()),
            "原生登记不在受管目录内");
        internal_routes(&logical_plugin_root, &plugin_root, &logical_record, &internal_files)?;
        let physical = plugin_root.join("registrations").join(logical_record.file_name().context("缺少原生登记名称")?);
        let mut descriptor = ManagedFileRule::new(logical_record, physical, body, MAX_DESCRIPTOR)?;
        descriptor.source_guard = Some((source.clone(), crate::library::filesystem_identity(&source)?));
        let core = files.iter().map(|(name, body)| {
            rule(logical_plugin_root.join(name), plugin_root.join(name), body.as_bytes().to_vec(), 256 * 1024)
                .map(|p| Arc::new(Mutex::new(p)))
        }).collect::<Result<Vec<_>>>()?;
        let path = descriptor.path.clone();
        let registrations = path.parent().context("缺少原生受管登记目录")?.to_owned();
        Ok(Self { path, descriptor, core, config, registrations, logical_plugin_root, plugin_root,
            internal_files, profile_guard: None })
    }
    pub(super) fn paths(&self) -> Vec<PathBuf> {
        let mut paths = vec![self.path.clone(), self.config.path.clone()];
        for p in &self.core {
            if let Ok(p) = p.lock() {
                paths.push(p.path.clone());
            }
        }
        paths.sort();
        paths.dedup();
        paths
    }

    fn lock_directory(&self) -> Result<crate::package_fs::Directory> {
        let root = &self.plugin_root;
        let mut parent = super::directory(&self.descriptor.anchor)?;
        ensure!(
            crate::library::filesystem_identity(&self.descriptor.anchor)?
                == self.descriptor.anchor_identity,
            "Hermes 适配目录锚点已变化，未执行写入"
        );
        for part in root.strip_prefix(&self.descriptor.anchor)?.components() {
            let name = Path::new(part.as_os_str());
            parent.mkdir(name)?;
            parent = parent.child(name)?;
        }
        parent.verify_location(root)?;
        Ok(parent)
    }
    pub(super) fn merge(&mut self, other: Self) -> Result<()> {
        ensure!(
            self.path == other.path
                && self.descriptor.body == other.descriptor.body
                && self.descriptor.expected == other.descriptor.expected,
            "同一 Hermes 原生适配登记内容冲突，已保留"
        );
        self.config.merge(other.config)
    }
    pub(super) fn coalesce(
        &mut self,
        files: &mut Pool,
        configs: &mut opencode::Pool,
    ) -> Result<()> {
        for file in &mut self.core {
            let current = file
                .lock()
                .map_err(|_| anyhow::anyhow!("原生适配状态不可用"))?;
            let path = current.path.clone();
            if let Some(existing) = files.get(&path) {
                if Arc::ptr_eq(file, existing) {
                    drop(current);
                    continue;
                }
                let previous = existing
                    .lock()
                    .map_err(|_| anyhow::anyhow!("原生适配状态不可用"))?;
                ensure!(
                    current.body == previous.body && current.expected == previous.expected,
                    "Hermes 共享适配入口在预览时变化"
                );
                drop(previous);
                drop(current);
                *file = existing.clone();
            } else {
                drop(current);
                files.insert(path, file.clone());
            }
        }
        self.config.coalesce(configs)
    }
    pub(super) fn needs_install(&self) -> bool {
        self.descriptor.needs_install()
            || self.config.needs_install()
            || self
                .core
                .iter()
                .any(|p| p.lock().map_or(true, |p| p.needs_install()))
    }
    pub(super) fn validate(&self) -> Result<()> {
        internal_routes(&self.logical_plugin_root, &self.plugin_root, &self.path, &self.internal_files)?;
        if let Some((source, _)) = &self.descriptor.source_guard {
            let source = source
                .canonicalize()
                .context("Hermes 技能源不可验证，已保留")?;
            ensure!(
                !self.plugin_root.starts_with(&source)
                    && !source.starts_with(&self.plugin_root)
                    && !self.config.path.starts_with(&source),
                "Hermes 原生适配入口与技能源重叠，已保留"
            );
        }
        if let Some((home, profile)) = &self.profile_guard {
            ensure!(
                crate::agents::hermes_home(home)? == *profile,
                "Hermes 当前 profile 已变化，请重新预览"
            );
        }
        self.descriptor.validate()?;
        self.config.validate()?;
        for p in &self.core {
            p.lock()
                .map_err(|_| anyhow::anyhow!("原生适配状态不可用"))?
                .validate()?;
        }
        Ok(())
    }
    pub(super) fn install(&mut self) -> Result<bool> {
        self.validate()?;
        let directory = self.lock_directory()?;
        let _lock = directory.lock(Path::new(".pleat-registration.lock"))?;
        self.validate()?;
        let mut changed = false;
        for p in &self.core {
            changed |= p
                .lock()
                .map_err(|_| anyhow::anyhow!("原生适配状态不可用"))?
                .install()?;
        }
        changed |= self.descriptor.install()?;
        changed |= self.config.install()?;
        Ok(changed)
    }
    fn has_other_records(&self) -> Result<bool> {
        if !self.registrations.exists() {
            return Ok(false);
        }
        let directory = super::directory(&self.registrations)?;
        let mut present = false;
        for (count, entry) in fs::read_dir(&self.registrations)?.enumerate() {
            ensure!(count < 4096, "Hermes 受管登记超过检查上限，保留共享入口");
            let entry = entry?;
            // Any other object keeps shared files/config. Never inspect or
            // remove another skill's data merely to clean a shared helper.
            if entry.path() != self.path {
                present = true;
                break;
            }
        }
        directory.verify_location(&self.registrations)?;
        Ok(present)
    }
    pub(super) fn detach(&mut self) -> Result<bool> {
        self.validate()?;
        let directory = self.lock_directory()?;
        let _lock = directory.lock(Path::new(".pleat-registration.lock"))?;
        self.validate()?;
        let mut changed = self.descriptor.detach()?;
        if !self.has_other_records()? {
            changed |= self.config.detach()?;
            for p in self.core.iter().rev() {
                changed |= p
                    .lock()
                    .map_err(|_| anyhow::anyhow!("原生适配状态不可用"))?
                    .detach()?;
            }
        }
        Ok(changed)
    }
}

#[cfg(all(test, unix, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let profile = root.join("profile");
        let source = root.join("source");
        let link = root.join("repo/.hermes/skills/probe");
        fs::create_dir_all(&profile).unwrap();
        fs::create_dir(&source).unwrap();
        fs::write(source.join("SKILL.md"), "Owned source").unwrap();
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&source, &link).unwrap();
        fs::write(profile.join("config.yaml"),"# keep header\nmodel:\n  default: 'unchanged/model' # keep model\nplugins:\n  enabled: [user-plugin]\n").unwrap();
        (temp, profile, source, link)
    }
    #[test]
    fn pooled_installs_remove_only_their_descriptor_and_keep_shared_plugin_until_last() {
        let (_temp, profile, source, first) = fixture();
        let second = first.with_file_name("second");
        std::os::unix::fs::symlink(&source, &second).unwrap();
        let mut a = plan_at(&profile, &first, &source, true).unwrap();
        let mut b = plan_at(&profile, &second, &source, true).unwrap();
        let mut files = Pool::new();
        let mut configs = opencode::Pool::new();
        a.coalesce(&mut files, &mut configs).unwrap();
        b.coalesce(&mut files, &mut configs).unwrap();
        assert!(a.install().unwrap());
        assert!(b.install().unwrap());
        let root = profile.join("plugins").join(PLUGIN_ID);
        assert!(a.path.is_file() && b.path.is_file());
        assert_eq!(a.paths().len(), 5);
        let config = fs::read_to_string(profile.join("config.yaml")).unwrap();
        assert!(config.contains("# keep model") && config.contains("user-plugin"));
        assert!(a.detach().unwrap());
        assert!(!a.path.exists() && b.path.is_file() && root.join("__init__.py").is_file());
        assert_eq!(
            fs::read_to_string(profile.join("config.yaml")).unwrap(),
            config
        );
        assert!(b.detach().unwrap());
        assert!(!root.join("__init__.py").exists());
        let final_config = fs::read_to_string(profile.join("config.yaml")).unwrap();
        assert!(final_config.contains("user-plugin") && !final_config.contains(PLUGIN_ID));
        assert_eq!(
            fs::read_to_string(source.join("SKILL.md")).unwrap(),
            "Owned source"
        );
    }
    #[test]
    fn config_or_source_changed_after_preview_prevents_any_published_plugin() {
        let (_temp, profile, source, link) = fixture();
        let mut p = plan_at(&profile, &link, &source, true).unwrap();
        fs::write(profile.join("config.yaml"), "model: user-change\n").unwrap();
        assert!(p.install().is_err());
        assert!(!p.path.exists());
        let mut p = plan_at(&profile, &link, &source, true).unwrap();
        fs::rename(&source, source.with_file_name("old-source")).unwrap();
        fs::create_dir(&source).unwrap();
        fs::write(source.join("SKILL.md"), "Concurrent source").unwrap();
        assert!(p.install().is_err());
        assert!(!p.path.exists());
    }
    #[test]
    fn foreign_plugin_is_protected_and_record_survives_storage_compatibility_move() {
        let (_temp, profile, source, link) = fixture();
        let mut first = plan_at(&profile, &link, &source, true).unwrap();
        first.install().unwrap();
        let bytes = fs::read(&first.path).unwrap();
        let moved = source.with_file_name("moved-source");
        fs::rename(&source, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &source).unwrap();
        let moved_plan = plan_at(&profile, &link, &moved, true).unwrap();
        assert_eq!(moved_plan.descriptor.body, bytes);
        let core = profile.join("plugins").join(PLUGIN_ID).join("adapter.py");
        fs::write(&core, "# user modified\n").unwrap();
        assert!(plan_at(&profile, &link, &moved, true).is_err());
        assert_eq!(fs::read_to_string(core).unwrap(), "# user modified\n");
    }
    #[test]
    fn native_plugin_alias_into_source_is_rejected_without_touching_source() {
        let (_temp, profile, source, link) = fixture();
        fs::create_dir(profile.join("plugins")).unwrap();
        std::os::unix::fs::symlink(&source, profile.join("plugins").join(PLUGIN_ID)).unwrap();
        assert!(plan_at(&profile, &link, &source, true).is_err());
        assert!(!source.join("__init__.py").exists());
        assert_eq!(
            fs::read_to_string(source.join("SKILL.md")).unwrap(),
            "Owned source"
        );
    }
    #[test]
    fn independent_registration_aliases_are_refused_before_any_write() {
        for destination in ["source", "local-data", "external"] {
            let (_temp, profile, source, link) = fixture();
            let root = profile.join("plugins").join(PLUGIN_ID);
            fs::create_dir_all(&root).unwrap();
            let target = match destination {
                "source" => source.clone(),
                "local-data" => source.join("local-data"),
                _ => source.with_file_name("external"),
            };
            fs::create_dir_all(&target).unwrap();
            fs::write(target.join("preserved"), "original").unwrap();
            std::os::unix::fs::symlink(&target, root.join("registrations")).unwrap();
            let config = fs::read(profile.join("config.yaml")).unwrap();
            let before = fs::read_dir(&target).unwrap().count();
            assert!(plan_at(&profile, &link, &source, true).is_err());
            assert_eq!(fs::read(profile.join("config.yaml")).unwrap(), config);
            assert_eq!(fs::read_dir(&target).unwrap().count(), before);
            assert_eq!(
                fs::read_to_string(target.join("preserved")).unwrap(),
                "original"
            );
            assert!(
                !root.join("__init__.py").exists()
                    && !root.join(".pleat-registration.lock").exists()
            );
            assert!(
                !target
                    .parent()
                    .unwrap()
                    .join(".pleat-registration.lock")
                    .exists()
            );
        }
    }
    #[test]
    fn registration_retarget_after_preview_cannot_write_source_or_create_wrong_lock() {
        let (_temp, profile, source, link) = fixture();
        let root = profile.join("plugins").join(PLUGIN_ID);
        let registrations = root.join("registrations");
        fs::create_dir_all(&registrations).unwrap();
        let mut plan = plan_at(&profile, &link, &source, true).unwrap();
        fs::rename(&registrations, root.join("original-registrations")).unwrap();
        std::os::unix::fs::symlink(&source, &registrations).unwrap();
        let config = fs::read(profile.join("config.yaml")).unwrap();
        assert!(plan.install().is_err());
        assert_eq!(fs::read(profile.join("config.yaml")).unwrap(), config);
        assert_eq!(fs::read_dir(&source).unwrap().count(), 1);
        assert!(!root.join(".pleat-registration.lock").exists());
    }
    #[test]
    fn profile_ancestor_alias_uses_one_actual_plugin_root_lock() {
        let (_temp, profile, source, link) = fixture();
        let alias = profile.with_file_name("profile-alias");
        std::os::unix::fs::symlink(&profile, &alias).unwrap();
        let mut first = plan_at(&alias, &link, &source, true).unwrap();
        first.install().unwrap();
        let second_link = link.with_file_name("second");
        std::os::unix::fs::symlink(&source, &second_link).unwrap();
        let mut second = plan_at(&profile, &second_link, &source, true).unwrap();
        assert_eq!(first.plugin_root, second.plugin_root);
        second.install().unwrap();
        assert!(
            profile
                .join("plugins")
                .join(PLUGIN_ID)
                .join(".pleat-registration.lock")
                .is_file()
        );
        first.detach().unwrap();
        assert!(second.path.is_file());
        second.detach().unwrap();
    }
    #[test]
    fn descriptor_file_alias_and_source_relocation_into_bundle_are_refused() {
        let (_temp, profile, source, link) = fixture();
        let record = record_path(&profile, &link);
        fs::create_dir_all(record.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(source.join("not-created"), &record).unwrap();
        assert!(plan_at(&profile, &link, &source, true).is_err());
        assert!(!source.join("not-created").exists());
        let (_temp, profile, source, link) = fixture();
        let mut plan = plan_at(&profile, &link, &source, true).unwrap();
        fs::create_dir(profile.join("plugins")).unwrap();
        fs::rename(&source, &plan.plugin_root).unwrap();
        std::os::unix::fs::symlink(&plan.plugin_root, &source).unwrap();
        assert!(plan.install().is_err());
        assert!(!plan.plugin_root.join("adapter.py").exists());
        assert_eq!(
            fs::read_to_string(source.join("SKILL.md")).unwrap(),
            "Owned source"
        );
    }
}
