use std::{
    sync::Mutex,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
use std::cell::RefCell;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::{
    package_fs::FileLock,
    AppConfig, SkillGroup, SkillOrigin, SkillRecord, SkillSource, SkillSourceRecord, SkillSummary,
    TriggerMode, config, trigger::TriggerSource,
};

pub(crate) mod storage;
pub use storage::{StorageAction, StoragePlan};
pub(crate) use storage::identity as filesystem_identity;

pub const KITTER_SKILL_STORAGE: &str = "_kitter-builtin";
const KITTER_SKILL_NAME: &str = "kitter";
const KITTER_SKILL_MD: &str = include_str!("../resources/skills/kitter/SKILL.md");
const KITTER_OPENAI_YAML: &str = include_str!("../resources/skills/kitter/agents/openai.yaml");
const KITTER_INSTALL_CLI_MD: &str =
    include_str!("../resources/skills/kitter/references/install-cli.md");

#[cfg(test)]
thread_local! {
    static BEFORE_UPDATE_COMMIT: RefCell<Option<fn(&Path, &Path, &Path)>> = RefCell::new(None);
    static AFTER_CONFIG_READ: RefCell<Option<fn(&Path)>> = RefCell::new(None);
    static BEFORE_STORAGE_OPEN_READ: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
    static AFTER_STORAGE_OPEN_READ: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    skills: HashMap<String, SkillRecord>,
    #[serde(default)]
    sources: HashMap<String, SkillSourceRecord>,
    #[serde(default)]
    groups: Vec<SkillGroup>,
    #[serde(default)]
    source_groups_migrated: bool,
    #[serde(default)]
    adopted_sources: HashMap<String, AdoptedSource>,
    #[serde(default)]
    trigger_overrides: HashMap<String, TriggerOverride>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    package_baselines: HashMap<String, crate::update_protection::Manifest>,
}

#[derive(Clone, Serialize, Deserialize)]
struct TriggerOverride {
    mode: TriggerMode,
    source: TriggerSource,
}

#[derive(Clone, Serialize, Deserialize)]
struct AdoptedSource {
    source: PathBuf,
    references: Vec<crate::adoption::SkillReference>,
    #[serde(default)]
    previous_library: Option<PathBuf>,
}

pub struct SkillLibrary {
    pub config: AppConfig,
    registry: Registry,
    data_dir: PathBuf,
    disk_revisions: Mutex<[Option<String>; 2]>,
    proma_sync_warnings: Vec<String>,
    project_observations: Mutex<Vec<crate::project_discovery::ProjectEntry>>,
}

fn lock_registry_file(path: &Path) -> Result<FileLock> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600).custom_flags(libc::O_NOFOLLOW); }
    let lock = options.open(path)?;
    lock.try_lock().map_err(|_| anyhow::anyhow!("存储正被其他操作使用，请稍后重试"))?;
    Ok(FileLock::from_locked(lock))
}

// This sibling lock protects root creation and rename until registry.lock can
// be acquired. A startup must not recreate the old root during a data move.
fn storage_parent_lock(data_dir: &Path) -> Result<FileLock> {
    let parent = data_dir.parent().context("应用数据目录缺少父目录")?;
    fs::create_dir_all(parent)?;
    lock_registry_file(&parent.join(".pleat-storage-root.lock"))
}

fn registry_file_lock(data_dir: &Path) -> Result<FileLock> {
    fs::create_dir_all(data_dir)?;
    lock_registry_file(&data_dir.join("registry.lock"))
}

impl SkillLibrary {
    pub fn open() -> Result<Self> {
        if std::env::var_os("KITTER_HOME").is_none() {
            config::validate_app_data_roots(&config::preferred_app_data_dir(), &config::legacy_app_data_dir())?;
        }
        Self::open_in(config::app_data_dir())
    }

    pub fn open_in(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let data_dir = data_dir.into();
        storage::ensure_no_pending(&data_dir)?;
        let _parent_lock = storage_parent_lock(&data_dir)?;
        let startup_lock = registry_file_lock(&data_dir)?;
        storage::ensure_no_pending(&data_dir)?;
        #[cfg(test)]
        BEFORE_STORAGE_OPEN_READ.with(|hook| { if let Some(callback) = hook.borrow_mut().take() { callback(); } });
        let config_bytes = read_optional(&data_dir.join("config.json"))?;
        #[cfg(test)]
        AFTER_STORAGE_OPEN_READ.with(|hook| { if let Some(callback) = hook.borrow_mut().take() { callback(); } });
        #[cfg(test)]
        AFTER_CONFIG_READ.with(|hook| {
            if let Some(callback) = hook.borrow_mut().take() { callback(&data_dir); }
        });
        let config: AppConfig = match &config_bytes {
            Some(bytes) => serde_json::from_slice(bytes).context("配置格式无效")?,
            None => AppConfig { library_dir: data_dir.join("skills"), ..AppConfig::default() },
        };
        // A transaction may have begun while the configuration was read.
        storage::ensure_no_pending(&data_dir)?;
        if !config.library_dir.exists() && data_dir.join(".pleat-storage-recovery").symlink_metadata().is_ok() {
            bail!("技能库缺失且存在存储恢复材料，未新建空库；请检查 {}", data_dir.join(".pleat-storage-recovery").display());
        }
        fs::create_dir_all(&config.library_dir)?;
        ensure_builtin_skill(&config.library_dir)?;
        let path = data_dir.join("registry.json");
        let registry_bytes = read_optional(&path)?;
        let disk_revisions = Mutex::new([
            config_bytes.as_deref().map(bytes_revision), registry_bytes.as_deref().map(bytes_revision),
        ]);
        let mut registry: Registry = match &registry_bytes {
            Some(bytes) => serde_json::from_slice(bytes)?,
            None => Registry::default(),
        };
        let mut changed = false;
        let builtin = registry
            .skills
            .entry(KITTER_SKILL_STORAGE.to_string())
            .or_insert_with(|| {
                changed = true;
                SkillRecord {
                    name: KITTER_SKILL_NAME.to_string(),
                    storage_name: KITTER_SKILL_STORAGE.to_string(),
                    description: String::new(),
                    origin: SkillOrigin::Builtin,
                    update_available: false,
                    group_id: None,
                    last_operated_at: u64::MAX,
                }
            });
        if builtin.name != KITTER_SKILL_NAME
            || builtin.storage_name != KITTER_SKILL_STORAGE
            || !builtin.origin.is_builtin()
            || builtin.update_available
            || builtin.group_id.is_some()
            || builtin.last_operated_at != u64::MAX
        {
            builtin.name = KITTER_SKILL_NAME.to_string();
            builtin.storage_name = KITTER_SKILL_STORAGE.to_string();
            builtin.origin = SkillOrigin::Builtin;
            builtin.update_available = false;
            builtin.group_id = None;
            builtin.last_operated_at = u64::MAX;
            changed = true;
        }
        for (storage_name, record) in registry.skills.iter_mut() {
            if record.storage_name.is_empty() {
                record.storage_name = storage_name.clone();
                changed = true;
            }
        }
        for record in registry.skills.values() {
            let source = record.origin.source();
            let source_record = registry.sources.entry(source.key()).or_insert_with(|| {
                changed = true;
                SkillSourceRecord {
                    source: source.clone(),
                    discovered_skills: Vec::new(),
                    added_skills: Vec::new(),
                }
            });
            if !source_record.discovered_skills.contains(&record.name) {
                source_record.discovered_skills.push(record.name.clone());
                changed = true;
            }
            if !source_record.added_skills.contains(&record.name) {
                source_record.added_skills.push(record.name.clone());
                changed = true;
            }
        }
        if !registry.source_groups_migrated {
            if registry.groups.is_empty() {
                let mut source_skills = BTreeMap::<String, (SkillSource, Vec<String>)>::new();
                for (storage_name, record) in &registry.skills {
                    let source = record.origin.source();
                    let source_key = source.key();
                    source_skills
                        .entry(source_key)
                        .or_insert_with(|| (source.clone(), Vec::new()))
                        .1
                        .push(storage_name.clone());
                }
                let mut used_names = HashSet::new();
                for (source_key, (source, storage_names)) in source_skills {
                    if source_key == "unknown" || storage_names.len() < 2 {
                        continue;
                    }
                    let name = unique_group_name(&source.label(), &mut used_names);
                    let id = migrated_group_id(&source_key, &registry.groups);
                    registry.groups.push(SkillGroup {
                        id: id.clone(),
                        name,
                        created_at: operation_stamp(),
                    });
                    for storage_name in storage_names {
                        if let Some(record) = registry.skills.get_mut(&storage_name) {
                            record.group_id = Some(id.clone());
                        }
                    }
                }
            }
            registry.source_groups_migrated = true;
            changed = true;
        }
        let group_ids = registry
            .groups
            .iter()
            .map(|group| group.id.as_str())
            .collect::<HashSet<_>>();
        for record in registry.skills.values_mut() {
            if record
                .group_id
                .as_deref()
                .is_some_and(|group_id| !group_ids.contains(group_id))
            {
                record.group_id = None;
                changed = true;
            }
        }
        if changed {
            for source in registry.sources.values_mut() {
                source.discovered_skills.sort();
                source.added_skills.sort();
            }
        }
        if let Some(override_entry) = registry.trigger_overrides.get_mut(KITTER_SKILL_STORAGE) {
            let builtin_source = TriggerSource {
                skill_md: KITTER_SKILL_MD.to_string(),
                openai_yaml: Some(KITTER_OPENAI_YAML.to_string()),
            };
            if override_entry.source != builtin_source {
                override_entry.source = builtin_source;
                changed = true;
            }
            override_entry
                .source
                .render(override_entry.mode)?
                .write(&config.library_dir.join(KITTER_SKILL_STORAGE))?;
        }
        let library = Self {
            config,
            registry,
            data_dir,
            disk_revisions,
            proma_sync_warnings: Vec::new(),
            project_observations: Mutex::new(Vec::new()),
        };
        if changed {
            library.save_locked(&startup_lock)?;
        }
        Ok(library)
    }

    pub fn save(&self) -> Result<()> {
        let lock = self.write_lock()?;
        self.save_locked(&lock)
    }

    fn verify_disk_revisions(&self) -> Result<()> {
        storage::ensure_no_pending(&self.data_dir)?;
        let paths = [self.data_dir.join("config.json"), self.data_dir.join("registry.json")];
        for (path, expected) in paths.iter().zip(self.disk_revisions.lock().map_err(|_| anyhow::anyhow!("登记版本锁不可用"))?.iter()) {
            if file_revision(path)? != *expected {
                bail!("Kitter 登记或配置已被其他操作修改，请重新读取后再执行");
            }
        }
        Ok(())
    }

    fn write_lock(&self) -> Result<FileLock> {
        let _parent_lock = storage_parent_lock(&self.data_dir)?;
        let lock = registry_file_lock(&self.data_dir)?;
        self.verify_disk_revisions()?;
        Ok(lock)
    }

    fn save_locked(&self, _lock: &fs::File) -> Result<()> {
        self.verify_disk_revisions()?;
        let paths = [self.data_dir.join("config.json"), self.data_dir.join("registry.json")];
        let old = paths.iter().map(|p| read_optional(p)).collect::<Result<Vec<_>>>()?;
        let values = [serde_json::to_vec_pretty(&self.config)?, serde_json::to_vec_pretty(&self.registry)?];
        for (index, path) in paths.iter().enumerate() {
            if let Err(error) = write_json_bytes(path, &values[index]) {
                for done in (0..index).rev() {
                    if file_revision(&paths[done])? != Some(bytes_revision(&values[done])) {
                        bail!("保存失败且文件发生并发修改，未覆盖并发版本：{error}");
                    }
                    match &old[done] {
                        Some(bytes) => write_json_bytes(&paths[done], bytes)?,
                        None => fs::remove_file(&paths[done])?,
                    }
                }
                return Err(error);
            }
        }
        *self.disk_revisions.lock().map_err(|_| anyhow::anyhow!("登记版本锁不可用"))? = [Some(bytes_revision(&values[0])), Some(bytes_revision(&values[1]))];
        Ok(())
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn take_proma_sync_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.proma_sync_warnings)
    }

    pub fn trigger_mode_by_storage(&self, storage_name: &str) -> TriggerMode {
        self.registry
            .trigger_overrides
            .get(storage_name)
            .map(|entry| entry.mode)
            .unwrap_or_default()
    }

    pub(crate) fn trigger_source_by_storage(&self, storage_name: &str) -> Option<&TriggerSource> {
        self.registry
            .trigger_overrides
            .get(storage_name)
            .map(|entry| &entry.source)
    }

    pub fn set_trigger_mode_by_storage(
        &mut self,
        storage_name: &str,
        mode: TriggerMode,
    ) -> Result<()> {
        let write_lock = self.write_lock()?;
        let path = self.skill_path_by_storage(storage_name)?;
        if self.is_linked_source(storage_name) {
            bail!("此技能暂不支持更改触发时机");
        }
        let current = crate::update_protection::snapshot(&path)?;
        let baseline = self.registry.package_baselines.get(storage_name).cloned();
        let openai_yaml_path = Path::new("agents").join("openai.yaml");
        let trigger_files = ["SKILL.md", openai_yaml_path.to_str().context("触发文件路径不是 UTF-8")?];
        if !self.record_by_storage(storage_name)?.origin.is_builtin() {
            let base = baseline.as_ref().context("缺少文件基线，无法安全修改技能触发设置")?;
            for name in trigger_files {
                if current.get(name) != base.get(name) {
                    bail!("技能触发文件存在本机修改，未覆盖：{name}");
                }
            }
        }
        for name in trigger_files {
            if current.get(name).is_some_and(|node| !matches!(node, crate::update_protection::Node::File { .. })) {
                bail!("技能触发文件类型已改变，未覆盖：{name}");
            }
        }
        if current.get("agents").is_some_and(|node| !matches!(node, crate::update_protection::Node::Directory { .. })) {
            bail!("技能配置父目录不是实体目录，未覆盖");
        }
        let before = TriggerSource::read(&path)?;
        if path.join("local-data/ADAPTATION.md").is_file() {
            let previous = self.registry.trigger_overrides.get(storage_name).cloned();
            if mode == TriggerMode::FollowSkill {
                self.registry.trigger_overrides.remove(storage_name);
            } else {
                self.registry.trigger_overrides.insert(storage_name.to_owned(), TriggerOverride { mode, source: before });
            }
            let result = crate::update_protection::verify_unchanged(&path, &current)
                .and_then(|_| self.save_locked(&write_lock));
            if result.is_err() {
                if let Some(previous) = previous { self.registry.trigger_overrides.insert(storage_name.to_owned(), previous); }
                else { self.registry.trigger_overrides.remove(storage_name); }
            }
            return result;
        }
        let source = self
            .registry
            .trigger_overrides
            .get(storage_name)
            .map(|entry| entry.source.clone())
            .unwrap_or_else(|| before.clone());
        let rendered = source.render(mode)?;
        let expected_trigger = tempfile::tempdir()?;
        rendered.write(expected_trigger.path())?;
        let mut expected_files = crate::update_protection::snapshot(expected_trigger.path())?;
        for name in trigger_files {
            if let Some(crate::update_protection::Node::File { mode, .. }) = current.get(name) {
                if let Some(crate::update_protection::Node::File { mode: expected, .. }) = expected_files.get_mut(name) {
                    *expected = *mode;
                }
            }
        }
        let previous = self.registry.trigger_overrides.get(storage_name).cloned();
        let result = (|| -> Result<()> {
            crate::update_protection::verify_unchanged(&path, &current)?;
            rendered.write(&path)?;
            if mode == TriggerMode::FollowSkill {
                self.registry.trigger_overrides.remove(storage_name);
            } else {
                self.registry
                    .trigger_overrides
                    .insert(storage_name.to_string(), TriggerOverride { mode, source });
            }
            let after = crate::update_protection::snapshot(&path)?;
            for name in trigger_files {
                if after.get(name) != expected_files.get(name) {
                    bail!("触发设置写入期间文件发生修改，已停止登记：{name}");
                }
            }
            if let Some(base) = self.registry.package_baselines.get_mut(storage_name) {
                for name in trigger_files {
                    if let Some(node) = expected_files.get(name) { base.insert(name.into(), node.clone()); }
                    else { base.remove(name); }
                }
                if current.get("agents").is_none() {
                    if let Some(node) = after.get("agents") { base.insert("agents".into(), node.clone()); }
                }
            }
            self.save_locked(&write_lock)
        })();
        if let Err(error) = result {
            let after = crate::update_protection::snapshot(&path)?;
            if trigger_files.iter().any(|name| {
                after.get(*name) != expected_files.get(*name) && after.get(*name) != current.get(*name)
            }) {
                // Preserve an external edit instead of overwriting it during rollback.
                if let Some(base) = baseline { self.registry.package_baselines.insert(storage_name.to_owned(), base); }
                if let Some(previous) = previous { self.registry.trigger_overrides.insert(storage_name.to_owned(), previous); }
                else { self.registry.trigger_overrides.remove(storage_name); }
                bail!("触发设置失败且文件发生并发修改，已保留当前文件，请检查：{error}");
            }
            before.write(&path)?;
            if let Some(base) = baseline { self.registry.package_baselines.insert(storage_name.to_owned(), base); }
            if let Some(previous) = previous {
                self.registry
                    .trigger_overrides
                    .insert(storage_name.to_string(), previous);
            } else {
                self.registry.trigger_overrides.remove(storage_name);
            }
            return Err(error);
        }
        Ok(())
    }

    /// Reapply the requested native fields even when the recorded mode is the
    /// same, so a source update cannot make a retry silently skip reconciliation.
    pub fn apply_trigger_mode_by_storage(
        &mut self,
        storage_name: &str,
        mode: TriggerMode,
    ) -> Result<bool> {
        let previous = self.trigger_mode_by_storage(storage_name);
        self.set_trigger_mode_by_storage(storage_name, mode)?;
        Ok(previous != mode)
    }

    pub fn list(&self) -> Result<Vec<SkillSummary>> {
        // Refresh native path projections once per list, not once per Skill.
        // Feed back observations so an unreadable source retains its own rows.
        let projects = {
            let mut observed = self.project_observations.lock()
                .map_err(|_| anyhow::anyhow!("项目观察锁不可用"))?;
            if let Ok(current) = crate::project_discovery::refresh_agent_projects(&observed) {
                *observed = current;
            }
            self.config.project_entries(&observed)
        };
        let mut result = vec![];
        for entry in fs::read_dir(&self.config.library_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() || !path.join("SKILL.md").is_file() {
                continue;
            }
            let storage_name = entry.file_name().to_string_lossy().into_owned();
            let (_, description) = read_frontmatter(&path.join("SKILL.md")).unwrap_or_default();
            let mut record = self
                .registry
                .skills
                .get(&storage_name)
                .cloned()
                .unwrap_or_else(|| SkillRecord {
                    name: storage_name.clone(),
                    storage_name: storage_name.clone(),
                    description: String::new(),
                    origin: SkillOrigin::Unknown,
                    update_available: false,
                    group_id: None,
                    last_operated_at: 0,
                });
            if record.storage_name.is_empty() {
                record.storage_name = storage_name.clone();
            }
            record.description = description.unwrap_or_default();
            result.push(SkillSummary {
                installed_projects: Self::install_count(&record.name, &path, &projects),
                record,
                manual_only: crate::effective_skills::is_manual_skill(&path),
                path,
            });
        }
        result.sort_by(|a, b| {
            b.record
                .origin
                .is_builtin()
                .cmp(&a.record.origin.is_builtin())
                .then_with(|| a.record.name.cmp(&b.record.name))
        });
        Ok(result)
    }

    pub(crate) fn local_source_paths(&self) -> BTreeSet<PathBuf> {
        self.registry.skills.iter().filter_map(|(storage, record)| {
            (matches!(record.origin, SkillOrigin::Local { .. }) || self.is_linked_source(storage))
                .then(|| self.config.library_dir.join(storage))
        }).collect()
    }

    fn install_count(
        name: &str,
        source: &Path,
        projects: &[crate::project_discovery::ProjectEntry],
    ) -> usize {
        // Visibility/archive flags do not uninstall actual links or copies.
        projects.iter()
            .filter(|project| crate::project::is_installed_any_from_path(&project.path, name, source))
            .count()
    }

    pub fn skill_path(&self, name: &str) -> Result<PathBuf> {
        Ok(self.resolve_skill(name)?.path)
    }

    /// Resolve a user-facing Skill selector. Display names must be unique;
    /// `id:<storage-name>` explicitly selects one of several same-named Skills.
    pub fn resolve_skill(&self, selector: &str) -> Result<SkillSummary> {
        if let Some(storage_name) = selector.strip_prefix("id:") {
            validate_name(storage_name)?;
            return self
                .list()?
                .into_iter()
                .find(|skill| skill.record.storage_name == storage_name)
                .with_context(|| format!("没有找到技能：{selector}"));
        }
        validate_name(selector)?;
        let skills = self.list()?;
        let matches = skills
            .into_iter()
            .filter(|skill| skill.record.name == selector)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [skill] => Ok(skill.clone()),
            [] => bail!("没有找到技能：{selector}"),
            _ => bail!("存在多个同名技能，请使用 id:<值> 明确选择"),
        }
    }

    pub fn skill_path_by_storage(&self, storage_name: &str) -> Result<PathBuf> {
        validate_name(storage_name)?;
        let path = self.config.library_dir.join(storage_name);
        if !path.join("SKILL.md").is_file() {
            bail!("没有找到技能：{storage_name}");
        }
        Ok(path)
    }

    pub fn files(&self, name: &str) -> Result<Vec<PathBuf>> {
        let root = self.skill_path(name)?;
        self.files_in(&root)
    }

    pub fn files_by_storage(&self, storage_name: &str) -> Result<Vec<PathBuf>> {
        let root = self.skill_path_by_storage(storage_name)?;
        self.files_in(&root)
    }

    fn files_in(&self, root: &Path) -> Result<Vec<PathBuf>> {
        let mut files = WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| entry.depth() == 0 || entry.file_name() != "local-data")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .filter_map(|entry| entry.path().strip_prefix(&root).ok().map(Path::to_path_buf))
            .collect::<Vec<_>>();
        files.sort();
        Ok(files)
    }

    pub fn read_file(&self, name: &str, relative: &Path) -> Result<String> {
        let root = self.skill_path(name)?.canonicalize()?;
        self.read_file_in(&root, relative)
    }

    pub fn read_file_by_storage(&self, storage_name: &str, relative: &Path) -> Result<String> {
        let root = self.skill_path_by_storage(storage_name)?.canonicalize()?;
        self.read_file_in(&root, relative)
    }

    fn read_file_in(&self, root: &Path, relative: &Path) -> Result<String> {
        if relative
            .components()
            .any(|part| part.as_os_str() == "local-data")
        {
            bail!("本机数据不提供技能内容预览");
        }
        let path = root.join(relative).canonicalize()?;
        if !path.starts_with(&root) {
            bail!("文件不在技能目录内");
        }
        if path
            .strip_prefix(root)?
            .components()
            .any(|part| part.as_os_str() == "local-data")
        {
            bail!("本机数据不提供技能内容预览");
        }
        fs::read_to_string(path).context("该文件不是可预览的文本文件")
    }

    pub fn import(&mut self, source: &Path, mut record: SkillRecord) -> Result<()> {
        let write_lock = self.write_lock()?;
        if !fs::symlink_metadata(source.join("SKILL.md"))?.is_file() {
            bail!("所选目录中没有 SKILL.md");
        }
        validate_name(&record.name)?;
        let local_source = if matches!(record.origin, SkillOrigin::Local { .. }) {
            let source = source.canonicalize()?;
            if let SkillOrigin::Local { path, .. } = &mut record.origin {
                *path = source.clone();
            }
            Some(source)
        } else {
            None
        };
        let identity = record.identity_key();
        if self
            .registry
            .skills
            .values()
            .any(|existing| existing.identity_key() == identity)
        {
            bail!("这个来源中的技能已添加：{}", record.name);
        }
        let storage_name = self.storage_name_for(&record.name, &identity);
        let destination = self.config.library_dir.join(&storage_name);
        if let Some(source) = &local_source {
            crate::directory_link::create(source, &destination)?;
            record.update_available = false;
        } else {
            copy_tree(source, &destination)?;
        }
        record.storage_name = storage_name.clone();
        if record.last_operated_at == 0 {
            record.last_operated_at = operation_stamp();
        }
        if record.group_id.as_deref().is_some_and(|group_id| {
            !self
                .registry
                .groups
                .iter()
                .any(|group| group.id == group_id)
        }) {
            record.group_id = None;
        }
        let before = self.registry.clone();
        self.registry.trigger_overrides.remove(&storage_name);
        if local_source.is_none() {
            self.registry.package_baselines.insert(
                storage_name.clone(), crate::update_protection::snapshot(&destination)?,
            );
        }
        self.registry.skills.insert(storage_name, record);
        if let Err(error) = self.save_locked(&write_lock) {
            self.registry = before;
            if let Some(link) = crate::directory_link::inspect(&destination)? {
                crate::directory_link::remove(&destination, link)?;
            } else {
                archive_directory(&destination, &self.config.library_dir)?;
            }
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn contains_identity(&self, identity: &str) -> bool {
        self.registry
            .skills
            .values()
            .any(|record| record.identity_key() == identity)
    }

    /// Adopt one source and its observed links as a single recoverable operation.
    /// Source files and native plugin registries are never written here.
    pub fn adopt(
        &mut self,
        candidate: &crate::adoption::AdoptionCandidate,
        references: &[crate::adoption::SkillReference],
    ) -> Result<String> {
        let write_lock = self.write_lock()?;
        use crate::{adoption, directory_link};
        candidate.verify()?;
        if let Some(issue) = &candidate.issue {
            bail!("{issue}");
        }
        validate_name(&candidate.name)?;
        let mut seen = HashSet::new();
        let references = references
            .iter()
            .filter(|r| seen.insert(crate::project::installation_key(&r.path)))
            .cloned()
            .collect::<Vec<_>>();
        for reference in &references {
            adoption::validate_reference(reference)?;
        }
        let before = self.registry.clone();
        let identity = candidate.identity();
        let existing = self
            .registry
            .skills
            .values()
            .find(|r| r.identity_key() == identity)
            .cloned();
        let storage = existing
            .as_ref()
            .map(|r| r.storage_name.clone())
            .unwrap_or_else(|| self.storage_name_for(&candidate.name, &identity));
        if self.registry.trigger_overrides.contains_key(&storage) {
            bail!("先将技能的触发时机设为跟随技能");
        }
        let destination = self.config.library_dir.join(&storage);
        let already_points_here =
            destination.canonicalize().ok().as_ref() == Some(&candidate.source);
        let mut backup = None;
        let mut created = false;
        let mut changed = Vec::new();
        let result = (|| -> Result<()> {
            // Resolve all snapshots before changing anything. A link can point through
            // another reference or through the library entry that this operation switches.
            for reference in &references {
                if reference.kind != adoption::ReferenceKind::Link {
                    continue;
                }
                if crate::project::installation_key(&reference.path) == candidate.source {
                    continue;
                }
                let current =
                    directory_link::inspect(&reference.path)?.context("引用已变化，请重新扫描")?;
                if Some(&current.target) != reference.original_target.as_ref() {
                    bail!("引用已变化，请重新扫描：{}", reference.path.display());
                }
                if reference.original_target.as_ref() != Some(&candidate.source) {
                    adoption::replace_link(&reference.path, &candidate.source)?;
                    changed.push(reference.clone());
                }
                if reference.path.canonicalize()? != candidate.source {
                    bail!("引用验证失败：{}", reference.path.display());
                }
            }
            if !already_points_here {
                if destination.symlink_metadata().is_ok() {
                    // Keep an existing Kitter materialization recoverable, never overwrite it.
                    let parent = self.config.library_dir.join(".adoption-backups");
                    fs::create_dir_all(&parent)?;
                    let dir = tempfile::Builder::new()
                        .prefix("source-")
                        .tempdir_in(&parent)?
                        .keep();
                    let old = dir.join(&storage);
                    fs::rename(&destination, &old)?;
                    backup = Some(old);
                }
                directory_link::create(&candidate.source, &destination)?;
                created = true;
            }
            let mut record = candidate.record();
            record.storage_name = storage.clone();
            record.group_id = existing.as_ref().and_then(|r| r.group_id.clone());
            record.last_operated_at = operation_stamp();
            let source = record.origin.source();
            let source_record = self
                .registry
                .sources
                .entry(source.key())
                .or_insert_with(|| SkillSourceRecord {
                    source: source.clone(),
                    discovered_skills: Vec::new(),
                    added_skills: Vec::new(),
                });
            for list in [
                &mut source_record.discovered_skills,
                &mut source_record.added_skills,
            ] {
                if !list.contains(&record.name) {
                    list.push(record.name.clone());
                    list.sort();
                }
            }
            self.registry.skills.insert(storage.clone(), record);
            self.registry.package_baselines.remove(&storage);
            let mut owned = self
                .registry
                .adopted_sources
                .get(&storage)
                .map(|s| s.references.clone())
                .unwrap_or_default();
            for mut reference in references.clone() {
                if reference.kind != adoption::ReferenceKind::Link {
                    continue;
                }
                reference.source = candidate.source.clone();
                reference.original_target = Some(candidate.source.clone());
                owned.retain(|r| {
                    crate::project::installation_key(&r.path)
                        != crate::project::installation_key(&reference.path)
                });
                owned.push(reference);
            }
            self.registry.adopted_sources.insert(
                storage.clone(),
                AdoptedSource {
                    source: candidate.source.clone(),
                    references: owned,
                    previous_library: backup.clone().or_else(|| {
                        before
                            .adopted_sources
                            .get(&storage)
                            .and_then(|s| s.previous_library.clone())
                    }),
                },
            );
            self.save_locked(&write_lock)?;
            Ok(())
        })();
        if let Err(error) = result {
            let mut failures = adoption::rollback_links(&changed);
            if created {
                if let Ok(Some(link)) = directory_link::inspect(&destination) {
                    if let Err(e) = directory_link::remove(&destination, link) {
                        failures.push(e.to_string());
                    }
                }
            }
            if let Some(path) = &backup {
                if let Err(e) = fs::rename(path, &destination) {
                    failures.push(e.to_string());
                }
            }
            self.registry = before;
            if failures.is_empty() {
                return Err(error);
            }
            bail!("{error}；回滚未完成：{}", failures.join("；"));
        }
        Ok(storage)
    }

    pub fn is_linked_source(&self, storage_name: &str) -> bool {
        crate::directory_link::inspect(&self.config.library_dir.join(storage_name))
            .ok()
            .flatten()
            .is_some()
    }

    pub fn replace(&mut self, source: &Path, record: SkillRecord) -> Result<()> {
        let storage_name = if record.storage_name.is_empty() {
            record.name.clone()
        } else {
            record.storage_name.clone()
        };
        self.replace_by_storage(source, storage_name, record)
    }

    pub(crate) fn has_package_baseline(&self, storage_name: &str) -> bool {
        self.registry.package_baselines.contains_key(storage_name)
    }

    pub fn replace_by_storage(
        &mut self,
        source: &Path,
        storage_name: String,
        mut record: SkillRecord,
    ) -> Result<()> {
        let write_lock = self.write_lock()?;
        if self.is_linked_source(&storage_name) {
            bail!("此技能链接到原始目录，请在来源中更新");
        }
        if !fs::symlink_metadata(source.join("SKILL.md"))?.is_file() {
            bail!("更新来源中没有 SKILL.md");
        }
        let affected_projects = self
            .config
            .recent_projects
            .iter()
            .filter(|project| {
                crate::project::is_installed_any_from_path(
                    project,
                    &record.name,
                    &self.config.library_dir.join(&storage_name),
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        let destination = self.skill_path_by_storage(&storage_name)?;
        let backup = destination.with_extension("kitter-backup");
        let staging = destination.with_extension("kitter-staging");
        if path_entry_exists(&backup)? || path_entry_exists(&staging)? {
            bail!(
                "存在上次更新的恢复或准备目录，请先检查：{}",
                backup.display()
            );
        }
        if protected_data_nodes(&destination)?
            .iter()
            .any(|p| p != Path::new("local-data") && p != Path::new(".git"))
        {
            bail!("技能包含嵌套 local-data 或 .git，请先检查其归属再更新");
        }
        let baseline = self.registry.package_baselines.get(&storage_name)
            .context("缺少已安装版本的文件基线，无法确认本机修改，已停止更新")?.clone();
        let current = crate::update_protection::snapshot(&destination)?;
        // Prepare code before touching the live package. Machine data stays at its
        // original path, including its directory inode and open file handles.
        if let Err(error) = copy_tree(source, &staging) {
            if path_entry_exists(&staging)? {
                archive_directory(&staging, &self.config.library_dir)?;
            }
            return Err(error);
        }
        if !fs::symlink_metadata(staging.join("SKILL.md"))?.is_file() {
            archive_directory(&staging, &self.config.library_dir)?;
            bail!("更新来源的 SKILL.md 不是可复制的实体文件，未替换技能");
        }
        let previous_registry = self.registry.clone();
        let previous_config = self.config.clone();
        let mut moved_old = Vec::new();
        let mut moved_new = Vec::new();
        let result = (|| -> Result<()> {
            if let Some(override_entry) = self.registry.trigger_overrides.get_mut(&storage_name) {
                override_entry.source = TriggerSource::read(&staging)?;
                // Native trigger policy also applies to adapted managed skills.
                // The original upstream entry stays in TriggerOverride for FollowSkill.
                override_entry.source.render(override_entry.mode)?.write(&staging)?;
            }
            let incoming = crate::update_protection::prepare(&destination, &staging, &baseline, &current)?;
            let prepared = crate::update_protection::snapshot(&staging)?;
            crate::update_protection::verify_unchanged(&destination, &current)?;
            fs::create_dir(&backup)?;
            for entry in fs::read_dir(&destination)? {
                let entry = entry?;
                let name = entry.file_name();
                if name == "local-data" || name == ".git" {
                    continue;
                }
                crate::package_fs::rename_exclusive(&entry.path(), &backup.join(&name))?;
                moved_old.push(name);
            }
            crate::update_protection::verify_unchanged(&backup, &current)?;
            if !protected_data_nodes(&backup)?.is_empty() {
                bail!("原件出现新的本机数据目录，已停止更新并恢复原位");
            }
            for entry in fs::read_dir(&staging)? {
                let entry = entry?;
                let name = entry.file_name();
                if path_entry_exists(&destination.join(&name))? {
                    bail!("更新期间代码位置被占用：{}", name.to_string_lossy());
                }
                crate::package_fs::rename_exclusive(&entry.path(), &destination.join(&name))?;
                moved_new.push(name);
            }
            crate::update_protection::verify_unchanged(&backup, &current)?;
            crate::update_protection::verify_unchanged(&destination, &prepared)?;
            record.storage_name = storage_name.clone();
            record.last_operated_at = operation_stamp();
            self.registry.package_baselines.insert(storage_name.clone(), incoming);
            self.registry.skills.insert(storage_name, record);
            for project in affected_projects {
                self.config.touch_project(&project);
            }
            #[cfg(test)]
            BEFORE_UPDATE_COMMIT.with(|hook| {
                if let Some(callback) = hook.borrow_mut().take() {
                    callback(&destination, &backup, &self.data_dir);
                }
            });
            crate::update_protection::verify_unchanged(&backup, &current)?;
            crate::update_protection::verify_unchanged(&destination, &prepared)?;
            if !protected_data_nodes(&backup)?.is_empty() {
                bail!("原件出现新的本机数据目录，已停止更新并恢复原位");
            }
            self.save_locked(&write_lock)
        })();
        if let Err(error) = result {
            self.registry = previous_registry;
            self.config = previous_config;
            // Keep all new and old entries; never recursively delete a directory
            // that may have received data from another process during the update.
            for name in moved_new.into_iter().rev() {
                crate::package_fs::rename_exclusive(&destination.join(&name), &staging.join(&name))
                    .context("更新失败；移回新代码失败，已保留恢复目录")?;
            }
            for name in moved_old.into_iter().rev() {
                if path_entry_exists(&destination.join(&name))? {
                    bail!(
                        "更新失败；还原位置被占用，已保留恢复目录：{}",
                        backup.display()
                    );
                }
                crate::package_fs::rename_exclusive(&backup.join(&name), &destination.join(&name))
                    .context("更新失败；还原代码失败，已保留恢复目录")?;
            }
            if path_entry_exists(&backup)? {
                archive_directory(&backup, &self.config.library_dir)?;
            }
            archive_directory(&staging, &self.config.library_dir)?;
            return Err(error);
        }
        archive_directory(&backup, &self.config.library_dir)?;
        archive_directory(&staging, &self.config.library_dir)?;
        self.proma_sync_warnings.extend(crate::proma::sync_existing_source(&destination));
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Result<()> {
        let record = self.record(name)?;
        self.remove_by_storage(&record.storage_name)
    }

    pub fn remove_by_storage(&mut self, storage_name: &str) -> Result<()> {
        let write_lock = self.write_lock()?;
        let record = self.record_by_storage(storage_name)?;
        if record.origin.is_builtin() {
            bail!("Kitter 内置 Skill 不能删除");
        }
        let source_key = record.origin.source().key();
        let path = self.skill_path_by_storage(storage_name)?;
        if crate::directory_link::inspect(&path)?.is_none()
            && !protected_data_nodes(&path)?.is_empty()
        {
            bail!(
                "此技能包含本机数据，请先迁出 local-data 后再删除：{}",
                path.display()
            );
        }
        let affected_projects = self
            .config
            .recent_projects
            .iter()
            .filter(|project| {
                crate::project::is_installed_any_from_path(project, &record.name, &path)
            })
            .cloned()
            .collect::<Vec<_>>();
        let home = dirs::home_dir();
        // Resolve every ambiguous Finder association before removing any entry.
        for project in self.config.recent_projects.iter().chain(home.iter()) {
            crate::project::validate_uninstall_all_from_path(project, &record.name, &path)?;
        }
        for project in self.config.recent_projects.iter().chain(home.iter()) {
            crate::project::uninstall_all_from_path(project, &record.name, &path)?;
        }
        if let Some(adopted) = self.registry.adopted_sources.get(storage_name) {
            for reference in &adopted.references {
                if reference.kind != crate::adoption::ReferenceKind::Link {
                    continue;
                }
                if let Some(link) = crate::directory_link::inspect(&reference.path)? {
                    // Never remove a reference that another tool/user has repointed.
                    if reference.path.canonicalize().ok().as_ref() == Some(&adopted.source) {
                        crate::directory_link::remove(&reference.path, link)?;
                    }
                }
            }
        }
        if let Some(link) = crate::directory_link::inspect(&path)? {
            crate::directory_link::remove(&path, link)?;
        } else {
            archive_directory(&path, &self.config.library_dir)?;
        }
        self.registry.skills.remove(storage_name);
        self.registry.adopted_sources.remove(storage_name);
        self.registry.trigger_overrides.remove(storage_name);
        self.registry.package_baselines.remove(storage_name);
        if let Some(source) = self.registry.sources.get_mut(&source_key) {
            source.added_skills.retain(|skill| skill != &record.name);
        }
        for project in affected_projects {
            self.config.touch_project(&project);
        }
        self.save_locked(&write_lock)
    }

    pub fn record_source(
        &mut self,
        source: SkillSource,
        mut discovered_skills: Vec<String>,
        added_skills: impl IntoIterator<Item = String>,
    ) -> Result<()> {
        discovered_skills.sort();
        discovered_skills.dedup();
        let key = source.key();
        let record = self
            .registry
            .sources
            .entry(key)
            .or_insert_with(|| SkillSourceRecord {
                source: source.clone(),
                discovered_skills: Vec::new(),
                added_skills: Vec::new(),
            });
        record.source = source;
        record.discovered_skills = discovered_skills;
        record.added_skills.extend(added_skills);
        record.added_skills.sort();
        record.added_skills.dedup();
        self.save()
    }

    pub fn source_records(&self) -> Vec<SkillSourceRecord> {
        let mut sources = self.registry.sources.values().cloned().collect::<Vec<_>>();
        sources.sort_by_key(|source| source.source.label());
        sources
    }

    pub fn groups(&self) -> Vec<SkillGroup> {
        self.registry.groups.clone()
    }

    pub fn move_group(&mut self, id: &str, target: &str, after: bool) -> Result<bool> {
        let groups = &mut self.registry.groups;
        let Some(from) = groups.iter().position(|group| group.id == id) else {
            return Ok(false);
        };
        let Some(to) = groups.iter().position(|group| group.id == target) else {
            return Ok(false);
        };
        if from == to {
            return Ok(false);
        }
        let insert = to + usize::from(after) - usize::from(from < to);
        if from == insert {
            return Ok(false);
        }
        let previous = groups.clone();
        let group = groups.remove(from);
        groups.insert(insert, group);
        if let Err(error) = self.save() {
            self.registry.groups = previous;
            return Err(error);
        }
        Ok(true)
    }

    pub fn ensure_group(&mut self, name: &str) -> Result<String> {
        let name = normalize_group_name(name)?;
        if let Some(group) = self
            .registry
            .groups
            .iter()
            .find(|group| group.name.eq_ignore_ascii_case(&name))
        {
            return Ok(group.id.clone());
        }
        let id = self.new_group_id();
        self.registry.groups.push(SkillGroup {
            id: id.clone(),
            name,
            created_at: operation_stamp(),
        });
        self.save()?;
        Ok(id)
    }

    pub fn create_group(&mut self, name: &str) -> Result<SkillGroup> {
        let name = normalize_group_name(name)?;
        if self
            .registry
            .groups
            .iter()
            .any(|group| group.name.eq_ignore_ascii_case(&name))
        {
            bail!("已经存在同名分组");
        }
        let group = SkillGroup {
            id: self.new_group_id(),
            name,
            created_at: operation_stamp(),
        };
        self.registry.groups.push(group.clone());
        self.save()?;
        Ok(group)
    }

    pub fn rename_group(&mut self, id: &str, name: &str) -> Result<()> {
        let name = normalize_group_name(name)?;
        if self
            .registry
            .groups
            .iter()
            .any(|group| group.id != id && group.name.eq_ignore_ascii_case(&name))
        {
            bail!("已经存在同名分组");
        }
        let group = self
            .registry
            .groups
            .iter_mut()
            .find(|group| group.id == id)
            .context("分组不存在")?;
        group.name = name;
        self.save()
    }

    pub fn assign_group(&mut self, skill: &str, group_id: Option<&str>) -> Result<()> {
        if self.registry.skills.contains_key(skill) {
            return self.assign_group_by_storage(skill, group_id);
        }
        let record = self.record(skill)?;
        self.assign_group_by_storage(&record.storage_name, group_id)
    }

    pub fn delete_group(&mut self, id: &str, delete_skills: bool) -> Result<Vec<String>> {
        let storage_names = self
            .registry
            .skills
            .values()
            .filter(|record| record.group_id.as_deref() == Some(id))
            .map(|record| {
                if record.storage_name.is_empty() {
                    record.name.clone()
                } else {
                    record.storage_name.clone()
                }
            })
            .collect::<Vec<_>>();
        if delete_skills {
            for storage_name in &storage_names {
                self.remove_by_storage(storage_name)?;
            }
        } else {
            for record in self.registry.skills.values_mut() {
                if record.group_id.as_deref() == Some(id) {
                    record.group_id = None;
                    record.last_operated_at = operation_stamp();
                }
            }
        }
        self.registry.groups.retain(|group| group.id != id);
        self.save()?;
        Ok(storage_names)
    }

    fn new_group_id(&self) -> String {
        let stamp = operation_stamp();
        let mut id = format!("group-{stamp}");
        let mut suffix = 1;
        while self.registry.groups.iter().any(|group| group.id == id) {
            id = format!("group-{stamp}-{suffix}");
            suffix += 1;
        }
        id
    }

    pub fn record(&self, name: &str) -> Result<SkillRecord> {
        Ok(self.resolve_skill(name)?.record)
    }

    pub fn record_by_storage(&self, storage_name: &str) -> Result<SkillRecord> {
        self.list()?
            .into_iter()
            .find(|skill| skill.record.storage_name == storage_name)
            .map(|skill| skill.record)
            .context("技能不存在")
    }

    pub(crate) fn has_record_by_storage(&self, storage_name: &str) -> bool {
        self.registry.skills.contains_key(storage_name)
    }

    pub fn set_update_available(&mut self, name: &str, available: bool) -> Result<()> {
        let record = self.record(name)?;
        self.set_update_available_by_storage(&record.storage_name, available)
    }

    pub fn set_update_available_by_storage(
        &mut self,
        storage_name: &str,
        available: bool,
    ) -> Result<()> {
        let record = self
            .registry
            .skills
            .get_mut(storage_name)
            .context("技能不存在")?;
        record.update_available = available;
        self.save()
    }

    pub fn assign_group_by_storage(
        &mut self,
        storage_name: &str,
        group_id: Option<&str>,
    ) -> Result<()> {
        if self
            .registry
            .skills
            .get(storage_name)
            .is_some_and(|record| record.origin.is_builtin())
        {
            bail!("Kitter 内置 Skill 固定显示在列表顶部");
        }
        if let Some(group_id) = group_id
            && !self
                .registry
                .groups
                .iter()
                .any(|group| group.id == group_id)
        {
            bail!("分组不存在");
        }
        let record = self
            .registry
            .skills
            .get_mut(storage_name)
            .context("技能不存在")?;
        record.group_id = group_id.map(str::to_string);
        record.last_operated_at = operation_stamp();
        self.save()
    }

    fn storage_name_for(&self, name: &str, identity: &str) -> String {
        let direct = self.config.library_dir.join(name);
        if !direct.exists() && !self.registry.skills.contains_key(name) {
            return name.to_string();
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        identity.hash(&mut hasher);
        let suffix = format!("{:08x}", hasher.finish() as u32);
        let base = format!("{name}--{suffix}");
        let mut candidate = base.clone();
        let mut index = 2;
        while self.config.library_dir.join(&candidate).exists()
            || self.registry.skills.contains_key(&candidate)
        {
            candidate = format!("{base}-{index}");
            index += 1;
        }
        candidate
    }
}

#[cfg(unix)]
fn ensure_builtin_skill(library_dir: &Path) -> Result<()> {
    use crate::package_fs::Directory;
    use std::{os::fd::AsFd, os::unix::fs::MetadataExt};
    // A registered library may be reached through a migration compatibility
    // link. Resolve that root once, then keep all managed children on bound FDs.
    let physical = library_dir.canonicalize()?;
    let library = Directory::open_anchored(&physical)?;
    let check = |directory: &Directory, path: &Path| -> Result<()> {
        directory.verify_location(path)?;
        let file = fs::File::from(directory.as_fd().try_clone_to_owned()?);
        let metadata = file.metadata()?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0
            || crate::file_access::Access::read(&file)?.has_acl() {
            bail!("内置目录需要当前用户独占写入权限且不能含扩展访问规则");
        }
        Ok(())
    };
    check(&library, &physical)?;
    library.mkdir(Path::new(KITTER_SKILL_STORAGE))?;
    let root = library.child(Path::new(KITTER_SKILL_STORAGE))?;
    check(&root, &physical.join(KITTER_SKILL_STORAGE))?;
    root.mkdir(Path::new("agents"))?;
    root.mkdir(Path::new("references"))?;
    let agents = root.child(Path::new("agents"))?;
    let references = root.child(Path::new("references"))?;
    #[cfg(test)]
    AFTER_BUILTIN_BIND.with(|hook| if let Some(callback) = hook.borrow_mut().take() { callback(); });
    let guard = || -> Result<()> {
        if library_dir.canonicalize()? != physical { bail!("技能库入口身份发生变化"); }
        for (directory, path) in [(&library, physical.clone()),
            (&root, physical.join(KITTER_SKILL_STORAGE)),
            (&agents, physical.join(KITTER_SKILL_STORAGE).join("agents")),
            (&references, physical.join(KITTER_SKILL_STORAGE).join("references"))] {
            check(directory, &path)?;
        }
        Ok(())
    };
    guard()?;
    write_builtin_file(&root, Path::new("SKILL.md"), KITTER_SKILL_MD, &library, &physical, &guard)?;
    write_builtin_file(&agents, Path::new("openai.yaml"), KITTER_OPENAI_YAML, &library, &physical, &guard)?;
    write_builtin_file(&references, Path::new("install-cli.md"), KITTER_INSTALL_CLI_MD, &library, &physical, &guard)?;
    Ok(())
}

#[cfg(unix)]
fn write_builtin_file(directory: &crate::package_fs::Directory, name: &Path, content: &str,
    library: &crate::package_fs::Directory, physical: &Path, guard: &impl Fn() -> Result<()>) -> Result<()> {
    use std::{io::{Read, Write}, os::unix::fs::MetadataExt};
    guard()?;
    let mut access = None;
    let prior = match directory.read(name) {
        Ok(file) => {
            // Reject hard links even when their bytes already match. The read
            // is bounded; an oversized managed file is replaced atomically.
            if file.metadata()?.nlink() != 1 { bail!("内置文件必须是独立实体文件"); }
            access = Some(crate::file_access::Access::read(&file)?);
            let mut bytes = Vec::new();
            let identity = (file.metadata()?.dev(), file.metadata()?.ino());
            file.take(content.len() as u64 + 1).read_to_end(&mut bytes)?;
            if bytes == content.as_bytes() { guard()?; return Ok(()); }
            Some(identity)
        }
        Err(error) if crate::package_fs::is_not_found(&error) => None,
        Err(error) => return Err(error),
    };
    // Only an owner-only staging directory contains the temporary name. Keep
    // displaced versions and failed stages for recovery; never recursively
    // clean a replaceable path. Public library opens also hold registry.lock.
    guard()?;
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let stage_name = format!(".pleat-builtin-stage-{}-{}-{}", std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let stage = library.mkdir_exclusive(Path::new(&stage_name))?;
    crate::file_access::private_directory(&stage)?;
    let staged = Path::new("previous");
    let mut file = stage.create_file(staged, access.as_ref().map_or(0o644, |access| access.mode()))?;
    if let Some(access) = &access { access.apply(&file)?; }
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
    let metadata = file.metadata()?;
    let identity = (metadata.dev(), metadata.ino());
    #[cfg(test)]
    BEFORE_BUILTIN_COMMIT.with(|hook| if let Some(callback) = hook.borrow_mut().take() {
        callback(&physical.join(&stage_name).join(staged));
    });
    guard()?;
    stage.verify_location(&physical.join(&stage_name))?;
    let metadata = stage.read(staged)?.metadata()?;
    if (metadata.dev(), metadata.ino()) != identity || metadata.nlink() != 1 {
        bail!("内置临时文件身份发生变化，已保留恢复目录");
    }
    let current = match directory.read(name) {
        Ok(file) => { let m = file.metadata()?; Some((m.dev(), m.ino())) },
        Err(error) if crate::package_fs::is_not_found(&error) => None,
        Err(error) => return Err(error),
    };
    if current != prior { bail!("内置文件在写入前发生变化，已保留恢复目录"); }
    if prior.is_some() { stage.exchange_to(staged, directory, name)?; }
    else { stage.rename_to(staged, directory, name)?; }
    guard()?;
    let metadata = directory.read(name)?.metadata()?;
    if (metadata.dev(), metadata.ino()) != identity || metadata.nlink() != 1 {
        bail!("内置提交文件身份发生变化，已保留恢复目录");
    }
    Ok(())
}

#[cfg(all(test, unix))]
thread_local! {
    static AFTER_BUILTIN_BIND: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    static BEFORE_BUILTIN_COMMIT: RefCell<Option<Box<dyn FnOnce(&Path)>>> = const { RefCell::new(None) };
}

#[cfg(not(unix))]
fn ensure_builtin_skill(library_dir: &Path) -> Result<()> {
    let root = library_dir.join(KITTER_SKILL_STORAGE);
    let agents = root.join("agents");
    let references = root.join("references");
    fs::create_dir_all(&agents)?;
    fs::create_dir_all(&references)?;
    write_builtin_file(&root.join("SKILL.md"), KITTER_SKILL_MD)?;
    write_builtin_file(&agents.join("openai.yaml"), KITTER_OPENAI_YAML)?;
    write_builtin_file(&references.join("install-cli.md"), KITTER_INSTALL_CLI_MD)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_builtin_file(path: &Path, content: &str) -> Result<()> {
    if fs::read_to_string(path).ok().as_deref() != Some(content) {
        fs::write(path, content)?;
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod builtin_write_guard_tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, symlink};

    #[test]
    fn builtin_write_guard_rejects_directory_links_without_external_writes() {
        for relative in [KITTER_SKILL_STORAGE, "_kitter-builtin/agents", "_kitter-builtin/references"] {
            let temp = tempfile::tempdir().unwrap();
            let library = temp.path().join("library");
            let outside = temp.path().join("outside");
            fs::create_dir_all(&outside).unwrap();
            let link = library.join(relative);
            fs::create_dir_all(link.parent().unwrap()).unwrap();
            fs::write(outside.join("sentinel"), "preserve").unwrap();
            symlink(&outside, &link).unwrap();
            assert!(ensure_builtin_skill(&library).is_err());
            assert_eq!(fs::read_to_string(outside.join("sentinel")).unwrap(), "preserve");
            assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
            assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        }
    }

    #[test]
    fn builtin_write_guard_rejects_leaf_links_without_external_writes() {
        for relative in ["SKILL.md", "agents/openai.yaml", "references/install-cli.md"] {
            let temp = tempfile::tempdir().unwrap();
            let library = temp.path().join("library");
            let leaf = library.join(KITTER_SKILL_STORAGE).join(relative);
            fs::create_dir_all(leaf.parent().unwrap()).unwrap();
            let outside = temp.path().join("outside");
            fs::write(&outside, "preserve").unwrap();
            let inode = fs::metadata(&outside).unwrap().ino();
            symlink(&outside, &leaf).unwrap();
            assert!(ensure_builtin_skill(&library).is_err());
            assert_eq!(fs::read_to_string(&outside).unwrap(), "preserve");
            assert_eq!(fs::metadata(&outside).unwrap().ino(), inode);
            assert!(fs::symlink_metadata(&leaf).unwrap().file_type().is_symlink());
        }
    }

    #[test]
    fn builtin_write_guard_rejects_hardlinks_even_with_expected_bytes() {
        for (relative, content) in [("SKILL.md", KITTER_SKILL_MD), ("agents/openai.yaml", KITTER_OPENAI_YAML), ("references/install-cli.md", KITTER_INSTALL_CLI_MD)] {
            let temp = tempfile::tempdir().unwrap();
            let library = temp.path().join("library");
            let leaf = library.join(KITTER_SKILL_STORAGE).join(relative);
            fs::create_dir_all(leaf.parent().unwrap()).unwrap();
            let outside = temp.path().join("outside");
            fs::write(&outside, content).unwrap();
            fs::hard_link(&outside, &leaf).unwrap();
            assert!(ensure_builtin_skill(&library).is_err());
            assert_eq!(fs::read_to_string(&outside).unwrap(), content);
            assert_eq!(fs::metadata(&outside).unwrap().ino(), fs::metadata(&leaf).unwrap().ino());
        }
    }

    #[test]
    fn builtin_write_guard_fifo_is_rejected_without_blocking() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("library");
        let leaf = library.join(KITTER_SKILL_STORAGE).join("SKILL.md");
        fs::create_dir_all(leaf.parent().unwrap()).unwrap();
        let path = CString::new(leaf.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let start = std::time::Instant::now();
        assert!(ensure_builtin_skill(&library).is_err());
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn builtin_write_guard_compatibility_root_and_noop_keep_identities() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("library");
        fs::create_dir(&library).unwrap();
        let legacy = temp.path().join("legacy");
        symlink(&library, &legacy).unwrap();
        ensure_builtin_skill(&legacy).unwrap();
        let leaf = library.join(KITTER_SKILL_STORAGE).join("SKILL.md");
        let inode = fs::metadata(&leaf).unwrap().ino();
        let sentinel = library.join(KITTER_SKILL_STORAGE).join("local-data/sentinel");
        fs::create_dir(sentinel.parent().unwrap()).unwrap();
        fs::write(&sentinel, "local data").unwrap();
        ensure_builtin_skill(&legacy).unwrap();
        assert_eq!(fs::metadata(&leaf).unwrap().ino(), inode);
        fs::write(&leaf, "outdated builtin").unwrap();
        ensure_builtin_skill(&legacy).unwrap();
        assert_eq!(fs::read_to_string(&leaf).unwrap(), KITTER_SKILL_MD);
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "local data");
        assert_eq!(fs::read_link(&legacy).unwrap(), library);
    }

    #[test]
    fn builtin_write_guard_rejects_child_moved_after_binding() {
        for name in ["agents", "references"] {
            let temp = tempfile::tempdir().unwrap();
            let library = temp.path().join("library");
            fs::create_dir(&library).unwrap();
            let child = library.join(KITTER_SKILL_STORAGE).join(name);
            let outside = temp.path().join("moved-outside");
            let moved = outside.clone();
            AFTER_BUILTIN_BIND.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
                fs::rename(&child, &moved).unwrap();
                fs::create_dir(&child).unwrap();
            })));
            assert!(ensure_builtin_skill(&library).is_err());
            assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
            assert!(!library.join(KITTER_SKILL_STORAGE).join("SKILL.md").exists());
        }
    }

    #[test]
    fn builtin_write_guard_rejects_replaced_stage_without_committing_or_cleaning_it() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("library");
        fs::create_dir(&library).unwrap();
        let outside = temp.path().join("sentinel");
        fs::write(&outside, "preserve").unwrap();
        let target = outside.clone();
        let recorded = std::rc::Rc::new(RefCell::new(None));
        let callback_recorded = recorded.clone();
        BEFORE_BUILTIN_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(move |path| {
            fs::rename(path, path.with_file_name("original-file")).unwrap();
            symlink(&target, path).unwrap();
            *callback_recorded.borrow_mut() = Some(path.to_path_buf());
        })));
        assert!(ensure_builtin_skill(&library).is_err());
        let staged = recorded.borrow().clone().unwrap();
        assert!(fs::symlink_metadata(&staged).unwrap().file_type().is_symlink());
        assert!(staged.with_file_name("original-file").is_file());
        assert!(!library.join(KITTER_SKILL_STORAGE).join("SKILL.md").exists());
        assert_eq!(fs::read_to_string(&outside).unwrap(), "preserve");
    }

    #[test]
    fn builtin_write_guard_rejects_shared_writers_and_preserves_leaf_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("library");
        fs::create_dir(&library).unwrap();
        fs::set_permissions(&library, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(ensure_builtin_skill(&library).is_err());
        assert_eq!(fs::read_dir(&library).unwrap().count(), 0);
        fs::set_permissions(&library, fs::Permissions::from_mode(0o700)).unwrap();
        ensure_builtin_skill(&library).unwrap();
        let leaf = library.join(KITTER_SKILL_STORAGE).join("SKILL.md");
        fs::write(&leaf, "outdated").unwrap();
        fs::set_permissions(&leaf, fs::Permissions::from_mode(0o600)).unwrap();
        ensure_builtin_skill(&library).unwrap();
        assert_eq!(fs::metadata(&leaf).unwrap().mode() & 0o7777, 0o600);
        assert_eq!(fs::read_to_string(&leaf).unwrap(), KITTER_SKILL_MD);
    }
}

fn operation_stamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn normalize_group_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("请输入分组名称");
    }
    Ok(name.to_string())
}

fn unique_group_name(name: &str, used_names: &mut HashSet<String>) -> String {
    let base = name.to_string();
    let mut candidate = base.clone();
    let mut suffix = 2;
    while used_names
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(&candidate))
    {
        candidate = format!("{base} {suffix}");
        suffix += 1;
    }
    used_names.insert(candidate.clone());
    candidate
}

fn migrated_group_id(source_key: &str, groups: &[SkillGroup]) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source_key.hash(&mut hasher);
    let base = format!("source-{:08x}", hasher.finish() as u32);
    let mut candidate = base.clone();
    let mut suffix = 2;
    while groups.iter().any(|group| group.id == candidate) {
        candidate = format!("{base}-{suffix}");
        suffix += 1;
    }
    candidate
}

pub fn read_frontmatter(path: &Path) -> Result<(Option<String>, Option<String>)> {
    let content = fs::read_to_string(path)?;
    let mut lines = content.lines();
    if lines.next().is_some_and(|line| line.trim() == "---") {
        let frontmatter = lines
            .take_while(|line| line.trim() != "---")
            .collect::<Vec<_>>();
        if let Ok(metadata) = serde_yaml::from_str::<Frontmatter>(&frontmatter.join("\n")) {
            return Ok((metadata.name, metadata.description.map(trim_description)));
        }
        let mut name = None;
        let mut description = None;
        for line in frontmatter {
            if let Some(value) = line.strip_prefix("name:") {
                name = Some(unquote(value));
            }
            if let Some(value) = line.strip_prefix("description:") {
                description = Some(unquote(value));
            }
        }
        return Ok((name, description));
    }
    Ok((None, None))
}

#[derive(Default, Deserialize)]
struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
}

fn trim_description(description: String) -> String {
    description.trim_end().to_string()
}

fn unquote(value: &str) -> String {
    value.trim().trim_matches(['\'', '"']).to_string()
}

pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        bail!("技能名称无效");
    }
    Ok(())
}

pub(crate) fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    crate::package_fs::copy_tree(source, destination)
}

fn path_entry_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("无法检查数据或恢复目录"),
    }
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn bytes_revision(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }

fn file_revision(path: &Path) -> Result<Option<String>> {
    Ok(read_optional(path)?.map(|bytes| bytes_revision(&bytes)))
}

fn write_json_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().context("登记文件缺少父目录")?)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

// Stop at each reserved node; its contents are private and must not be read.
fn protected_data_nodes(root: &Path) -> Result<Vec<PathBuf>> {
    let mut nodes = Vec::new();
    let mut walker = WalkDir::new(root).follow_links(false).into_iter();
    while let Some(entry) = walker.next() {
        let entry = entry?;
        if entry.depth() > 0 && (entry.file_name() == "local-data" || entry.file_name() == ".git") {
            nodes.push(entry.path().strip_prefix(root)?.to_path_buf());
            if entry.file_type().is_dir() {
                walker.skip_current_dir();
            }
        }
    }
    Ok(nodes)
}

// Removed code is recoverable. This also preserves concurrent data writes that
// arrive after a preflight check, instead of racing a recursive deletion.




fn archive_directory(path: &Path, library_dir: &Path) -> Result<()> {
    let recovery = library_dir.parent().context("技能库目录缺少管理父目录")?
        .join("library-support").join("recovery");
    fs::create_dir_all(&recovery)?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let slot = recovery.join(format!("{stamp}"));
    fs::create_dir(&slot)?;
    fs::rename(
        path,
        slot.join(path.file_name().context("恢复目录缺少名称")?),
    )
    .context("无法归档代码目录，原目录仍保留，请先检查")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_skill_is_manual_pinned_and_protected() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let library_dir = data_dir.join("skills");
        fs::create_dir_all(&library_dir).unwrap();
        fixture_skill(&library_dir.join("another-skill"), "another-skill");
        ensure_builtin_skill(&library_dir).unwrap();

        let builtin_path = library_dir.join(KITTER_SKILL_STORAGE);
        assert!(crate::effective_skills::is_manual_skill(&builtin_path));
        let skill_markdown = fs::read_to_string(builtin_path.join("SKILL.md")).unwrap();
        assert!(skill_markdown.contains("kitter install"));
        assert!(skill_markdown.contains("Get-Command kitter"));
        assert!(skill_markdown.contains("disable-model-invocation: true"));
        assert!(skill_markdown.contains("autoinvoke: false"));
        assert!(
            fs::read_to_string(builtin_path.join("references/install-cli.md"))
                .unwrap()
                .contains("gh release download")
        );
        assert!(
            fs::read_to_string(builtin_path.join("agents/openai.yaml"))
                .unwrap()
                .contains("allow_implicit_invocation: false")
        );

        assert!(!builtin_path.join("bin").exists());

        let mut registry = Registry::default();
        registry.skills.insert(
            KITTER_SKILL_STORAGE.to_string(),
            SkillRecord {
                name: KITTER_SKILL_NAME.to_string(),
                storage_name: KITTER_SKILL_STORAGE.to_string(),
                description: String::new(),
                origin: SkillOrigin::Builtin,
                update_available: false,
                group_id: None,
                last_operated_at: u64::MAX,
            },
        );
        let mut library = SkillLibrary {
            config: AppConfig {
                library_dir,
                ..AppConfig::default()
            },
            registry,
            data_dir,
            disk_revisions: Mutex::new([None, None]),
            proma_sync_warnings: Vec::new(),
            project_observations: Mutex::new(Vec::new()),
        };
        let skills = library.list().unwrap();
        assert_eq!(skills[0].record.name, KITTER_SKILL_NAME);
        assert!(skills[0].record.origin.is_builtin());
        assert!(library.remove_by_storage(KITTER_SKILL_STORAGE).is_err());
        assert!(
            library
                .assign_group_by_storage(KITTER_SKILL_STORAGE, None)
                .is_err()
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn opening_library_does_not_touch_a_locked_legacy_cli() {
        use std::{fs::OpenOptions, io::Read as _, os::windows::fs::OpenOptionsExt as _};

        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let legacy_cli = data_dir
            .join("skills")
            .join(KITTER_SKILL_STORAGE)
            .join("bin/kitter.exe");
        fs::create_dir_all(legacy_cli.parent().unwrap()).unwrap();
        fs::write(&legacy_cli, b"legacy desktop-hosted CLI").unwrap();
        let mut locked = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&legacy_cli)
            .unwrap();

        let library = SkillLibrary::open_in(&data_dir).unwrap();

        assert_eq!(library.data_dir(), data_dir);
        let mut contents = Vec::new();
        locked.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"legacy desktop-hosted CLI");
    }

    #[cfg(unix)]
    fn adoption_fixture() -> (tempfile::TempDir, SkillLibrary, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let home = root.join("home");
        let library_dir = root.join("data/skills");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&library_dir).unwrap();
        let library = SkillLibrary {
            config: AppConfig {
                library_dir,
                ..AppConfig::default()
            },
            registry: Registry::default(),
            data_dir: root.join("data"),
            disk_revisions: Mutex::new([None, None]),
            proma_sync_warnings: Vec::new(),
            project_observations: Mutex::new(Vec::new()),
        };
        (temp, library, home)
    }

    fn fixture_skill(path: &Path, name: &str) {
        fs::create_dir_all(path).unwrap();
        fs::write(
            path.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: test\n---\n"),
        )
        .unwrap();
        fs::write(path.join("keep.txt"), "original source").unwrap();
    }

    fn copied_fixture() -> (tempfile::TempDir, SkillLibrary, PathBuf, SkillRecord) {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fixture_skill(&source, "demo");
        fs::create_dir(source.join("local-data")).unwrap();
        fs::write(source.join("local-data/source.txt"), "source data").unwrap();
        let mut library = SkillLibrary::open_in(temp.path().join("data")).unwrap();
        library
            .import(
                &source,
                SkillRecord {
                    name: "demo".into(),
                    storage_name: String::new(),
                    description: String::new(),
                    origin: SkillOrigin::Unknown,
                    update_available: false,
                    group_id: None,
                    last_operated_at: 0,
                },
            )
            .unwrap();
        let record = library.record("demo").unwrap();
        (temp, library, source, record)
    }

    #[test]
    fn adaptation_keeps_source_entry_original_and_retains_manual_mode_on_update() {
        let (_temp, mut library, source, record) = copied_fixture();
        library.set_trigger_mode_by_storage(&record.storage_name, TriggerMode::Manual).unwrap();
        let path = library.skill_path("demo").unwrap();
        fs::create_dir(path.join("local-data")).unwrap();
        fs::write(path.join("local-data/ADAPTATION.md"), "manual local instructions").unwrap();
        let updated = "---\nname: demo\ndescription: upstream update\n---\n";
        fs::write(source.join("SKILL.md"), updated).unwrap();
        library.replace_by_storage(&source, record.storage_name.clone(), record.clone()).unwrap();
        assert_eq!(fs::read_to_string(path.join("SKILL.md")).unwrap(), updated);
        assert_eq!(library.trigger_mode_by_storage(&record.storage_name), TriggerMode::Manual);
        assert_eq!(fs::read_to_string(path.join("local-data/ADAPTATION.md")).unwrap(), "manual local instructions");
        assert_eq!(library.registry.package_baselines[&record.storage_name], crate::update_protection::snapshot(&source).unwrap());
        library.set_trigger_mode_by_storage(&record.storage_name, TriggerMode::Automatic).unwrap();
        assert_eq!(fs::read_to_string(path.join("SKILL.md")).unwrap(), updated);
        assert_eq!(library.trigger_mode_by_storage(&record.storage_name), TriggerMode::Automatic);
        library.set_trigger_mode_by_storage(&record.storage_name, TriggerMode::FollowSkill).unwrap();
        assert_eq!(fs::read_to_string(path.join("SKILL.md")).unwrap(), updated);
        assert_eq!(library.registry.package_baselines[&record.storage_name], crate::update_protection::snapshot(&source).unwrap());
    }

    #[test]
    fn config_decode_and_revision_use_the_same_bytes_under_concurrent_edit() {
        let (temp,_initial,_,_)=copied_fixture();
        let _stable=SkillLibrary::open_in(temp.path().join("data")).unwrap();
        AFTER_CONFIG_READ.with(|hook| *hook.borrow_mut()=Some(|data| {
            let p=data.join("config.json");let mut value:serde_json::Value=serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
            value["collapsed_skill_groups_initialized"]=serde_json::Value::Bool(true);
            fs::write(p,serde_json::to_vec(&value).unwrap()).unwrap();
        }));
        let loaded=SkillLibrary::open_in(temp.path().join("data")).unwrap();
        assert!(!loaded.config.collapsed_skill_groups_initialized);
        assert!(loaded.save().unwrap_err().to_string().contains("其他操作"));
        let current:serde_json::Value=serde_json::from_slice(&fs::read(temp.path().join("data/config.json")).unwrap()).unwrap();
        assert_eq!(current["collapsed_skill_groups_initialized"],true);
    }

    #[cfg(unix)]
    #[test]
    fn source_link_and_directory_permission_conflicts_stop_before_live_mutation() {
        use std::os::unix::fs::{symlink,PermissionsExt};
        for link in [false,true] {
            let (_temp,mut library,source,record)=copied_fixture();let path=library.skill_path("demo").unwrap();
            if link {
                fs::write(path.join("collision"),"local file").unwrap();symlink("missing",source.join("collision")).unwrap();
            } else {
                fs::create_dir(source.join("refs")).unwrap();
                library.replace_by_storage(&source,record.storage_name.clone(),record.clone()).unwrap();
                fs::set_permissions(path.join("refs"),fs::Permissions::from_mode(0o700)).unwrap();
                fs::set_permissions(source.join("refs"),fs::Permissions::from_mode(0o555)).unwrap();
            }
            let before=crate::update_protection::snapshot(&path).unwrap();
            assert!(library.replace_by_storage(&source,record.storage_name.clone(),record.clone()).is_err());
            assert_eq!(crate::update_protection::snapshot(&path).unwrap(),before);
        }
    }

    #[test]
    fn protected_update_retains_local_files_across_reopen_and_two_updates() {
        let (temp, mut library, source, record) = copied_fixture();
        let path = library.skill_path("demo").unwrap();
        fs::write(path.join("SKILL.md"), "---\nname: demo\ndescription: locally edited\n---\n").unwrap();
        fs::create_dir(path.join("references")).unwrap();
        fs::write(path.join("references/mine.md"), "local reference").unwrap();
        fs::create_dir(path.join("scripts")).unwrap();
        fs::write(path.join("scripts/mine.py"), "local script").unwrap();
        fs::write(path.join("asset.png"), b"local asset").unwrap();
        fs::write(source.join("keep.txt"), "version 2").unwrap();
        library.replace_by_storage(&source, record.storage_name.clone(), record.clone()).unwrap();
        let mut reopened = SkillLibrary::open_in(temp.path().join("data")).unwrap();
        fs::write(source.join("keep.txt"), "version 3").unwrap();
        reopened.replace_by_storage(&source, record.storage_name.clone(), record).unwrap();
        assert!(fs::read_to_string(path.join("SKILL.md")).unwrap().contains("locally edited"));
        assert_eq!(fs::read_to_string(path.join("keep.txt")).unwrap(), "version 3");
        assert_eq!(fs::read_to_string(path.join("scripts/mine.py")).unwrap(), "local script");
        assert_eq!(fs::read_to_string(path.join("references/mine.md")).unwrap(), "local reference");
        assert_eq!(fs::read(path.join("asset.png")).unwrap(), b"local asset");
    }

    #[test]
    fn protected_conflict_and_missing_legacy_baseline_do_not_touch_live_files() {
        let (_temp, mut library, source, record) = copied_fixture();
        let path = library.skill_path("demo").unwrap();
        fs::write(path.join("keep.txt"), "local edit").unwrap();
        fs::write(source.join("keep.txt"), "source edit").unwrap();
        let registry = fs::read(library.data_dir.join("registry.json")).unwrap();
        assert!(library.replace_by_storage(&source, record.storage_name.clone(), record.clone()).is_err());
        assert_eq!(fs::read_to_string(path.join("keep.txt")).unwrap(), "local edit");
        assert_eq!(fs::read(library.data_dir.join("registry.json")).unwrap(), registry);
        assert!(!path.with_extension("kitter-backup").exists());
        library.registry.package_baselines.clear();
        library.save().unwrap();
        assert!(library.replace_by_storage(&source, record.storage_name.clone(), record).unwrap_err().to_string().contains("基线"));
        assert_eq!(fs::read_to_string(path.join("keep.txt")).unwrap(), "local edit");
        assert!(!path.with_extension("kitter-staging").exists());
    }

    #[test]
    fn trigger_changes_do_not_overwrite_edits_or_wash_unrelated_files() {
        let (_temp, mut library, source, record) = copied_fixture();
        let path = library.skill_path("demo").unwrap();
        fs::write(path.join("keep.txt"), "local script edit").unwrap();
        library.set_trigger_mode_by_storage(&record.storage_name, TriggerMode::Manual).unwrap();
        library.replace_by_storage(&source, record.storage_name.clone(), record.clone()).unwrap();
        assert_eq!(fs::read_to_string(path.join("keep.txt")).unwrap(), "local script edit");
        fs::write(source.join("keep.txt"), "source script edit").unwrap();
        assert!(library.replace_by_storage(&source, record.storage_name.clone(), record.clone()).is_err());
        let original = fs::read(path.join("SKILL.md")).unwrap();
        fs::write(path.join("SKILL.md"), "edited skill body").unwrap();
        assert!(library.set_trigger_mode_by_storage(&record.storage_name, TriggerMode::Automatic).is_err());
        assert_eq!(fs::read_to_string(path.join("SKILL.md")).unwrap(), "edited skill body");
        fs::write(path.join("SKILL.md"), original).unwrap();
        fs::write(path.join("agents/openai.yaml"), "interface: locally-edited\n").unwrap();
        assert!(library.set_trigger_mode_by_storage(&record.storage_name, TriggerMode::Automatic).is_err());
        assert_eq!(fs::read_to_string(path.join("agents/openai.yaml")).unwrap(), "interface: locally-edited\n");
    }









    #[cfg(unix)]
    #[test]
    fn local_import_does_not_authorize_writing_plugin_cache_or_app_bundle() {
        for relative in [".codex/plugins/cache/plugin/skills/demo", "Some.app/Contents/Resources/skills/demo"] {
            let temp = tempfile::tempdir().unwrap(); let source = temp.path().join(relative);
            fixture_skill(&source, "demo");
            let mut library = SkillLibrary::open_in(temp.path().join("pleat")).unwrap();
            library.import(&source, SkillRecord { name: "demo".into(), storage_name: String::new(), description: String::new(),
                origin: SkillOrigin::Local { path: source.clone(), source_root: None }, update_available: false, group_id: None, last_operated_at: 0 }).unwrap();
            let storage = library.resolve_skill("demo").unwrap().record.storage_name;
            let before = fs::read(source.join("SKILL.md")).unwrap();
            assert!(library.set_trigger_mode_by_storage(&storage, TriggerMode::Manual).is_err());
            assert_eq!(fs::read(source.join("SKILL.md")).unwrap(), before);
        }
    }

    #[test]
    fn concurrent_writers_and_stale_registry_are_rejected_before_package_changes() {
        let (temp, _initial_library, source, record) = copied_fixture();
        let mut other = SkillLibrary::open_in(temp.path().join("data")).unwrap();
        let mut library = SkillLibrary::open_in(temp.path().join("data")).unwrap();
        let lock = library.write_lock().unwrap();
        assert!(other.replace_by_storage(&source, record.storage_name.clone(), record.clone()).is_err());
        drop(lock);
        other.config.recent_projects.push(temp.path().join("new-project"));
        other.save().unwrap();
        let path = library.skill_path("demo").unwrap();
        fs::write(source.join("keep.txt"), "new source").unwrap();
        assert!(library.replace_by_storage(&source, record.storage_name.clone(), record).unwrap_err().to_string().contains("其他操作"));
        assert_eq!(fs::read_to_string(path.join("keep.txt")).unwrap(), "original source");
    }

    #[test]
    fn late_original_edit_and_registry_conflict_roll_back_without_losing_edits() {
        for registry_change in [false, true] {
            let (_temp, mut library, source, record) = copied_fixture();
            let path = library.skill_path("demo").unwrap();
            fs::write(source.join("keep.txt"), "new source").unwrap();
            if registry_change {
                BEFORE_UPDATE_COMMIT.with(|hook| *hook.borrow_mut() = Some(|_,_,data| {
                    let p=data.join("registry.json");let mut value:serde_json::Value=serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
                    value["source_groups_migrated"]=serde_json::Value::Bool(false);
                    fs::write(p,serde_json::to_vec(&value).unwrap()).unwrap();
                }));
            } else {
                BEFORE_UPDATE_COMMIT.with(|hook| *hook.borrow_mut() = Some(|_,backup,_| {
                    fs::write(backup.join("keep.txt"), "late external edit").unwrap();
                }));
            }
            assert!(library.replace_by_storage(&source, record.storage_name.clone(), record).is_err());
            assert_eq!(fs::read_to_string(path.join("keep.txt")).unwrap(), if registry_change {"original source"} else {"late external edit"});
            assert!(!path.with_extension("kitter-backup").exists());
            if registry_change {
                let value:serde_json::Value=serde_json::from_slice(&fs::read(library.data_dir.join("registry.json")).unwrap()).unwrap();
                assert_eq!(value["source_groups_migrated"],false);
            }
        }
    }

    #[test]
    fn late_nested_local_data_is_restored_with_original_code() {
        let (_temp, mut library, source, record) = copied_fixture();
        fs::create_dir(source.join("scripts")).unwrap();
        fs::write(source.join("scripts/main"), "old code").unwrap();
        library.replace_by_storage(&source, record.storage_name.clone(), record.clone()).unwrap();
        let before = fs::read(library.data_dir.join("registry.json")).unwrap();
        fs::write(source.join("scripts/main"), "new code").unwrap();
        BEFORE_UPDATE_COMMIT.with(|hook| *hook.borrow_mut() = Some(|_, backup, _| {
            fs::create_dir(backup.join("scripts/local-data")).unwrap();
            fs::write(backup.join("scripts/local-data/state"), "late user state").unwrap();
        }));
        assert!(library.replace_by_storage(&source, record.storage_name.clone(), record).is_err());
        let path = library.skill_path("demo").unwrap();
        assert_eq!(fs::read_to_string(path.join("scripts/main")).unwrap(), "old code");
        assert_eq!(fs::read_to_string(path.join("scripts/local-data/state")).unwrap(), "late user state");
        assert_eq!(fs::read(library.data_dir.join("registry.json")).unwrap(), before);
    }

    #[cfg(unix)]
    #[test]
    fn links_permissions_and_root_git_are_preserved_without_reading_targets() {
        use std::os::unix::fs::{symlink,PermissionsExt};
        use std::os::unix::fs::MetadataExt;
        let (_temp, mut library, source, record) = copied_fixture();
        let path = library.skill_path("demo").unwrap();
        fs::create_dir(path.join(".git")).unwrap();
        fs::write(path.join(".git/config"), "private git settings").unwrap();
        let inode=fs::metadata(path.join(".git")).unwrap().ino();
        symlink("local-data/missing",path.join("runtime-link")).unwrap();
        fs::set_permissions(path.join("keep.txt"),fs::Permissions::from_mode(0o755)).unwrap();
        library.replace_by_storage(&source,record.storage_name.clone(),record.clone()).unwrap();
        assert_eq!(fs::read_link(path.join("runtime-link")).unwrap(),Path::new("local-data/missing"));
        assert_eq!(fs::metadata(path.join("keep.txt")).unwrap().permissions().mode() & 0o777,0o755);
        assert_eq!(fs::metadata(path.join(".git")).unwrap().ino(),inode);
        fs::write(source.join("keep.txt"),"changed source").unwrap();
        assert!(library.replace_by_storage(&source,record.storage_name.clone(),record).is_err());
        assert_eq!(fs::read_to_string(path.join(".git/config")).unwrap(),"private git settings");
    }

    #[test]
    fn copied_import_and_preview_exclude_local_data() {
        let (_temp, library, _source, record) = copied_fixture();
        let path = library.skill_path_by_storage(&record.storage_name).unwrap();
        assert!(!path.join("local-data").exists());
        fs::create_dir(path.join("local-data")).unwrap();
        fs::write(path.join("local-data/private.txt"), "private fixture").unwrap();
        assert!(
            !library
                .files("demo")
                .unwrap()
                .iter()
                .any(|p| p.starts_with("local-data"))
        );
        assert!(
            library
                .read_file("demo", Path::new("local-data/private.txt"))
                .is_err()
        );
        assert!(library.read_file("demo", Path::new("keep.txt")).is_ok());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("local-data/private.txt", path.join("alias.txt")).unwrap();
            assert!(library.read_file("demo", Path::new("alias.txt")).is_err());
        }
    }

    #[test]
    fn update_preserves_local_data_and_failed_update_restores_it() {
        let (temp, mut library, source, record) = copied_fixture();
        let path = library.skill_path_by_storage(&record.storage_name).unwrap();
        fs::create_dir(path.join("local-data")).unwrap();
        let data = path.join("local-data/private.bin");
        fs::write(&data, [0, 1, 255, 2]).unwrap();
        #[cfg(unix)]
        let original_inode = {
            use std::os::unix::fs::MetadataExt;
            fs::metadata(&data).unwrap().ino()
        };
        fs::write(source.join("keep.txt"), "updated code").unwrap();
        library
            .replace_by_storage(&source, record.storage_name.clone(), record.clone())
            .unwrap();
        assert_eq!(
            fs::read_to_string(path.join("keep.txt")).unwrap(),
            "updated code"
        );
        assert_eq!(fs::read(&data).unwrap(), [0, 1, 255, 2]);
        assert!(!path.join("local-data/source.txt").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(fs::metadata(&data).unwrap().ino(), original_inode);
        }
        fs::write(source.join("keep.txt"), "failing code").unwrap();
        library.data_dir = temp.path().join("source/SKILL.md");
        assert!(
            library
                .replace_by_storage(&source, record.storage_name.clone(), record.clone())
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(path.join("keep.txt")).unwrap(),
            "updated code"
        );
        assert_eq!(fs::read(&data).unwrap(), [0, 1, 255, 2]);
        assert!(!path.with_extension("kitter-backup").exists());
        // Protection runs before uninstalling references or changing registry.
        assert!(library.remove_by_storage(&record.storage_name).is_err());
        assert!(library.record("demo").is_ok());
        assert!(data.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn dangling_data_links_and_existing_recovery_directories_are_protected() {
        use std::os::unix::fs::symlink;
        let (_temp, mut library, source, record) = copied_fixture();
        let path = library.skill_path_by_storage(&record.storage_name).unwrap();
        symlink("absent-directory", path.join("local-data")).unwrap();
        assert!(library.remove_by_storage(&record.storage_name).is_err());
        library
            .replace_by_storage(&source, record.storage_name.clone(), record.clone())
            .unwrap();
        assert_eq!(
            fs::read_link(path.join("local-data")).unwrap(),
            Path::new("absent-directory")
        );
        let backup = path.with_extension("kitter-backup");
        symlink("absent-recovery", &backup).unwrap();
        assert!(
            library
                .replace_by_storage(&source, record.storage_name.clone(), record)
                .is_err()
        );
        assert_eq!(
            fs::read_link(&backup).unwrap(),
            Path::new("absent-recovery")
        );
        assert!(path.join("SKILL.md").is_file());
    }

    #[test]
    fn nested_data_blocks_updates_and_removal_without_changing_the_package() {
        let (_temp, mut library, source, record) = copied_fixture();
        let path = library.skill_path_by_storage(&record.storage_name).unwrap();
        fs::create_dir_all(path.join("nested/local-data")).unwrap();
        fs::write(path.join("nested/local-data/state"), "retained").unwrap();
        assert!(
            library
                .replace_by_storage(&source, record.storage_name.clone(), record.clone())
                .is_err()
        );
        assert!(library.remove_by_storage(&record.storage_name).is_err());
        assert_eq!(
            fs::read_to_string(path.join("nested/local-data/state")).unwrap(),
            "retained"
        );
        assert_eq!(
            fs::read_to_string(path.join("keep.txt")).unwrap(),
            "original source"
        );
        assert!(library.record("demo").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn update_keeps_data_directory_and_open_handle_stable() {
        use std::{io::Write, os::unix::fs::MetadataExt};
        let (_temp, mut library, source, record) = copied_fixture();
        let path = library.skill_path_by_storage(&record.storage_name).unwrap();
        let local_data = path.join("local-data");
        fs::create_dir(&local_data).unwrap();
        let inode = fs::metadata(&local_data).unwrap().ino();
        let mut handle = fs::File::create(local_data.join("active-state")).unwrap();
        library
            .replace_by_storage(&source, record.storage_name.clone(), record)
            .unwrap();
        handle.write_all(b"after update").unwrap();
        assert_eq!(fs::metadata(&local_data).unwrap().ino(), inode);
        assert_eq!(
            fs::read(local_data.join("active-state")).unwrap(),
            b"after update"
        );
    }

    #[cfg(unix)]
    fn discover(library: &SkillLibrary, home: &Path) -> crate::adoption::AdoptionScan {
        crate::adoption::scan(
            home,
            &[],
            &library.config.library_dir,
            &library.list().unwrap(),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn adoption_links_library_and_all_references_without_copying_or_deleting_source() {
        use std::os::unix::fs::symlink;
        let (_temp, mut library, home) = adoption_fixture();
        let source = home.join("source");
        fixture_skill(&source, "demo");
        fs::create_dir_all(home.join(".claude/skills")).unwrap();
        let link = home.join(".claude/skills/demo");
        symlink("../../source", &link).unwrap();
        symlink(&source, home.join("custom-alias")).unwrap();
        let scan = discover(&library, &home);
        let candidate = &scan.candidates[0];
        let storage = library
            .adopt(candidate, &scan.references_for(candidate))
            .unwrap();
        assert!(library.is_linked_source(&storage));
        assert!(crate::source::update_by_storage(&mut library, &storage).is_err());
        assert_eq!(fs::read_link(&link).unwrap(), source);
        assert_eq!(
            crate::project::list(&home, &library.config.library_dir).unwrap()[0].installations[0]
                .managed,
            true
        );
        let scan = discover(&library, &home);
        library
            .adopt(
                &scan.candidates[0],
                &scan.references_for(&scan.candidates[0]),
            )
            .unwrap();
        assert_eq!(library.list().unwrap().len(), 1);
        library.remove_by_storage(&storage).unwrap();
        assert_eq!(
            fs::read_to_string(source.join("keep.txt")).unwrap(),
            "original source"
        );
        assert!(link.symlink_metadata().is_err());
        assert_eq!(home.join("custom-alias").canonicalize().unwrap(), source); // Outside direct install roots.
    }

    #[cfg(unix)]
    #[test]
    fn adoption_rolls_back_links_and_registration_when_save_fails() {
        use std::os::unix::fs::symlink;
        let (_temp, mut library, home) = adoption_fixture();
        fixture_skill(&home.join("source"), "demo");
        fs::create_dir_all(home.join(".claude/skills")).unwrap();
        symlink("../../source", home.join(".claude/skills/alias")).unwrap();
        let scan = discover(&library, &home);
        library.data_dir = home.join("source/SKILL.md");
        assert!(
            library
                .adopt(
                    &scan.candidates[0],
                    &scan.references_for(&scan.candidates[0])
                )
                .is_err()
        );
        assert_eq!(
            home.join(".claude/skills/alias").canonicalize().unwrap(),
            home.join("source")
        );
        assert!(library.list().unwrap().is_empty());
        assert!(library.registry.skills.is_empty());
        assert!(home.join("source/keep.txt").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn adopted_same_names_keep_distinct_source_identities_and_storage() {
        let (_temp, mut library, home) = adoption_fixture();
        fixture_skill(&home.join(".agents/skills/a"), "same");
        fixture_skill(&home.join(".agents/skills/b"), "same");
        let scan = discover(&library, &home);
        for candidate in &scan.candidates {
            library.adopt(candidate, &[]).unwrap();
        }
        let skills = library.list().unwrap();
        assert_eq!(skills.len(), 2);
        assert_ne!(
            skills[0].record.identity_key(),
            skills[1].record.identity_key()
        );
        assert_ne!(skills[0].record.storage_name, skills[1].record.storage_name);
        assert!(library.resolve_skill("same").is_err());
        for skill in skills {
            let resolved = library
                .resolve_skill(&format!("id:{}", skill.record.storage_name))
                .unwrap();
            assert_eq!(resolved.path, skill.path);
        }
    }

    #[cfg(unix)]
    #[test]
    fn switching_an_identity_updates_chained_and_library_references() {
        use std::os::unix::fs::symlink;
        for fail_save in [false, true] {
            let (_temp, mut library, home) = adoption_fixture();
            for project in ["a", "b"] {
                let root = home.join(project);
                fixture_skill(&root.join(".agents/skills/alias"), "demo");
                fs::write(
                    root.join("skills-lock.json"),
                    r#"{"skills":{"alias":{"source":"owner/repo","sourceType":"github"}}}"#,
                )
                .unwrap();
            }
            let a = home.join("a/.agents/skills/alias");
            let b = home.join("b/.agents/skills/alias");
            let scan = discover(&library, &home);
            let first = scan.candidates.iter().find(|c| c.source == a).unwrap();
            let storage = library.adopt(first, &[]).unwrap();
            let library_path = library.config.library_dir.join(&storage);
            fs::create_dir_all(home.join(".codex/skills")).unwrap();
            symlink(&library_path, home.join(".codex/skills/library-ref")).unwrap();
            symlink(&a, home.join(".codex/skills/first-ref")).unwrap();
            symlink(
                home.join(".codex/skills/first-ref"),
                home.join(".codex/skills/chained-ref"),
            )
            .unwrap();
            let scan = discover(&library, &home);
            let second = scan.candidates.iter().find(|c| c.source == b).unwrap();
            assert_eq!(scan.references_for(second).len(), 5);
            if fail_save {
                library.data_dir = home.join("a/.agents/skills/alias/SKILL.md");
            }
            let result = library.adopt(second, &scan.references_for(second));
            assert_eq!(result.is_err(), fail_save);
            let expected = if fail_save { &a } else { &b };
            for reference in ["library-ref", "first-ref", "chained-ref"] {
                assert_eq!(
                    &home
                        .join(".codex/skills")
                        .join(reference)
                        .canonicalize()
                        .unwrap(),
                    expected
                );
            }
            assert_eq!(&library_path.canonicalize().unwrap(), expected);
            assert!(a.join("keep.txt").is_file());
            assert!(b.join("keep.txt").is_file());
            assert_eq!(library.list().unwrap().len(), 1);
        }
    }

    #[cfg(unix)]
    #[test]
    fn removal_does_not_delete_a_repointed_reference_or_a_direct_source_directory() {
        use std::os::unix::fs::symlink;
        let (_temp, mut library, home) = adoption_fixture();
        let source = home.join(".agents/skills/demo");
        fixture_skill(&source, "demo");
        fixture_skill(&home.join("other"), "other");
        fs::create_dir_all(home.join(".claude/skills")).unwrap();
        symlink(&source, home.join(".claude/skills/alias")).unwrap();
        let scan = discover(&library, &home);
        let candidate = scan.candidates.iter().find(|c| c.name == "demo").unwrap();
        let storage = library
            .adopt(candidate, &scan.references_for(candidate))
            .unwrap();
        assert!(
            !crate::project::list(&home, &library.config.library_dir).unwrap()[0].installations[0]
                .managed
        );
        crate::adoption::replace_link(&home.join(".claude/skills/alias"), &home.join("other"))
            .unwrap();
        library.remove_by_storage(&storage).unwrap();
        assert!(source.join("keep.txt").is_file());
        assert_eq!(
            home.join(".claude/skills/alias").canonicalize().unwrap(),
            home.join("other")
        );
    }

    #[cfg(unix)]
    #[test]
    fn direct_install_scan_does_not_adopt_or_modify_plugins() {
        let (_temp, library, home) = adoption_fixture();
        let root = home.join(".claude/plugins/cache/market/demo/v1");
        fixture_skill(&root.join("skills/demo"), "demo");
        let registry_path = home.join(".claude/plugins/installed_plugins.json");
        let bytes = serde_json::to_vec(
            &serde_json::json!({"plugins":{"demo@market":[{"scope":"user","installPath":root}]}}),
        )
        .unwrap();
        fs::write(&registry_path, &bytes).unwrap();
        let scan = discover(&library, &home);
        assert!(scan.candidates.is_empty());
        assert_eq!(fs::read(registry_path).unwrap(), bytes);
        assert!(root.join("skills/demo/keep.txt").is_file());
    }

    #[test]
    fn reads_literal_block_descriptions() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("SKILL.md");
        fs::write(
            &path,
            "---\nname: multiline\ndescription: |\n  First line\n  Second line\n---\n",
        )
        .unwrap();

        assert_eq!(
            read_frontmatter(&path).unwrap(),
            (
                Some("multiline".into()),
                Some("First line\nSecond line".into())
            )
        );
    }

    #[test]
    fn list_reads_the_current_description_without_persisting_it() {
        let temp = tempfile::tempdir().unwrap();
        let library_dir = temp.path().join("skills");
        let skill_dir = library_dir.join("demo");
        fs::create_dir_all(&skill_dir).unwrap();
        let skill_file = skill_dir.join("SKILL.md");
        fs::write(&skill_file, "---\nname: demo\ndescription: first\n---\n").unwrap();

        let library = SkillLibrary {
            config: AppConfig {
                library_dir,
                ..AppConfig::default()
            },
            registry: Registry::default(),
            data_dir: temp.path().join("data"),
            disk_revisions: Mutex::new([None, None]),
            proma_sync_warnings: Vec::new(),
            project_observations: Mutex::new(Vec::new()),
        };

        assert_eq!(library.list().unwrap()[0].record.description, "first");
        fs::write(&skill_file, "---\nname: demo\ndescription: second\n---\n").unwrap();
        assert_eq!(library.list().unwrap()[0].record.description, "second");

        let json = serde_json::to_string(&library.list().unwrap()[0].record).unwrap();
        assert!(!json.contains("description"));
    }

    #[test]
    fn group_order_is_persisted_without_changing_group_identity() {
        let temp = tempfile::tempdir().unwrap();
        let mut library = SkillLibrary::open_in(temp.path()).unwrap();
        let a = library.create_group("A").unwrap();
        let b = library.create_group("B").unwrap();
        let c = library.create_group("C").unwrap();
        assert!(library.move_group(&a.id, &c.id, true).unwrap());
        assert_eq!(
            library
                .groups()
                .iter()
                .map(|g| g.name.as_str())
                .collect::<Vec<_>>(),
            ["B", "C", "A"]
        );
        assert!(library.move_group(&a.id, &b.id, false).unwrap());
        assert!(!library.move_group(&a.id, &b.id, false).unwrap());
        assert!(!library.move_group(&a.id, &a.id, true).unwrap());
        assert!(!library.move_group("missing", &a.id, true).unwrap());
        let reopened = SkillLibrary::open_in(temp.path()).unwrap();
        assert_eq!(
            reopened
                .groups()
                .iter()
                .map(|g| g.id.clone())
                .collect::<Vec<_>>(),
            [a.id, b.id, c.id]
        );
    }

    #[test]
    fn group_names_preserve_source_style_separators() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let mut library = SkillLibrary::open_in(&data_dir).unwrap();

        let group = library.create_group("owner/repository").unwrap();
        assert_eq!(group.name, "owner/repository");
        library.rename_group(&group.id, r"team\skills").unwrap();

        let reopened = SkillLibrary::open_in(data_dir).unwrap();
        assert_eq!(reopened.groups()[0].name, r"team\skills");
    }

    #[test]
    fn local_import_links_source_and_removal_keeps_source() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fixture_skill(&source, "demo");
        let mut library = SkillLibrary::open_in(temp.path().join("data")).unwrap();
        let group = library.create_group("local").unwrap();
        library
            .import(
                &source,
                SkillRecord {
                    name: "demo".into(),
                    storage_name: String::new(),
                    description: String::new(),
                    origin: SkillOrigin::Local {
                        path: source.clone(),
                        source_root: None,
                    },
                    update_available: false,
                    group_id: Some(group.id.clone()),
                    last_operated_at: 0,
                },
            )
            .unwrap();
        let record = library.record("demo").unwrap();
        let path = library.skill_path_by_storage(&record.storage_name).unwrap();
        assert!(library.is_linked_source(&record.storage_name));
        assert_eq!(path.canonicalize().unwrap(), source.canonicalize().unwrap());
        fs::write(source.join("body.txt"), "changed").unwrap();
        assert_eq!(
            fs::read_to_string(path.join("body.txt")).unwrap(),
            "changed"
        );
        assert_eq!(crate::source::check_updates(&mut library).unwrap(), 0);
        let mut reopened = SkillLibrary::open_in(temp.path().join("data")).unwrap();
        assert_eq!(reopened.record("demo").unwrap().group_id, Some(group.id));
        reopened.remove_by_storage(&record.storage_name).unwrap();
        assert!(source.join("SKILL.md").is_file());
        assert!(path.symlink_metadata().is_err());
    }

    #[test]
    fn updating_legacy_local_copy_links_source_and_keeps_backup() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fixture_skill(&source, "demo");
        let mut library = SkillLibrary::open_in(temp.path().join("data")).unwrap();
        let group = library.create_group("local").unwrap();
        // Reproduce the copy and registry written by earlier versions.
        library
            .import(
                &source,
                SkillRecord {
                    name: "demo".into(),
                    storage_name: String::new(),
                    description: String::new(),
                    origin: SkillOrigin::Unknown,
                    update_available: false,
                    group_id: Some(group.id.clone()),
                    last_operated_at: 0,
                },
            )
            .unwrap();
        library.registry.skills.get_mut("demo").unwrap().origin = SkillOrigin::Local {
            path: source.clone(),
            source_root: None,
        };
        library.save().unwrap();
        let installed = library.skill_path_by_storage("demo").unwrap();
        fs::write(installed.join("local-edit.txt"), "keep me").unwrap();
        crate::source::update_by_storage(&mut library, "demo").unwrap();
        assert!(library.is_linked_source("demo"));
        assert_eq!(
            installed.canonicalize().unwrap(),
            source.canonicalize().unwrap()
        );
        assert_eq!(library.record("demo").unwrap().group_id, Some(group.id));
        let backup = library.registry.adopted_sources["demo"]
            .previous_library
            .as_ref()
            .unwrap();
        assert_eq!(
            fs::read_to_string(backup.join("local-edit.txt")).unwrap(),
            "keep me"
        );
        assert!(!source.join("local-edit.txt").exists());
    }

    #[test]
    fn trigger_override_survives_updates_and_follow_restores_the_new_source() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let source = temp.path().join("source");
        fixture_skill(&source, "demo");
        fs::create_dir_all(source.join("agents")).unwrap();
        fs::write(
            source.join("agents/openai.yaml"),
            "interface:\n  display_name: Original\n",
        )
        .unwrap();
        let mut library = SkillLibrary::open_in(&data_dir).unwrap();
        library
            .import(
                &source,
                SkillRecord {
                    name: "demo".into(),
                    storage_name: String::new(),
                    description: String::new(),
                    origin: SkillOrigin::Unknown,
                    update_available: false,
                    group_id: None,
                    last_operated_at: 0,
                },
            )
            .unwrap();

        library
            .set_trigger_mode_by_storage("demo", TriggerMode::Manual)
            .unwrap();
        let installed = library.skill_path_by_storage("demo").unwrap();
        assert!(
            fs::read_to_string(installed.join("SKILL.md"))
                .unwrap()
                .contains("disable-model-invocation: true")
        );
        assert!(
            fs::read_to_string(installed.join("agents/openai.yaml"))
                .unwrap()
                .contains("allow_implicit_invocation: false")
        );

        let updated_skill = "---\nname: demo\ndescription: updated\n---\nUpdated body\n";
        let updated_openai = "interface:\n  display_name: Updated\n";
        fs::write(source.join("SKILL.md"), updated_skill).unwrap();
        fs::write(source.join("agents/openai.yaml"), updated_openai).unwrap();
        let record = library.record_by_storage("demo").unwrap();
        library
            .replace_by_storage(&source, "demo".into(), record)
            .unwrap();
        assert!(
            fs::read_to_string(installed.join("SKILL.md"))
                .unwrap()
                .contains("disable-model-invocation: true")
        );

        library
            .set_trigger_mode_by_storage("demo", TriggerMode::FollowSkill)
            .unwrap();
        assert_eq!(
            fs::read_to_string(installed.join("SKILL.md")).unwrap(),
            updated_skill
        );
        assert_eq!(
            fs::read_to_string(installed.join("agents/openai.yaml")).unwrap(),
            updated_openai
        );
    }
}
