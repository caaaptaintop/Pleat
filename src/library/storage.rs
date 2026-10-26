//! Explicit, read-only storage previews followed by a revalidated transaction.
//! Private payloads are moved by directory rename; only metadata is inspected.
use super::*;
use crate::{directory_link, package_fs::Directory, update_protection::Node};
use std::{io::{Read, Write}, path::Component};

const ALIASES: &str = ".pleat-storage-migrations.json";
const PENDING: &str = ".pleat-storage-pending.json";
const MAX_RECORD: u64 = 4 * 1024 * 1024;

pub(super) fn ensure_no_pending(data_dir: &Path) -> Result<()> {
    match fs::symlink_metadata(data_dir.join(PENDING)) {
        Ok(_) => bail!("存储事务未完成，已停止加载和写入技能库；请按 {} 中的记录恢复配置与登记", data_dir.join(PENDING).display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("无法核验存储事务状态，未打开技能库"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageAction {
    MoveLibrary,
    UseLibrary,
    MoveAppData,
}

#[derive(Clone)]
pub struct StoragePlan {
    pub action: StorageAction,
    pub from: PathBuf,
    pub to: PathBuf,
    pub skill_count: usize,
    pub compatibility_alias: bool,
    frozen: Frozen,
}

#[derive(Clone, PartialEq, Eq)]
struct Frozen {
    action: StorageAction,
    from: PathBuf,
    to: PathBuf,
    skill_count: usize,
    data_dir: PathBuf,
    library_dir: PathBuf,
    state: Vec<u8>,
    revisions: [Option<String>; 2],
    source_tree: String,
    target_tree: Option<String>,
    target_identity: Option<String>,
    parents: [String; 2],
    data_identity: String,
    library_tree: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct Migration {
    from: PathBuf,
    to: PathBuf,
    identity: String,
    // Exact source directory identities, never a blanket canonicalize rule.
    sources: BTreeMap<PathBuf, String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Migrations {
    schema: u32,
    moves: Vec<Migration>,
}

#[cfg(test)]
thread_local! {
    static BEFORE_STORAGE_SAVE: RefCell<Option<Box<dyn FnOnce() -> Result<()>>>> = RefCell::new(None);
    static AFTER_EMPTY_PARKED: RefCell<Option<Box<dyn FnOnce(&Path)>>> = RefCell::new(None);
}

// macOS's system /var and /tmp aliases are the only implicit links accepted.
fn normalized(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() || path.components().any(|p| matches!(p, Component::ParentDir | Component::CurDir)) {
        bail!("存储位置必须是无 .. 的绝对路径");
    }
    #[cfg(target_os = "macos")]
    for alias in ["/var", "/tmp"] {
        if let Ok(rest) = path.strip_prefix(alias) {
            return Ok(Path::new("/private").join(alias.trim_start_matches('/')).join(rest));
        }
    }
    Ok(path.to_path_buf())
}

fn real_path(path: &Path, allow_missing: bool) -> Result<PathBuf> {
    let path = normalized(path)?;
    #[cfg(windows)]
    {
        let mut components = path.components();
        if matches!(components.next(), Some(Component::Prefix(_)))
            && !matches!(components.next(), Some(Component::RootDir))
        { bail!("存储位置必须包含完整的 Windows 根目录"); }
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        // A Windows prefix (including \\?\C:) is not yet its root directory.
        // Wait for RootDir before asking the filesystem about it.
        #[cfg(windows)]
        if matches!(component, Component::Prefix(_)) { continue; }
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if !metadata.is_dir() || directory_link::inspect(&current)?.is_some() {
                    bail!("存储位置及其父目录必须是实体目录：{}", current.display());
                }
            }
            Err(error) if allow_missing && current == path && error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error).context("无法核验存储目录"),
        }
    }
    Ok(path)
}

pub(crate) fn identity(path: &Path) -> Result<String> {
    let m = fs::symlink_metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(format!("{}:{}", m.dev(), m.ino()))
    }
    #[cfg(windows)]
    {
        use std::{os::windows::{fs::OpenOptionsExt, io::AsRawHandle}, ffi::c_void};
        #[repr(C)]
        struct Info { attributes: u32, times: [u32; 6], volume: u32, size_high: u32, size_low: u32, links: u32, index_high: u32, index_low: u32 }
        #[link(name = "kernel32")]
        unsafe extern "system" { fn GetFileInformationByHandle(h: *mut c_void, info: *mut Info) -> i32; }
        let file = fs::OpenOptions::new().read(true).custom_flags(0x02000000 | 0x00200000).open(path)?;
        let mut info: Info = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let _ = m;
        Ok(format!("{}:{}:{}", info.volume, info.index_high, info.index_low))
    }
    #[cfg(not(any(unix, windows)))]
    { let _ = m; bail!("此平台尚未支持可靠存储目录身份校验") }
}

fn same_volume(from: &Path, to_parent: &Path) -> Result<bool> {
    Ok(identity(from)?.split(':').next() == identity(to_parent)?.split(':').next())
}

fn tree_revision(root: &Path) -> Result<String> {
    let mut digest = Sha256::new();
    for entry in WalkDir::new(root).follow_links(false).sort_by_file_name().into_iter().filter_entry(|entry| {
        entry.path().strip_prefix(root).ok().is_none_or(|relative| !relative.starts_with(".pleat-storage-recovery"))
    }) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(root)?;
        // The cooperative write lock is not library content.
        if relative == Path::new("registry.lock") { continue; }
        let m = fs::symlink_metadata(entry.path())?;
        #[cfg(unix)]
        let metadata = {
            use std::os::unix::fs::MetadataExt;
            (m.mode(), if m.is_dir() { 0 } else { m.len() },
             if m.is_dir() { 0 } else { m.mtime() }, if m.is_dir() { 0 } else { m.mtime_nsec() },
             if m.is_dir() { 0 } else { m.ctime() }, if m.is_dir() { 0 } else { m.ctime_nsec() })
        };
        #[cfg(not(unix))]
        let metadata = (m.permissions().readonly(), if m.is_dir() { 0 } else { m.len() },
            if m.is_dir() { None } else { m.modified().ok() });
        let link = m.file_type().is_symlink().then(|| fs::read_link(entry.path())).transpose()?;
        digest.update(serde_json::to_vec(&(relative, identity(entry.path())?, metadata, link))?);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn small_file(path: &Path) -> Result<Option<Vec<u8>>> {
    let m = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !m.is_file() || m.file_type().is_symlink() || m.len() > MAX_RECORD {
        bail!("存储登记必须是有界实体文件：{}", path.display());
    }
    let directory = handle(path.parent().context("登记缺少父目录")?)?;
    let mut bytes = Vec::new();
    directory.read(Path::new(path.file_name().context("登记缺少名称")?))?
        .take(MAX_RECORD + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECORD { bail!("存储登记过大"); }
    Ok(Some(bytes))
}

fn migrations(root: &Path) -> Result<Migrations> {
    match small_file(&root.join(ALIASES))? {
        None => Ok(Migrations { schema: 1, moves: vec![] }),
        Some(bytes) => {
            let value: Migrations = serde_json::from_slice(&bytes).context("存储迁移记录无效")?;
            if value.schema != 1 || value.moves.len() > 128 { bail!("存储迁移记录版本或数量无效"); }
            Ok(value)
        }
    }
}

fn handle(path: &Path) -> Result<Directory> {
    #[cfg(unix)]
    { Directory::open_anchored(path) }
    #[cfg(not(unix))]
    { real_path(path, false)?; Directory::open(path) }
}

fn skill_records(root: &Path) -> Result<Vec<SkillRecord>> {
    let mut records = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let m = fs::symlink_metadata(&path)?;
        let linked = directory_link::inspect(&path)?.is_some();
        let source = if linked {
            let resolved = path.canonicalize().context("已有库包含失效链接，未接管")?;
            if !resolved.is_dir() { continue; }
            real_path(&resolved, false)?
        } else {
            if !m.is_dir() { continue; }
            path.clone()
        };
        let Some(_) = small_file(&source.join("SKILL.md"))? else { continue; };
        let storage_name = entry.file_name().into_string().map_err(|_| anyhow::anyhow!("技能目录名称不是 UTF-8"))?;
        validate_name(&storage_name)?;
        if storage_name.starts_with('.') { bail!("已有库包含隐藏技能目录，未接管"); }
        let origin = if storage_name == KITTER_SKILL_STORAGE {
            if linked { bail!("已有库的内置技能不能是目录链接，未接管"); }
            for (relative, expected) in [
                ("SKILL.md", KITTER_SKILL_MD), ("agents/openai.yaml", KITTER_OPENAI_YAML),
                ("references/install-cli.md", KITTER_INSTALL_CLI_MD),
            ] {
                if small_file(&path.join(relative))?.as_deref() != Some(expected.as_bytes()) {
                    bail!("已有库的内置技能同名内容冲突，未覆盖");
                }
            }
            SkillOrigin::Builtin
        } else { SkillOrigin::Unknown };
        records.push(SkillRecord {
            name: if origin.is_builtin() { KITTER_SKILL_NAME.into() } else { storage_name.clone() },
            storage_name, description: String::new(), origin, update_available: false,
            group_id: None, last_operated_at: 0,
        });
    }
    records.sort_by(|a, b| a.storage_name.cmp(&b.storage_name));
    Ok(records)
}

fn target_revision(root: &Path, action: StorageAction) -> Result<String> {
    let tree = tree_revision(root)?;
    if action != StorageAction::UseLibrary { return Ok(tree); }
    let mut linked = BTreeMap::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if directory_link::inspect(&entry.path())?.is_some() {
            let resolved = entry.path().canonicalize().context("已有库包含失效链接，未接管")?;
            if resolved.is_dir() {
                real_path(&resolved, false)?;
                let name = entry.file_name().into_string().map_err(|_| anyhow::anyhow!("已有库的链接名称不是 UTF-8"))?;
                linked.insert(name, (resolved.clone(), tree_revision(&resolved)?));
            }
        }
    }
    Ok(bytes_revision(&serde_json::to_vec(&(tree, linked))?))
}

fn skill_count(root: &Path) -> Result<usize> {
    let mut count = 0;
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() && path.join("SKILL.md").is_file() { count += 1; }
    }
    Ok(count)
}

impl SkillLibrary {
    pub fn plan_storage_change(&self, action: StorageAction, target: &Path) -> Result<StoragePlan> {
        ensure_no_pending(&self.data_dir)?;
        self.verify_disk_revisions()?;
        let data_dir = real_path(&self.data_dir, false)?;
        let library_dir = real_path(&self.config.library_dir, false)?;
        for file in ["config.json", "registry.json"] { small_file(&data_dir.join(file))?; }
        let from = if action == StorageAction::MoveAppData { data_dir.clone() } else { library_dir.clone() };
        let to = real_path(target, true)?;
        if to.components().any(|part| part.as_os_str() == ".pleat-storage-recovery") {
            bail!("恢复材料目录不能作为活动技能库");
        }
        if from == to || from.starts_with(&to) || to.starts_with(&from) {
            bail!("存储目标不能相同或互为父子目录");
        }
        if action != StorageAction::MoveAppData && data_dir.starts_with(&to) {
            bail!("技能库不能覆盖应用数据根或其父目录");
        }
        if action == StorageAction::MoveAppData
            && (to.parent() != from.parent() || to.file_name() != Some(std::ffi::OsStr::new("Pleat")))
        {
            bail!("应用数据迁移目标必须是当前数据根同级的 Pleat 目录");
        }
        let target_exists = fs::symlink_metadata(&to).is_ok();
        let count = if action == StorageAction::UseLibrary {
            if !target_exists { bail!("请选择已存在的非空技能库"); }
            let records = skill_records(&to)?;
            if records.is_empty() { bail!("目标不是非空技能库"); }
            records.len()
        } else {
            if target_exists && fs::read_dir(&to)?.next().is_some() { bail!("迁移目标非空，未覆盖任何内容"); }
            if !same_volume(&from, to.parent().context("目标缺少父目录")?)? {
                bail!("暂不支持跨盘迁移；所有目录均未改变");
            }
            if migrations(&from)?.moves.len() >= 128 { bail!("迁移记录数量已达上限，未迁移"); }
            skill_count(&library_dir)?
        };
        let source_tree = tree_revision(&from)?;
        let library_tree = if from == library_dir || library_dir.starts_with(&from) {
            source_tree.clone()
        } else { tree_revision(&library_dir)? };
        let frozen = Frozen {
            action, from: from.clone(), to: to.clone(), skill_count: count,
            state: serde_json::to_vec(&(&self.config, &self.registry))?,
            revisions: self.disk_revisions.lock().map_err(|_| anyhow::anyhow!("登记锁不可用"))?.clone(),
            source_tree,
            target_tree: target_exists.then(|| target_revision(&to, action)).transpose()?,
            target_identity: target_exists.then(|| identity(&to)).transpose()?,
            parents: [identity(from.parent().context("源缺少父目录")?)?, identity(to.parent().context("目标缺少父目录")?)?],
            data_identity: identity(&data_dir)?,
            library_tree, data_dir, library_dir,
        };
        Ok(StoragePlan { action, from, to, skill_count: count, compatibility_alias: action != StorageAction::UseLibrary, frozen })
    }

    pub fn apply_storage_change(&mut self, plan: &StoragePlan) -> Result<()> {
        if plan.action != plan.frozen.action || plan.from != plan.frozen.from || plan.to != plan.frozen.to
            || plan.skill_count != plan.frozen.skill_count || plan.compatibility_alias != (plan.action != StorageAction::UseLibrary)
        { bail!("存储预览已被更改，请重新预览"); }
        let _parent_lock = super::storage_parent_lock(&plan.frozen.data_dir)?;
        let data = handle(&plan.frozen.data_dir)?;
        #[cfg(unix)]
        let lock = data.lock(Path::new("registry.lock"))?;
        #[cfg(not(unix))]
        let lock = super::registry_file_lock(&plan.frozen.data_dir)?;
        #[allow(unused_mut)]
        let mut registry_lock = Some(lock);
        if self.plan_storage_change(plan.action, &plan.to)?.frozen != plan.frozen {
            bail!("存储预览已失效，请重新预览");
        }
        verify_location(&data, &plan.frozen.data_dir)?;
        let config_before = self.config.clone();
        let registry_before = self.registry.clone();
        let data_before = self.data_dir.clone();
        let (recovery, backup) = make_recovery(&data, &plan.frozen.data_dir)?;
        let files_before = [small_file(&plan.frozen.data_dir.join("config.json"))?, small_file(&plan.frozen.data_dir.join("registry.json"))?];
        for (name, bytes) in ["config.json", "registry.json"].iter().zip(&files_before) {
            if let Some(bytes) = bytes { private_file(&backup, Path::new(name), bytes)?; }
        }
        private_file(&backup, Path::new("plan.json"), &serde_json::to_vec_pretty(&serde_json::json!({
            "schema": 1, "from": plan.from, "to": plan.to, "action": format!("{:?}", plan.action),
            "source_identity": identity(&plan.from)?, "data_identity": plan.frozen.data_identity,
            "target_identity": plan.frozen.target_identity,
        }))?)?;

        if plan.action == StorageAction::UseLibrary {
            let result = (|| {
                let mut registry = Registry { source_groups_migrated: true, ..Registry::default() };
                for record in skill_records(&plan.to)? {
                    let source = record.origin.source();
                    let entry = registry.sources.entry(source.key()).or_insert_with(|| SkillSourceRecord {
                        source, discovered_skills: vec![], added_skills: vec![],
                    });
                    entry.discovered_skills.push(record.name.clone());
                    entry.added_skills.push(record.name.clone());
                    registry.skills.insert(record.storage_name.clone(), record);
                }
                if target_revision(&plan.to, plan.action)? != plan.frozen.target_tree.clone().context("目标库预览缺失")? {
                    bail!("目标库在切换前变化，请重新预览");
                }
                self.config.library_dir = plan.to.clone();
                self.registry = registry;
                self.save_storage(&data, registry_lock.as_deref().context("登记锁已释放，未提交配置")?, &recovery)
            })();
            if result.is_err() { self.config = config_before; self.registry = registry_before; }
            return result.with_context(|| format!("切换库失败；原配置恢复材料：{}", recovery.display()));
        }

        let source_parent = handle(plan.from.parent().context("源缺少父目录")?)?;
        let target_parent = handle(plan.to.parent().context("目标缺少父目录")?)?;
        let source = handle(&plan.from)?;
        let from_name = Path::new(plan.from.file_name().context("源缺少名称")?);
        let to_name = Path::new(plan.to.file_name().context("目标缺少名称")?);
        let aliases_before = small_file(&plan.from.join(ALIASES))?;
        if let Some(bytes) = &aliases_before { private_file(&backup, Path::new(ALIASES), bytes)?; }
        let mut records = migrations(&plan.from)?;
        let mut sources = BTreeMap::from([(PathBuf::new(), identity(&plan.from)?)]);
        if plan.frozen.library_dir.starts_with(&plan.from) {
            sources.insert(plan.frozen.library_dir.strip_prefix(&plan.from)?.to_path_buf(), identity(&plan.frozen.library_dir)?);
            for entry in fs::read_dir(&plan.frozen.library_dir)? {
                let path = entry?.path();
                if directory_link::inspect(&path)?.is_none() && path.is_dir() && path.join("SKILL.md").is_file() {
                    sources.insert(path.strip_prefix(&plan.from)?.to_path_buf(), identity(&path)?);
                }
            }
        }
        records.moves.push(Migration { from: plan.from.clone(), to: plan.to.clone(), identity: identity(&plan.from)?, sources });
        let aliases_after = serde_json::to_vec_pretty(&records)?;
        if aliases_after.len() as u64 > MAX_RECORD { bail!("迁移记录过大，未迁移"); }
        let mut parked = None;
        let mut moved = false;
        let mut alias_identity = None;
        let mut aliases_written = false;
        let result = (|| -> Result<()> {
            if plan.frozen.target_tree.is_some() {
                // Keep the selected empty directory's inode, including any late
                // writes through already-open handles. Never rmdir that inode.
                let (slot, slot_handle) = make_recovery(&target_parent, plan.to.parent().context("目标缺少父目录")?)?;
                private_file(&backup, Path::new("empty-target.json"), &serde_json::to_vec_pretty(&serde_json::json!({
                    "target": plan.to, "identity": plan.frozen.target_identity, "parked": slot.join("empty-target"),
                }))?)?;
                move_between(&target_parent, to_name, &slot_handle, Path::new("empty-target"), &plan.to, &slot.join("empty-target"))?;
                parked = Some((slot.join("empty-target"), slot_handle));
                #[cfg(test)]
                AFTER_EMPTY_PARKED.with(|hook| { if let Some(callback) = hook.borrow_mut().take() { callback(&parked.as_ref().unwrap().0); } });
                if Some(identity(&parked.as_ref().unwrap().0)?) != plan.frozen.target_identity
                    || fs::read_dir(&parked.as_ref().unwrap().0)?.next().is_some()
                { bail!("所选空目录出现并发内容，未移动技能库"); }
            }
            if identity(&plan.from)? != records.moves.last().context("迁移记录缺失")?.identity
                || tree_revision(&plan.from)? != plan.frozen.source_tree
                || (!plan.frozen.library_dir.starts_with(&plan.from) && tree_revision(&plan.frozen.library_dir)? != plan.frozen.library_tree)
                || identity(plan.from.parent().context("源父目录缺失")?)? != plan.frozen.parents[0]
                || identity(plan.to.parent().context("目标父目录缺失")?)? != plan.frozen.parents[1]
            {
                bail!("源目录或父目录在迁移前变化，未迁移");
            }
            // Windows cannot rename a directory with an open file in its tree.
            // The sibling lock still excludes every startup and new writer;
            // taking registry.lock above has already drained existing writers.
            #[cfg(windows)]
            if plan.action == StorageAction::MoveAppData { drop(registry_lock.take()); }
            move_between(&source_parent, from_name, &target_parent, to_name, &plan.from, &plan.to)
                .context("排他迁移失败；跨盘或目标冲突时原实体未改变")?;
            moved = true;
            verify_location(&source, &plan.to)?;
            verify_location(&source_parent, plan.from.parent().context("源父目录缺失")?)?;
            verify_location(&target_parent, plan.to.parent().context("目标父目录缺失")?)?;
            create_alias(&source_parent, from_name, &plan.to, &plan.from)?;
            alias_identity = Some(link_observation(&source_parent, from_name, &plan.from)?);
            write_storage_file(&source, &plan.to, Path::new(ALIASES), &aliases_after)?;
            aliases_written = true;
            if plan.action == StorageAction::MoveAppData {
                self.data_dir = plan.to.clone();
                if let Ok(relative) = config_before.library_dir.strip_prefix(&plan.from) {
                    self.config.library_dir = plan.to.join(relative);
                } else if let Ok(relative) = plan.frozen.library_dir.strip_prefix(&plan.from) {
                    self.config.library_dir = plan.to.join(relative);
                }
            } else { self.config.library_dir = plan.to.clone(); }
            #[cfg(windows)]
            if plan.action == StorageAction::MoveAppData {
                registry_lock = Some(super::registry_file_lock(&self.data_dir)?);
            }
            #[cfg(windows)]
            let data = handle(&self.data_dir)?;
            self.save_storage(&data, registry_lock.as_deref().context("登记锁已释放，未提交配置")?, &recovery)
        })();
        if let Err(error) = result {
            #[cfg(windows)]
            if plan.action == StorageAction::MoveAppData { drop(registry_lock.take()); }
            self.config = config_before;
            self.registry = registry_before;
            self.data_dir = data_before;
            let rollback = (|| -> Result<()> {
                if aliases_written {
                    if small_file(&plan.to.join(ALIASES))?.as_deref() != Some(aliases_after.as_slice()) {
                        bail!("迁移记录出现并发变化，保留恢复材料");
                    }
                    restore_file(&source, &plan.to, Path::new(ALIASES), aliases_before.as_deref())?;
                }
                if let Some(expected) = &alias_identity {
                    if link_observation(&source_parent, from_name, &plan.from)? != *expected {
                        bail!("旧入口出现并发变化，未删除它");
                    }
                    remove_alias(&source_parent, from_name, &plan.from)?;
                }
                if moved {
                    if identity(&plan.to)? != records.moves.last().context("迁移记录缺失")?.identity { bail!("迁移实体身份变化，未覆盖"); }
                    move_between(&target_parent, to_name, &source_parent, from_name, &plan.to, &plan.from)?;
                }
                if let Some((parked, slot)) = &parked {
                    move_between(slot, Path::new("empty-target"), &target_parent, to_name, parked, &plan.to)?;
                }
                Ok(())
            })();
            if let Err(rollback) = rollback {
                let moved_recovery = if plan.action == StorageAction::MoveAppData {
                    plan.to.join(recovery.strip_prefix(&plan.frozen.data_dir)?)
                } else { recovery.clone() };
                bail!("迁移失败：{error}；回滚受阻：{rollback}；恢复材料：{}（数据根已移动时：{}）", recovery.display(), moved_recovery.display());
            }
            return Err(error).with_context(|| format!("迁移已回滚；恢复材料：{}", recovery.display()));
        }
        Ok(())
    }

    fn save_storage(&self, directory: &Directory, _lock: &fs::File, recovery: &Path) -> Result<()> {
        verify_location(directory, &self.data_dir)?;
        ensure_no_pending(&self.data_dir)?;
        self.verify_disk_revisions()?;
        let names = [Path::new("config.json"), Path::new("registry.json")];
        let old = names.iter().map(|name| small_file(&self.data_dir.join(name))).collect::<Result<Vec<_>>>()?;
        let values = [serde_json::to_vec_pretty(&self.config)?, serde_json::to_vec_pretty(&self.registry)?];
        let mut revisions = self.disk_revisions.lock().map_err(|_| anyhow::anyhow!("登记锁不可用"))?;
        let pending = serde_json::to_vec_pretty(&serde_json::json!({"schema":1,"recovery":recovery,"library":self.config.library_dir}))?;
        private_file(directory, Path::new(PENDING), &pending)?;
        directory.sync()?;
        let clear_pending = || -> Result<()> {
            verify_location(directory, &self.data_dir)?;
            if small_file(&self.data_dir.join(PENDING))?.as_deref() != Some(pending.as_slice()) {
                bail!("存储事务标记发生变化，保留恢复材料");
            }
            directory.remove(Path::new(PENDING), &Node::File { sha256: String::new(), mode: 0o600 })
        };
        let mut completed = 0;
        let result = (|| -> Result<()> {
            for (index, name) in names.iter().enumerate() {
                verify_location(directory, &self.data_dir)?;
                #[cfg(test)]
                if index == 1 { BEFORE_STORAGE_SAVE.with(|hook| hook.borrow_mut().take().map(|callback| callback()).unwrap_or(Ok(())))?; }
                write_storage_file(directory, &self.data_dir, name, &values[index])?;
                completed += 1;
            }
            for (name, expected) in names.iter().zip(&values) {
                if small_file(&self.data_dir.join(name))?.as_deref() != Some(expected.as_slice()) {
                    bail!("存储元数据在提交后变化，保留事务标记");
                }
            }
            directory.sync()?;
            clear_pending()
        })();
        if let Err(error) = result {
            let rollback = (|| -> Result<()> {
                for done in (0..completed).rev() {
                    if small_file(&self.data_dir.join(names[done]))?.as_deref() != Some(values[done].as_slice()) {
                        bail!("保存失败且配置出现并发变化，未覆盖并发版本：{error}");
                    }
                    restore_file(directory, &self.data_dir, names[done], old[done].as_deref())?;
                }
                for (name, expected) in names.iter().zip(&old) {
                    if small_file(&self.data_dir.join(name))?.as_deref() != expected.as_deref() {
                        bail!("存储元数据回滚未能核验，保留事务标记");
                    }
                }
                directory.sync()?;
                clear_pending()
            })();
            if let Err(rollback) = rollback {
                bail!("保存失败：{error}；回滚未完成：{rollback}；事务标记与恢复材料已保留");
            }
            return Err(error);
        }
        *revisions = values.map(|bytes| Some(bytes_revision(&bytes)));
        Ok(())
    }
}

fn make_recovery(root: &Directory, root_path: &Path) -> Result<(PathBuf, Directory)> {
    let relative = Path::new(".pleat-storage-recovery");
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags};
        use std::os::unix::fs::MetadataExt;
        let mut parent = rustix::fs::open("/", OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty())?;
        for component in normalized(root_path)?.components() {
            if let Component::Normal(name) = component {
                parent = rustix::fs::openat(&parent, name, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())?;
            }
        }
        let parent: fs::File = parent.into();
        let m = parent.metadata()?;
        if format!("{}:{}", m.dev(), m.ino()) != root.identity()? { bail!("恢复材料父目录已变化"); }
        match rustix::fs::mkdirat(&parent, relative, Mode::from_raw_mode(0o700)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {},
            Err(error) => return Err(error.into()),
        }
        let recovery = rustix::fs::openat(&parent, relative, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())?;
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let name = format!("storage-{}-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(), SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        rustix::fs::mkdirat(&recovery, name.as_str(), Mode::from_raw_mode(0o700))?;
        let child = relative.join(name);
        return Ok((root_path.join(&child), root.child(&child)?));
    }
    #[cfg(not(unix))]
    {
    root.mkdir(relative)?;
    let recovery = root_path.join(relative);
    real_path(&recovery, false)?;
    let path = tempfile::Builder::new().prefix("storage-").tempdir_in(recovery)?.keep();
    let directory = handle(&path)?;
    Ok((path, directory))
    }
}

fn private_file(root: &Directory, name: &Path, bytes: &[u8]) -> Result<()> {
    #[cfg(unix)]
    let mode = 0o600;
    // Directory uses a readonly flag on Windows, not Unix permission bits.
    #[cfg(not(unix))]
    let mode = 0;
    let mut file = root.create_file(name, mode)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn verify_location(directory: &Directory, path: &Path) -> Result<()> {
    #[cfg(unix)]
    { directory.verify_location(path) }
    #[cfg(not(unix))]
    { let _ = directory; real_path(path, false).map(|_| ()) }
}

fn link_observation(parent: &Directory, name: &Path, path: &Path) -> Result<String> {
    #[cfg(unix)]
    { let _ = path; parent.observation(name) }
    #[cfg(not(unix))]
    { let _ = (parent, name); Ok(format!("{}:{:?}", identity(path)?, directory_link::inspect(path)?)) }
}

fn write_storage_file(directory: &Directory, root: &Path, name: &Path, bytes: &[u8]) -> Result<()> {
    #[cfg(unix)]
    { let _ = root; directory.write(name, bytes) }
    #[cfg(not(unix))]
    {
        let _ = directory;
        real_path(root, false)?;
        let path = root.join(name);
        small_file(&path)?;
        // Directory::write's non-Unix fallback truncates in place. Storage
        // transactions require atomic replacement on Windows as well.
        super::write_json_bytes(&path, bytes)
    }
}

fn restore_file(directory: &Directory, root: &Path, name: &Path, bytes: Option<&[u8]>) -> Result<()> {
    match bytes {
        Some(bytes) => write_storage_file(directory, root, name, bytes),
        None => directory.remove(name, &Node::File { sha256: String::new(), mode: 0o600 }),
    }
}

fn move_between(from: &Directory, from_name: &Path, to: &Directory, to_name: &Path, from_path: &Path, to_path: &Path) -> Result<()> {
    #[cfg(unix)]
    { let _ = (from_path, to_path); from.rename_to(from_name, to, to_name) }
    #[cfg(not(unix))]
    { let _ = (from, from_name, to, to_name); real_path(from_path.parent().context("源父目录缺失")?, false)?; real_path(to_path.parent().context("目标父目录缺失")?, false)?; crate::package_fs::rename_exclusive(from_path, to_path) }
}

fn create_alias(parent: &Directory, name: &Path, target: &Path, path: &Path) -> Result<()> {
    #[cfg(unix)]
    { let _ = path; parent.symlink(name, target.to_str().context("兼容路径不是 UTF-8")?, true) }
    #[cfg(not(unix))]
    { let _ = (parent, name); directory_link::create(target, path).map_err(Into::into) }
}

fn remove_alias(parent: &Directory, name: &Path, path: &Path) -> Result<()> {
    #[cfg(unix)]
    { let _ = path; parent.remove(name, &Node::Symlink { target: String::new(), directory: true }) }
    #[cfg(not(unix))]
    { let _ = (parent, name); directory_link::remove(path, directory_link::inspect(path)?.context("兼容入口缺失")?).map_err(Into::into) }
}

fn storage_path_spelling(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::{ffi::OsString, os::windows::ffi::{OsStrExt, OsStringExt}, path::Prefix};
        if matches!(path.components().next(), Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::VerbatimDisk(_))) {
            let wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
            return PathBuf::from(OsString::from_wide(&wide[4..]));
        }
    }
    path.to_path_buf()
}

fn alias_target_matches(actual: &Path, recorded: &Path) -> bool {
    // junction strips the Win32 verbatim drive prefix. Normalize only that
    // spelling; the root and individual source identities are still checked.
    storage_path_spelling(actual) == storage_path_spelling(recorded)
}

/// Resolve only identities listed by a controlled migration. An arbitrary
/// symlink to a source is deliberately insufficient to inherit marker rights.
pub(crate) fn migrated_source(source: &Path) -> Option<PathBuf> {
    fn resolve(source: &Path, visited: &mut BTreeSet<PathBuf>) -> Option<PathBuf> {
        if visited.len() >= 16 || !visited.insert(source.to_path_buf()) { return None; }
        if let Ok(real) = real_path(source, false) { return Some(real); }
        for ancestor in source.ancestors() {
            let canonical = ancestor.canonicalize().ok()?;
            let records = match migrations(&canonical) { Ok(records) => records, Err(_) => continue };
            for record in records.moves.iter().rev() {
                let source_spelling = storage_path_spelling(source);
                let from_spelling = storage_path_spelling(&record.from);
                let Ok(relative) = source_spelling.strip_prefix(&from_spelling) else { continue; };
                let Some(expected) = record.sources.get(relative) else { continue; };
                if directory_link::inspect(&record.from).ok().flatten().is_none_or(|link| !alias_target_matches(&link.target, &record.to)) { continue; }
                let mut branch = visited.clone();
                let Some(root) = resolve(&record.to, &mut branch) else { continue; };
                if !alias_target_matches(&canonical, &root) || identity(&root).ok().as_ref() != Some(&record.identity) { continue; }
                let target = canonical.join(relative);
                let target = match real_path(&target, false) {
                    Ok(target) => target,
                    Err(_) => match resolve(&target, &mut branch) {
                        Some(target) => target,
                        None => continue,
                    },
                };
                if real_path(&target, false).is_ok() && identity(&target).ok().as_ref() == Some(expected) {
                    return Some(target);
                }
            }
        }
        None
    }
    // Return None for ordinary paths as well, so the caller retains exact
    // original marker spelling unless there really was a controlled alias.
    if real_path(source, false).is_ok() { return None; }
    resolve(source, &mut BTreeSet::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn storage_released_locks_do_not_remain_held_by_forked_children() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        fs::create_dir(&root).unwrap();
        for kind in 0..3 {
            let acquire = || match kind {
                0 => super::super::storage_parent_lock(&root),
                1 => super::super::registry_file_lock(&root),
                _ => handle(&root).and_then(|directory| directory.lock(Path::new("registry.lock"))),
            };
            let held = acquire().unwrap();
            assert!(acquire().is_err());
            let mut pipe = [-1; 2];
            assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
            let child = unsafe { libc::fork() };
            if child < 0 {
                unsafe { libc::close(pipe[0]); libc::close(pipe[1]); }
                panic!("fork probe failed");
            }
            if child == 0 {
                // Only async-signal-safe calls after fork in this test binary.
                unsafe {
                    libc::close(pipe[1]);
                    let mut byte = 0u8;
                    let read = libc::read(pipe[0], (&mut byte as *mut u8).cast(), 1);
                    libc::_exit(if read == 1 { 0 } else { 74 });
                }
            }
            unsafe { libc::close(pipe[0]); }
            drop(held);
            let reacquired = acquire();
            let mut status = 0;
            unsafe {
                let byte = 1u8;
                libc::write(pipe[1], (&byte as *const u8).cast(), 1);
                libc::close(pipe[1]);
                libc::waitpid(child, &mut status, 0);
            }
            assert_eq!(status, 0);
            assert!(reacquired.is_ok(), "lock kind {kind} retained by child after owner scope ended");
        }
    }

    #[cfg(windows)]
    #[test]
    fn storage_alias_spelling_only_normalizes_verbatim_drive_prefixes() {
        assert!(alias_target_matches(Path::new(r"C:\library"), Path::new(r"\\?\C:\library")));
        assert!(alias_target_matches(Path::new(r"\\?\C:\library"), Path::new(r"C:\library")));
        assert!(!alias_target_matches(Path::new(r"C:\other"), Path::new(r"\\?\C:\library")));
        assert!(!alias_target_matches(Path::new(r"\\.\C:\library"), Path::new(r"\\?\C:\library")));
    }

    #[cfg(windows)]
    #[test]
    fn storage_windows_plain_records_resolve_canonical_source_paths() {
        let (temp, mut library) = fixture();
        library.config.library_dir = storage_path_spelling(&library.config.library_dir);
        library.save().unwrap();
        let source = library.config.library_dir.join("demo");
        let canonical_source = source.canonicalize().unwrap();
        let target = storage_path_spelling(&root(&temp).join("moved"));
        let plan = library.plan_storage_change(StorageAction::MoveLibrary, &target).unwrap();
        library.apply_storage_change(&plan).unwrap();
        let expected = target.join("demo").canonicalize().unwrap();
        assert_eq!(migrated_source(&source), Some(expected.clone()));
        assert_eq!(migrated_source(&canonical_source), Some(expected));
    }

    #[cfg(windows)]
    #[test]
    fn storage_windows_drive_and_verbatim_paths_validate_complete_roots() {
        let temp = tempfile::tempdir().unwrap();
        let canonical = temp.path().canonicalize().unwrap();
        let plain = PathBuf::from(canonical.to_str().unwrap().trim_start_matches(r"\\?\"));
        for path in [&canonical, &plain] {
            assert!(path.is_absolute());
            assert_eq!(real_path(path, false).unwrap(), *path);
            let missing = path.join("missing");
            assert_eq!(real_path(&missing, true).unwrap(), missing);
            assert!(real_path(&missing, false).is_err());
            assert_eq!(real_path(path.ancestors().last().unwrap(), false).unwrap(), path.ancestors().last().unwrap());
        }
        let prefix_only = Path::new(r"\\?\UNC\pleat-invalid-host\pleat-invalid-share");
        assert!(prefix_only.is_absolute());
        assert!(real_path(prefix_only, false).is_err());
        assert!(real_path(prefix_only, true).is_err());

        let real = canonical.join("real");
        let link = canonical.join("junction");
        fs::create_dir(&real).unwrap();
        directory_link::create(&real, &link).unwrap();
        assert!(real_path(&link, false).is_err());
        assert!(real_path(&link.join("missing"), true).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn storage_private_metadata_can_clear_the_pending_marker() {
        let temp = tempfile::tempdir().unwrap();
        let directory = handle(temp.path()).unwrap();
        private_file(&directory, Path::new(PENDING), b"fixture marker").unwrap();
        assert!(!fs::metadata(temp.path().join(PENDING)).unwrap().permissions().readonly());
        directory.remove(Path::new(PENDING), &Node::File { sha256: String::new(), mode: 0 }).unwrap();
        assert!(!temp.path().join(PENDING).exists());
    }

    fn fixture() -> (tempfile::TempDir, SkillLibrary) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let mut library = SkillLibrary::open_in(root.join("Kitter")).unwrap();
        let skill = library.config.library_dir.join("demo");
        fs::create_dir_all(skill.join("local-data/venv/bin")).unwrap();
        fs::write(skill.join("SKILL.md"), "---\nname: demo\n---\nFixture skill\n").unwrap();
        fs::write(skill.join("local-data/fixture.env"), "temporary fixture only").unwrap();
        fs::write(skill.join("local-data/venv/bin/python"), format!("#!{}/local-data/venv/bin/python\n", skill.display())).unwrap();
        library.registry.skills.insert("demo".into(), SkillRecord {
            name: "demo".into(), storage_name: "demo".into(), description: String::new(),
            origin: SkillOrigin::Npx { repository: "fixture/remote".into(), skill: "demo".into(), source_hash: None },
            update_available: true, group_id: None, last_operated_at: 7,
        });
        library.registry.package_baselines.insert("demo".into(), crate::update_protection::snapshot(&skill).unwrap());
        library.save().unwrap();
        (temp, library)
    }

    fn root(temp: &tempfile::TempDir) -> PathBuf { temp.path().canonicalize().unwrap() }

    fn saved(library: &SkillLibrary) -> [Vec<u8>; 2] {
        [fs::read(library.data_dir.join("config.json")).unwrap(), fs::read(library.data_dir.join("registry.json")).unwrap()]
    }

    fn fail_save() {
        BEFORE_STORAGE_SAVE.with(|hook| *hook.borrow_mut() = Some(Box::new(|| bail!("fixture: registry save failed"))));
    }

    fn existing_library(path: &Path, content: &str) {
        fs::create_dir_all(path.join("demo/local-data")).unwrap();
        fs::write(path.join("demo/SKILL.md"), content).unwrap();
        fs::write(path.join("demo/local-data/keep"), "target fixture").unwrap();
    }

    #[test]
    fn storage_interrupted_switch_never_exposes_old_remote_rights() {
        const CHILD_ROOT: &str = "PLEAT_STORAGE_CRASH_TEST_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = PathBuf::from(root);
            let mut library = SkillLibrary::open_in(root.join("Kitter")).unwrap();
            fs::write(root.join("registry-before-crash.json"), fs::read(library.data_dir.join("registry.json")).unwrap()).unwrap();
            let plan = library.plan_storage_change(StorageAction::UseLibrary, &root.join("existing")).unwrap();
            BEFORE_STORAGE_SAVE.with(|hook| *hook.borrow_mut() = Some(Box::new(|| std::process::exit(73))));
            library.apply_storage_change(&plan).unwrap();
            panic!("fixture child must exit between metadata commits");
        }
        let (temp, library) = fixture();
        let data = library.data_dir.clone();
        let target = root(&temp).join("existing");
        existing_library(&target, "different skill with the same name");
        let old_tree = tree_revision(&library.config.library_dir).unwrap();
        let target_tree = tree_revision(&target).unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "library::storage::tests::storage_interrupted_switch_never_exposes_old_remote_rights", "--nocapture"])
            .env(CHILD_ROOT, root(&temp)).output().unwrap();
        assert_eq!(child.status.code(), Some(73), "{}", String::from_utf8_lossy(&child.stderr));
        let old_registry: serde_json::Value = serde_json::from_slice(&fs::read(root(&temp).join("registry-before-crash.json")).unwrap()).unwrap();
        assert_eq!(old_registry["skills"]["demo"]["origin"]["type"], "npx");
        let partial: AppConfig = serde_json::from_slice(&fs::read(data.join("config.json")).unwrap()).unwrap();
        assert_eq!(partial.library_dir, target);
        assert!(serde_json::from_slice::<serde_json::Value>(&fs::read(data.join("registry.json")).unwrap()).unwrap() == old_registry, "registry permissions must still be the old version at the crash point");
        assert_eq!(tree_revision(&target).unwrap(), target_tree);
        assert_eq!(tree_revision(&library.config.library_dir).unwrap(), old_tree);
        assert!(data.join(".pleat-storage-recovery").is_dir());
        let pending: serde_json::Value = serde_json::from_slice(&fs::read(data.join(PENDING)).unwrap()).unwrap();
        let recovery = PathBuf::from(pending["recovery"].as_str().unwrap());
        assert!(recovery.join("plan.json").is_file());
        let backup: AppConfig = serde_json::from_slice(&fs::read(recovery.join("config.json")).unwrap()).unwrap();
        assert_eq!(backup.library_dir, library.config.library_dir);
        assert!(serde_json::from_slice::<serde_json::Value>(&fs::read(recovery.join("registry.json")).unwrap()).unwrap() == old_registry);
        let reopened = SkillLibrary::open_in(&data);
        assert!(reopened.is_err(), "an interrupted switch must not expose the new library with old remote origins");
        assert!(reopened.err().unwrap().to_string().contains("未完成"));
        assert_eq!(tree_revision(&target).unwrap(), target_tree);
        assert!(serde_json::from_slice::<serde_json::Value>(&fs::read(data.join("registry.json")).unwrap()).unwrap() == old_registry);
    }

    #[test]
    fn storage_pending_created_during_open_blocks_target_library_writes() {
        let (temp, library) = fixture();
        let target = root(&temp).join("existing");
        existing_library(&target, "different fixture");
        let before = tree_revision(&target).unwrap();
        let mut config = library.config.clone();
        config.library_dir = target.clone();
        fs::write(library.data_dir.join("config.json"), serde_json::to_vec(&config).unwrap()).unwrap();
        super::super::AFTER_CONFIG_READ.with(|hook| *hook.borrow_mut() = Some(|data: &Path| {
            fs::write(data.join(PENDING), b"fixture interrupted transaction").unwrap();
        }));
        assert!(SkillLibrary::open_in(&library.data_dir).is_err());
        assert_eq!(tree_revision(&target).unwrap(), before);
        assert!(!target.join(KITTER_SKILL_STORAGE).exists());
    }

    #[test]
    fn storage_concurrent_open_and_rollback_never_mix_library_and_origins() {
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}, mpsc::channel};
        let (temp, initial) = fixture();
        let mut library = SkillLibrary::open_in(initial.data_dir.clone()).unwrap();
        let target = root(&temp).join("existing");
        existing_library(&target, "different target fixture");
        super::super::ensure_builtin_skill(&target).unwrap();
        let target_before = tree_revision(&target).unwrap();
        let plan = library.plan_storage_change(StorageAction::UseLibrary, &target).unwrap();
        let (started_tx, started_rx) = channel();
        let (ready_tx, ready_rx) = channel();
        let (read_tx, read_rx) = channel();
        let (rolled_tx, rolled_rx) = channel();
        let data = library.data_dir.clone();
        let reader = std::thread::spawn(move || {
            super::super::BEFORE_STORAGE_OPEN_READ.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
                started_tx.send(()).unwrap();
                ready_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            })));
            super::super::AFTER_STORAGE_OPEN_READ.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
                // A writer rejected by the startup lock has already dropped
                // its receiver; it does not need the reader's cached snapshot.
                let _ = read_tx.send(());
                rolled_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            })));
            SkillLibrary::open_in(data).map(|opened| (opened.config.library_dir.clone(), opened.record_by_storage("demo").unwrap().origin))
        });
        started_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        let reached_commit = Arc::new(AtomicBool::new(false));
        let reached = reached_commit.clone();
        let write_ready = ready_tx.clone();
        BEFORE_STORAGE_SAVE.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            reached.store(true, Ordering::SeqCst);
            write_ready.send(()).unwrap();
            read_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            bail!("fixture registry commit failed");
        })));
        assert!(library.apply_storage_change(&plan).is_err());
        BEFORE_STORAGE_SAVE.with(|hook| { hook.borrow_mut().take(); });
        if !reached_commit.load(Ordering::SeqCst) { ready_tx.send(()).unwrap(); }
        rolled_tx.send(()).unwrap();
        let opened = reader.join().unwrap();
        if let Ok((path, origin)) = opened {
            assert!(!(path == target && matches!(origin, SkillOrigin::Npx { .. })), "concurrent open exposed a new library with old remote origins");
        }
        assert_eq!(tree_revision(&target).unwrap(), target_before);
        assert!(!library.data_dir.join(PENDING).exists());
    }

    #[test]
    fn storage_preview_is_read_only_and_public_fields_cannot_retarget_it() {
        let (temp, mut library) = fixture();
        let target = root(&temp).join("moved");
        let before = tree_revision(library.data_dir()).unwrap();
        let mut plan = library.plan_storage_change(StorageAction::MoveLibrary, &target).unwrap();
        assert_eq!(plan.skill_count, 2);
        assert!(plan.compatibility_alias);
        assert!(!target.exists());
        assert!(!library.data_dir.join(".pleat-storage-recovery").exists());
        assert_eq!(tree_revision(library.data_dir()).unwrap(), before);
        plan.to = root(&temp).join("different");
        assert!(library.apply_storage_change(&plan).is_err());
        assert!(!plan.to.exists());
    }

    #[test]
    fn storage_move_preserves_private_data_permissions_and_installed_links_round_trip() {
        let (temp, mut library) = fixture();
        let old = library.config.library_dir.clone();
        let old_id = identity(&old).unwrap();
        let runtime = old.join("demo/local-data/venv/bin/python");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&runtime, fs::Permissions::from_mode(0o751)).unwrap();
        }
        let runtime_id = identity(&runtime).unwrap();
        let runtime_bytes = fs::read(&runtime).unwrap();
        let install = root(&temp).join("installed-demo");
        directory_link::create(&old.join("demo"), &install).unwrap();
        let installed_target = directory_link::inspect(&install).unwrap().unwrap().target;
        let target = root(&temp).join("library-two");
        let plan = library.plan_storage_change(StorageAction::MoveLibrary, &target).unwrap();
        library.apply_storage_change(&plan).unwrap();
        assert_eq!(library.config.library_dir, target);
        assert_eq!(identity(&target).unwrap(), old_id);
        assert_eq!(identity(&target.join("demo/local-data/venv/bin/python")).unwrap(), runtime_id);
        assert_eq!(fs::read(&runtime).unwrap(), runtime_bytes);
        assert_eq!(fs::read(install.join("local-data/fixture.env")).unwrap(), b"temporary fixture only");
        assert_eq!(directory_link::inspect(&install).unwrap().unwrap().target, installed_target);
        #[cfg(unix)]
        { use std::os::unix::fs::PermissionsExt; assert_eq!(fs::metadata(&runtime).unwrap().permissions().mode() & 0o777, 0o751); }
        // An explicit temporary-fixture removal of the old alias allows moving
        // back. The storage API itself never removes a selected linked target.
        directory_link::remove(&old, directory_link::inspect(&old).unwrap().unwrap()).unwrap();
        let back = library.plan_storage_change(StorageAction::MoveLibrary, &old).unwrap();
        library.apply_storage_change(&back).unwrap();
        assert_eq!(identity(&old).unwrap(), old_id);
        assert_eq!(fs::read(&runtime).unwrap(), runtime_bytes);
        assert_eq!(fs::read(install.join("local-data/fixture.env")).unwrap(), b"temporary fixture only");
    }

    #[test]
    fn storage_empty_target_inode_is_parked_and_preserved() {
        let (temp, mut library) = fixture();
        let target = root(&temp).join("empty");
        fs::create_dir(&target).unwrap();
        let selected_id = identity(&target).unwrap();
        let old_id = identity(&library.config.library_dir).unwrap();
        let plan = library.plan_storage_change(StorageAction::MoveLibrary, &target).unwrap();
        library.apply_storage_change(&plan).unwrap();
        assert_eq!(identity(&target).unwrap(), old_id);
        let parked = WalkDir::new(root(&temp).join(".pleat-storage-recovery")).into_iter().filter_map(Result::ok)
            .find(|entry| entry.file_name() == "empty-target").unwrap();
        assert_eq!(identity(parked.path()).unwrap(), selected_id);
    }

    #[test]
    fn storage_empty_target_concurrent_write_is_restored_without_deletion() {
        let (temp, mut library) = fixture();
        let source = library.config.library_dir.clone();
        let target = root(&temp).join("empty");
        fs::create_dir(&target).unwrap();
        let target_id = identity(&target).unwrap();
        let plan = library.plan_storage_change(StorageAction::MoveLibrary, &target).unwrap();
        AFTER_EMPTY_PARKED.with(|hook| *hook.borrow_mut() = Some(Box::new(|parked| fs::write(parked.join("concurrent"), "keep late write").unwrap())));
        assert!(library.apply_storage_change(&plan).is_err());
        assert_eq!(identity(&target).unwrap(), target_id);
        assert_eq!(fs::read(target.join("concurrent")).unwrap(), b"keep late write");
        assert!(directory_link::inspect(&source).unwrap().is_none());
        assert!(source.join("demo/SKILL.md").is_file());
    }

    #[test]
    fn storage_rejects_conflicts_nested_and_symbolic_paths() {
        let (temp, mut library) = fixture();
        let source = library.config.library_dir.clone();
        for target in [&source, source.parent().unwrap(), &source.join("nested")] {
            assert!(library.plan_storage_change(StorageAction::MoveLibrary, target).is_err());
        }
        let nonempty = root(&temp).join("nonempty");
        fs::create_dir(&nonempty).unwrap();
        fs::write(nonempty.join("keep"), "unchanged").unwrap();
        assert!(library.plan_storage_change(StorageAction::MoveLibrary, &nonempty).is_err());
        let alias = root(&temp).join("alias");
        directory_link::create(&nonempty, &alias).unwrap();
        assert!(library.plan_storage_change(StorageAction::MoveLibrary, &alias).is_err());
        assert!(library.plan_storage_change(StorageAction::MoveLibrary, &alias.join("new")).is_err());
        let source_alias = root(&temp).join("source-alias");
        directory_link::create(&source, &source_alias).unwrap();
        library.config.library_dir = source_alias;
        assert!(library.plan_storage_change(StorageAction::MoveLibrary, &root(&temp).join("new")).is_err());
        assert_eq!(fs::read(nonempty.join("keep")).unwrap(), b"unchanged");
    }

    #[test]
    fn storage_rejects_stale_preview_for_files_config_registry_targets_and_parents() {
        for change in 0..6 {
            let (temp, mut library) = fixture();
            let parent = root(&temp).join("destination-parent");
            fs::create_dir(&parent).unwrap();
            let target = parent.join("new");
            let source = library.config.library_dir.clone();
            let plan = library.plan_storage_change(StorageAction::MoveLibrary, &target).unwrap();
            match change {
                0 => fs::write(source.join("demo/SKILL.md"), "changed after preview").unwrap(),
                1 => library.config.theme = crate::config::Theme::Dark,
                2 => fs::write(library.data_dir.join("registry.json"), b"external revision").unwrap(),
                3 => { fs::create_dir(&target).unwrap(); fs::write(target.join("concurrent"), "keep").unwrap(); },
                4 => { fs::rename(&parent, root(&temp).join("old-parent")).unwrap(); fs::create_dir(&parent).unwrap(); },
                _ => { library.registry.skills.get_mut("demo").unwrap().update_available = false; },
            }
            assert!(library.apply_storage_change(&plan).is_err(), "case {change}");
            assert!(directory_link::inspect(&source).unwrap().is_none());
            assert!(source.join("demo/SKILL.md").exists());
        }
    }

    #[test]
    fn storage_respects_existing_write_lock() {
        let (temp, mut library) = fixture();
        let target = root(&temp).join("target");
        let plan = library.plan_storage_change(StorageAction::MoveLibrary, &target).unwrap();
        let lock = library.write_lock().unwrap();
        assert!(library.apply_storage_change(&plan).is_err());
        assert!(!target.exists());
        drop(lock);
        library.apply_storage_change(&plan).unwrap();
    }

    #[test]
    fn storage_move_save_failure_rolls_back_both_files_memory_alias_and_empty_inode() {
        let (temp, mut library) = fixture();
        let source = library.config.library_dir.clone();
        let source_id = identity(&source).unwrap();
        let target = root(&temp).join("empty");
        fs::create_dir(&target).unwrap();
        let target_id = identity(&target).unwrap();
        let before = saved(&library);
        let plan = library.plan_storage_change(StorageAction::MoveLibrary, &target).unwrap();
        fail_save();
        assert!(format!("{:#}", library.apply_storage_change(&plan).unwrap_err()).contains("已回滚"));
        assert_eq!(library.config.library_dir, source);
        assert_eq!(identity(&source).unwrap(), source_id);
        assert_eq!(identity(&target).unwrap(), target_id);
        assert!(directory_link::inspect(&source).unwrap().is_none());
        assert!(!source.join(ALIASES).exists());
        assert_eq!(saved(&library), before);
        library.verify_disk_revisions().unwrap();
        assert!(library.data_dir.join(".pleat-storage-recovery").is_dir());
    }

    #[test]
    fn storage_app_data_move_preserves_registry_and_external_library_choice() {
        for external in [false, true] {
            for empty_target in [false, true] {
                let (temp, mut library) = fixture();
                if external {
                    let external_root = root(&temp).join("external-library");
                    fs::rename(&library.config.library_dir, &external_root).unwrap();
                    library.config.library_dir = external_root;
                    library.save().unwrap();
                }
                let old_data = library.data_dir.clone();
                let old_library = library.config.library_dir.clone();
                let data_id = identity(&old_data).unwrap();
                fs::write(old_data.join("AGENTS.md"), "fixture absolute paths remain byte-identical").unwrap();
                let registry_before = saved(&library)[1].clone();
                let target = root(&temp).join("Pleat");
                if empty_target { fs::create_dir(&target).unwrap(); }
                let plan = library.plan_storage_change(StorageAction::MoveAppData, &target).unwrap();
                #[cfg(windows)]
                {
                    let old_root = old_data.clone();
                    let new_root = target.clone();
                    BEFORE_STORAGE_SAVE.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
                        assert!(super::super::storage_parent_lock(&old_root).is_err());
                        assert!(super::super::registry_file_lock(&new_root).is_err());
                        Ok(())
                    })));
                }
                library.apply_storage_change(&plan).unwrap();
                assert_eq!(library.data_dir(), target);
                assert_eq!(identity(&target).unwrap(), data_id);
                assert_eq!(old_data.canonicalize().unwrap(), target);
                assert_eq!(library.config.library_dir, if external { old_library } else { target.join("skills") });
                assert_eq!(saved(&library)[1], registry_before);
                assert_eq!(fs::read(old_data.join("AGENTS.md")).unwrap(), b"fixture absolute paths remain byte-identical");
                assert!(target.join(".pleat-storage-recovery").is_dir());
                let reopened = SkillLibrary::open_in(&target).unwrap();
                assert_eq!(reopened.config.library_dir, library.config.library_dir);
            }
        }
    }

    #[test]
    fn storage_app_data_save_failure_restores_data_root_and_empty_target() {
        let (temp, mut library) = fixture();
        let old_data = library.data_dir.clone();
        let old_library = library.config.library_dir.clone();
        let before = saved(&library);
        let target = root(&temp).join("Pleat");
        fs::create_dir(&target).unwrap();
        let target_id = identity(&target).unwrap();
        let plan = library.plan_storage_change(StorageAction::MoveAppData, &target).unwrap();
        fail_save();
        let error = library.apply_storage_change(&plan).unwrap_err();
        assert!(format!("{error:#}").contains("fixture: registry save failed"), "{error:#}");
        assert_eq!(library.data_dir(), old_data);
        assert_eq!(library.config.library_dir, old_library);
        assert_eq!(saved(&library), before);
        assert_eq!(identity(&target).unwrap(), target_id);
        assert!(directory_link::inspect(&old_data).unwrap().is_none());
        assert!(!old_data.join(ALIASES).exists());
    }

    #[test]
    fn storage_use_library_never_inherits_same_named_remote_rights_and_keeps_backups() {
        let (temp, mut library) = fixture();
        let source = library.config.library_dir.clone();
        let before = saved(&library);
        let target = root(&temp).join("existing");
        existing_library(&target, "different same-named skill");
        let target_before = tree_revision(&target).unwrap();
        let old_before = tree_revision(&source).unwrap();
        let plan = library.plan_storage_change(StorageAction::UseLibrary, &target).unwrap();
        assert!(!plan.compatibility_alias);
        assert_eq!(plan.skill_count, 1);
        library.apply_storage_change(&plan).unwrap();
        assert_eq!(library.config.library_dir, target);
        assert_eq!(library.registry.skills["demo"].origin, SkillOrigin::Unknown);
        assert!(!library.registry.skills["demo"].update_available);
        assert!(library.registry.package_baselines.is_empty());
        assert!(library.registry.trigger_overrides.is_empty());
        assert!(library.registry.adopted_sources.is_empty());
        assert_eq!(tree_revision(&source).unwrap(), old_before);
        assert_eq!(tree_revision(&target).unwrap(), target_before);
        let backup = fs::read_dir(library.data_dir.join(".pleat-storage-recovery")).unwrap().next().unwrap().unwrap().path();
        assert_eq!(fs::read(backup.join("config.json")).unwrap(), before[0]);
        assert_eq!(fs::read(backup.join("registry.json")).unwrap(), before[1]);
    }

    #[test]
    fn storage_use_library_save_failure_restores_configuration_and_registry() {
        let (temp, mut library) = fixture();
        let old_library = library.config.library_dir.clone();
        let before = saved(&library);
        let state_before = serde_json::to_value(&library.registry).unwrap();
        let target = root(&temp).join("existing");
        existing_library(&target, "different fixture");
        let target_before = tree_revision(&target).unwrap();
        let plan = library.plan_storage_change(StorageAction::UseLibrary, &target).unwrap();
        fail_save();
        assert!(library.apply_storage_change(&plan).is_err());
        assert_eq!(library.config.library_dir, old_library);
        assert_eq!(serde_json::to_value(&library.registry).unwrap(), state_before);
        assert_eq!(saved(&library), before);
        assert_eq!(tree_revision(&target).unwrap(), target_before);
        assert!(!library.data_dir.join(PENDING).exists());
        library.verify_disk_revisions().unwrap();
    }

    #[test]
    fn storage_alias_requires_exact_migration_record_link_root_and_source_identity() {
        let (temp, mut library) = fixture();
        let old_source = library.config.library_dir.join("demo");
        let target = root(&temp).join("moved");
        let plan = library.plan_storage_change(StorageAction::MoveLibrary, &target).unwrap();
        library.apply_storage_change(&plan).unwrap();
        assert_eq!(migrated_source(&old_source), Some(target.join("demo")));
        let unapproved = root(&temp).join("unapproved");
        directory_link::create(&target, &unapproved).unwrap();
        assert_eq!(migrated_source(&unapproved.join("demo")), None);
        fs::rename(target.join("demo"), target.join("original-demo")).unwrap();
        existing_library(&target, "replacement directory");
        assert_eq!(migrated_source(&old_source), None);
        fs::remove_file(target.join(ALIASES)).unwrap();
        assert_eq!(migrated_source(&old_source), None);
    }

    #[test]
    fn storage_alias_chains_survive_app_data_then_library_move() {
        let (temp, mut library) = fixture();
        let old_source = library.config.library_dir.join("demo");
        let data_target = root(&temp).join("Pleat");
        let plan = library.plan_storage_change(StorageAction::MoveAppData, &data_target).unwrap();
        library.apply_storage_change(&plan).unwrap();
        let next_source = library.config.library_dir.join("demo");
        let library_target = root(&temp).join("relocated-library");
        let plan = library.plan_storage_change(StorageAction::MoveLibrary, &library_target).unwrap();
        library.apply_storage_change(&plan).unwrap();
        let expected = library_target.join("demo");
        assert_eq!(migrated_source(&old_source), Some(expected.clone()));
        assert_eq!(migrated_source(&next_source), Some(expected));
        let unapproved = root(&temp).join("unapproved");
        directory_link::create(&library_target, &unapproved).unwrap();
        let approved_link = next_source.parent().unwrap();
        directory_link::remove(approved_link, directory_link::inspect(approved_link).unwrap().unwrap()).unwrap();
        directory_link::create(&unapproved, approved_link).unwrap();
        assert_eq!(migrated_source(&old_source), None);
        directory_link::remove(approved_link, directory_link::inspect(approved_link).unwrap().unwrap()).unwrap();
        directory_link::create(&library_target, approved_link).unwrap();
        fs::rename(library_target.join("demo"), library_target.join("original-demo")).unwrap();
        existing_library(&library_target, "replacement directory");
        assert_eq!(migrated_source(&old_source), None);
    }

    #[test]
    fn storage_alias_chains_survive_library_then_app_data_move_and_failed_next_move() {
        let (temp, mut library) = fixture();
        let old_source = library.config.library_dir.join("demo");
        let first = library.data_dir.join("relocated-library");
        let plan = library.plan_storage_change(StorageAction::MoveLibrary, &first).unwrap();
        library.apply_storage_change(&plan).unwrap();
        let target = root(&temp).join("Pleat");
        let plan = library.plan_storage_change(StorageAction::MoveAppData, &target).unwrap();
        library.apply_storage_change(&plan).unwrap();
        let moved = target.join("relocated-library/demo");
        assert_eq!(migrated_source(&old_source), Some(moved.clone()));
        assert_eq!(migrated_source(&first.join("demo")), Some(moved.clone()));
        let before = fs::read(library.config.library_dir.join(ALIASES)).unwrap();
        let plan = library.plan_storage_change(StorageAction::MoveLibrary, &root(&temp).join("third-library")).unwrap();
        fail_save();
        assert!(library.apply_storage_change(&plan).is_err());
        assert_eq!(fs::read(library.config.library_dir.join(ALIASES)).unwrap(), before);
        assert_eq!(migrated_source(&old_source), Some(moved));
    }

    #[test]
    fn storage_use_library_keeps_linked_entities_unknown_and_rechecks_their_identity() {
        let (temp, mut library) = fixture();
        let target = root(&temp).join("existing");
        fs::create_dir(&target).unwrap();
        let external = root(&temp).join("external");
        existing_library(&external, "linked fixture");
        directory_link::create(&external.join("demo"), &target.join("demo")).unwrap();
        let plan = library.plan_storage_change(StorageAction::UseLibrary, &target).unwrap();
        fs::write(external.join("demo/SKILL.md"), "modified after preview").unwrap();
        assert!(library.apply_storage_change(&plan).is_err());
        let plan = library.plan_storage_change(StorageAction::UseLibrary, &target).unwrap();
        library.apply_storage_change(&plan).unwrap();
        assert_eq!(library.registry.skills["demo"].origin, SkillOrigin::Unknown);
        assert!(library.is_linked_source("demo"));
        assert!(library.registry.package_baselines.is_empty());
        assert!(directory_link::inspect(&target.join("demo")).unwrap().is_some());
        assert_eq!(fs::read(external.join("demo/local-data/keep")).unwrap(), b"target fixture");
    }

    #[test]
    fn storage_conflicting_late_target_keeps_both_its_files_and_selected_empty_inode() {
        let (temp, mut library) = fixture();
        let source = library.config.library_dir.clone();
        let target = root(&temp).join("empty");
        fs::create_dir(&target).unwrap();
        let target_id = identity(&target).unwrap();
        let plan = library.plan_storage_change(StorageAction::MoveLibrary, &target).unwrap();
        let concurrent_target = target.clone();
        AFTER_EMPTY_PARKED.with(|hook| *hook.borrow_mut() = Some(Box::new(move |_| {
            fs::create_dir(&concurrent_target).unwrap();
            fs::write(concurrent_target.join("late"), "keep late occupant").unwrap();
        })));
        let error = format!("{:#}", library.apply_storage_change(&plan).unwrap_err());
        assert!(error.contains("回滚受阻"));
        assert_eq!(library.config.library_dir, source);
        assert!(directory_link::inspect(&source).unwrap().is_none());
        assert_eq!(fs::read(target.join("late")).unwrap(), b"keep late occupant");
        let parked = WalkDir::new(root(&temp).join(".pleat-storage-recovery")).into_iter().filter_map(Result::ok)
            .find(|entry| entry.file_name() == "empty-target").unwrap();
        assert_eq!(identity(parked.path()).unwrap(), target_id);
        let backup = fs::read_dir(library.data_dir.join(".pleat-storage-recovery")).unwrap().next().unwrap().unwrap().path();
        let note: serde_json::Value = serde_json::from_slice(&fs::read(backup.join("empty-target.json")).unwrap()).unwrap();
        assert_eq!(note["parked"], serde_json::to_value(parked.path()).unwrap());
    }

    #[test]
    fn storage_interrupted_move_does_not_recreate_an_empty_library_on_open() {
        for move_data in [false, true] {
            let (temp, library) = fixture();
            fs::create_dir(library.data_dir.join(".pleat-storage-recovery")).unwrap();
            let target = root(&temp).join(if move_data { "Pleat" } else { "moved-library" });
            let old = if move_data { library.data_dir.clone() } else { library.config.library_dir.clone() };
            fs::rename(&old, &target).unwrap();
            let data = if move_data { target } else { library.data_dir.clone() };
            assert!(SkillLibrary::open_in(data).is_err());
            assert!(!old.exists());
            assert!(!library.config.library_dir.exists());
        }
    }
}
