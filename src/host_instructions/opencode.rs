//! Native config publication shares one CAS/ACL/recovery transaction.
//! JSONC instructions and YAML plugin opt-in retain separate text editors.
use std::{
    collections::BTreeMap,
    fmt, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use anyhow::{Context, Result, anyhow, ensure};
use sha2::{Digest, Sha256};
use crate::package_fs::Directory;
use super::{directory, jsonc, permissions, hermes_yaml, openclaw_json, agents_md};

const MAX_CONFIG: u64 = 2 * 1024 * 1024;

#[derive(Clone, PartialEq, Eq)]
struct Snapshot {
    text: String,
    identity: String,
    access: permissions::Access,
}
impl fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeConfigSnapshot").field("bytes", &self.text.len())
            .field("identity", &self.identity).field("access", &self.access).finish()
    }
}

fn identity(metadata: &fs::Metadata) -> String {
    #[cfg(unix)]
    { use std::os::unix::fs::MetadataExt; format!("{}:{}", metadata.dev(), metadata.ino()) }
    #[cfg(not(unix))]
    { format!("{:?}:{}", metadata.created().ok(), metadata.len()) }
}
fn snapshot(parent: &Directory, name: &Path) -> Result<Option<Snapshot>> {
    let mut file = match parent.read(name) {
        Ok(file) => file,
        Err(error) if crate::package_fs::is_not_found(&error) => return Ok(None),
        Err(_) => return Err(anyhow!("宿主配置不是可读取的普通文件，已保留")),
    };
    let before = file.metadata()?;
    let before_access = permissions::Access::read(&file)?;
    ensure!(before.is_file() && before.len() <= MAX_CONFIG, "宿主配置过大或类型无效，已保留");
    let mut bytes = Vec::new();
    (&mut file).take(MAX_CONFIG + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_CONFIG, "宿主配置超过读取上限，已保留");
    let after = file.metadata()?;
    let after_access = permissions::Access::read(&file)?;
    let current_file = parent.read(name)?;
    let current = current_file.metadata()?;
    let current_access = permissions::Access::read(&current_file)?;
    ensure!(identity(&before) == identity(&after) && identity(&after) == identity(&current)
        && before.len() == after.len() && before.modified().ok() == after.modified().ok()
        && before_access == after_access && after_access == current_access,
        "宿主配置在读取时变化，已保留");
    let text = String::from_utf8(bytes).map_err(|_| anyhow!("宿主配置编码无效，已保留"))?;
    Ok(Some(Snapshot { text, identity: identity(&after), access: after_access }))
}

#[derive(Debug)]
struct State {
    expected: Option<Snapshot>,
    parent_identity: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry { path: String, marker: String }

#[derive(Clone, Debug, PartialEq, Eq)]
struct InstallationSnapshot { logical: PathBuf, physical: PathBuf, identity: Option<String> }

impl InstallationSnapshot {
    fn read(logical: &Path) -> Result<Self> {
        let logical = std::path::absolute(logical)?;
        let physical = crate::cross_project::physical_entry(&logical)?;
        let identity = match fs::symlink_metadata(&logical) {
            Ok(_) => Some(crate::library::filesystem_identity(&logical)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        Ok(Self { logical, physical, identity })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Syntax { OpenCodeJsonc, HermesYaml, OpenClawJson5, DroidMarkdown, CopilotMarkdown }

impl Syntax {
    fn empty_document(self) -> &'static str {
        if matches!(self, Self::DroidMarkdown | Self::CopilotMarkdown) { "" } else { "{}" }
    }
}

#[derive(Clone, Debug)]
pub(super) struct ConfigPlan {
    pub(super) path: PathBuf,
    logical: PathBuf,
    anchor: PathBuf,
    anchor_identity: String,
    entries: Vec<Entry>,
    syntax: Syntax,
    filename: String,
    state: Arc<Mutex<State>>,
    removal_aliases: Vec<InstallationSnapshot>,
    added_entries: Vec<Entry>,
}

pub(super) type Pool = BTreeMap<PathBuf, ConfigPlan>;

impl ConfigPlan {
    pub(super) fn new_copilot_at(config: &Path, installation: &Path) -> Result<Self> {
        let installation = std::path::absolute(installation)?;
        let instruction = installation.join("local-data/ADAPTATION.md").to_str()
            .context("Copilot 适配路径编码无效")?.to_owned();
        let marker = format!("pleat-copilot-adaptation-v1:{:x}", Sha256::digest(instruction.as_bytes()));
        Self::document_at(config, "copilot-instructions.md", Syntax::CopilotMarkdown,
            Entry { path: instruction, marker }, true)
    }

    pub(super) fn guard_copilot_removal(&mut self, aliases: &[PathBuf]) -> Result<()> {
        ensure!(self.syntax == Syntax::CopilotMarkdown, "不支持此宿主别名校验");
        self.removal_aliases = aliases.iter().map(|path| InstallationSnapshot::read(path)).collect::<Result<_>>()?;
        if let Some(first) = self.removal_aliases.first() {
            ensure!(self.removal_aliases.iter().all(|entry|
                entry.physical == first.physical && entry.identity == first.identity),
                "Copilot 安装别名在预览时变化，已保留，请刷新");
        }
        Ok(())
    }

    fn validate_removal_aliases(&self) -> Result<()> {
        for expected in &self.removal_aliases {
            ensure!(InstallationSnapshot::read(&expected.logical)? == *expected,
                "Copilot 安装别名与预览归属不一致，已保留，请刷新");
        }
        Ok(())
    }

    pub(super) fn new_droid(installation: &Path) -> Result<Self> {
        let installation = std::path::absolute(installation)?;
        let factory = installation.parent().and_then(Path::parent).context("缺少 Droid 配置目录")?;
        ensure!(factory.file_name().is_some_and(|name| name == ".factory")
            && installation.parent().and_then(Path::file_name).is_some_and(|name| name == "skills"),
            "无法确认 Droid 技能安装目录，已保留");
        let project = factory.parent().context("缺少 Droid 项目目录")?;
        let global = dirs::home_dir().is_some_and(|home|
            crate::project_discovery::path_identity(&home) == crate::project_discovery::path_identity(project));
        Self::droid_at(&installation, global)
    }

    fn droid_at(installation: &Path, global: bool) -> Result<Self> {
        let factory = installation.parent().and_then(Path::parent).context("缺少 Droid 配置目录")?;
        let config = if global { factory } else { factory.parent().context("缺少 Droid 项目目录")? };
        let instruction = installation.join("local-data/ADAPTATION.md").to_str()
            .context("Droid 适配路径编码无效")?.to_owned();
        let marker = format!("pleat-droid-adaptation-v1:{:x}", Sha256::digest(instruction.as_bytes()));
        Self::document_at(config, "AGENTS.md", Syntax::DroidMarkdown, Entry { path: instruction, marker }, true)
    }

    pub(super) fn new(installation: &Path) -> Result<Self> {
        let installation = if installation.is_absolute() { installation.to_path_buf() }
            else { std::env::current_dir()?.join(installation) };
        let config = installation.parent().and_then(Path::parent).context("缺少 宿主配置目录")?;
        Self::new_at(config, &installation)
    }

    pub(super) fn new_at(config: &Path, installation: &Path) -> Result<Self> {
        let installation = if installation.is_absolute() { installation.to_path_buf() }
            else { std::env::current_dir()?.join(installation) };
        let instruction = installation.join("local-data/ADAPTATION.md").to_str()
            .context("OpenCode 适配路径编码无效")?.to_owned();
        let marker = format!("pleat-adaptation-v1:{:x}", Sha256::digest(instruction.as_bytes()));
        Self::document_at(config, "opencode.jsonc", Syntax::OpenCodeJsonc, Entry { path: instruction, marker }, true)
    }

    pub(super) fn new_hermes(profile: &Path, install: bool) -> Result<Self> {
        Self::document_at(profile, "config.yaml", Syntax::HermesYaml,
            Entry { path: super::hermes::PLUGIN_ID.to_owned(), marker: String::new() }, install)
    }

    pub(super) fn new_openclaw(path: &Path, id: &str, install: bool) -> Result<Self> {
        let parent = path.parent().context("缺少 OpenClaw 配置目录")?;
        let filename = path.file_name().and_then(|name| name.to_str()).context("OpenClaw 配置文件名无效")?;
        Self::document_at(parent, filename, Syntax::OpenClawJson5,
            Entry { path: id.to_owned(), marker: String::new() }, install)
    }

    fn document_at(config: &Path, filename: &str, syntax: Syntax, entry: Entry, install: bool) -> Result<Self> {
        let logical = config.join(filename);
        let path = crate::cross_project::physical_entry(&logical)?;
        let root = path.parent().context("缺少 宿主配置目录")?;
        let parent_identity = if root.exists() { Some(crate::library::filesystem_identity(root)?) } else { None };
        let expected = if root.exists() { snapshot(&directory(root)?, Path::new(filename))? } else { None };
        let text = expected.as_ref().map_or(syntax.empty_document(), |s| s.text.as_str());
        match syntax {
            Syntax::OpenCodeJsonc => { jsonc::contains(text, &entry.path)?; jsonc::is_managed(text, &entry.path, &entry.marker)?; }
            Syntax::HermesYaml => { if install { hermes_yaml::enable(text, &entry.path)?; } else { hermes_yaml::disable_owned(text, &entry.path)?; } }
            Syntax::OpenClawJson5 => { if install { openclaw_json::enable(text, &entry.path)?; } else { openclaw_json::disable_owned(text, &entry.path)?; } }
            Syntax::DroidMarkdown | Syntax::CopilotMarkdown => { agents_md::contains(text, &entry.path, &entry.marker)?; }
        }
        let mut anchor = root;
        while !anchor.exists() { anchor = anchor.parent().context("缺少 宿主配置锚点")?; }
        let anchor_identity = crate::library::filesystem_identity(anchor)?;
        Ok(Self { path: path.clone(), logical, anchor: anchor.to_path_buf(), anchor_identity,
            entries: vec![entry], syntax, filename: filename.to_owned(),
            state: Arc::new(Mutex::new(State { expected, parent_identity })),
            removal_aliases: Vec::new(), added_entries: Vec::new() })
    }

    pub(super) fn merge(&mut self, other: Self) -> Result<()> {
        ensure!(self.path == other.path && self.syntax == other.syntax, "不能合并不同原生配置");
        if !Arc::ptr_eq(&self.state, &other.state) {
            let a = self.state.lock().map_err(|_| anyhow!("原生配置状态不可用"))?;
            let b = other.state.lock().map_err(|_| anyhow!("原生配置状态不可用"))?;
            ensure!(a.expected == b.expected && a.parent_identity == b.parent_identity, "原生配置在预览时变化，请刷新");
            drop(a); drop(b);
        }
        for entry in other.entries { if !self.entries.contains(&entry) { self.entries.push(entry); } }
        for alias in other.removal_aliases { if !self.removal_aliases.contains(&alias) { self.removal_aliases.push(alias); } }
        Ok(())
    }

    pub(super) fn coalesce(&mut self, pool: &mut Pool) -> Result<()> {
        if let Some(previous) = pool.get(&self.path) {
            ensure!(self.syntax == previous.syntax, "同一原生配置涉及不同宿主格式，已保留");
            if !Arc::ptr_eq(&self.state, &previous.state) {
                let a = self.state.lock().map_err(|_| anyhow!("原生配置状态不可用"))?;
                let b = previous.state.lock().map_err(|_| anyhow!("原生配置状态不可用"))?;
                ensure!(a.expected == b.expected && a.parent_identity == b.parent_identity, "原生配置在预览时变化，请刷新");
                drop(a); drop(b);
                self.state = previous.state.clone();
            }
        } else { pool.insert(self.path.clone(), self.clone()); }
        Ok(())
    }

    pub(super) fn needs_install(&self) -> bool {
        self.state.lock().ok().is_none_or(|state| {
            let text = state.expected.as_ref().map_or(self.syntax.empty_document(), |s| s.text.as_str());
            self.entries.iter().any(|entry| match self.syntax {
                Syntax::OpenCodeJsonc => !jsonc::contains(text, &entry.path).unwrap_or(false),
                Syntax::HermesYaml => hermes_yaml::enable(text, &entry.path).map_or(true, |next| next != text),
                Syntax::OpenClawJson5 => openclaw_json::enable(text, &entry.path).map_or(true, |next| next != text),
                Syntax::DroidMarkdown | Syntax::CopilotMarkdown => !agents_md::contains(text, &entry.path, &entry.marker).unwrap_or(false),
            })
        })
    }

    fn validate_state(&self, state: &State) -> Result<()> {
        ensure!(crate::library::filesystem_identity(&self.anchor)? == self.anchor_identity
            && crate::cross_project::physical_entry(&self.logical)? == self.path,
            "宿主配置目录身份变化，请重新预览");
        let root = self.path.parent().context("缺少 宿主配置目录")?;
        let actual = if root.exists() {
            let parent = directory(root)?;
            ensure!(state.parent_identity.as_ref().is_none_or(|id| parent.identity().is_ok_and(|actual| &actual == id)),
                "宿主配置父目录已变化，未写入");
            let value = snapshot(&parent, Path::new(&self.filename))?;
            parent.verify_location(root)?;
            value
        } else { None };
        ensure!(actual == state.expected, "宿主配置与预览不一致，已保留");
        Ok(())
    }

    pub(super) fn validate(&self) -> Result<()> {
        self.validate_removal_aliases()?;
        let state = self.state.lock().map_err(|_| anyhow!("原生配置状态不可用"))?;
        self.validate_state(&state)
    }

    fn parent(&self, state: &mut State) -> Result<Directory> {
        #[cfg(test)]
        BEFORE_PARENT_OPEN.with(|hook| { if let Some(callback) = hook.borrow_mut().take() { callback(); } });
        let root = self.path.parent().context("缺少 宿主配置目录")?;
        let mut opened = directory(&self.anchor)?;
        ensure!(opened.identity()? == self.anchor_identity, "宿主配置锚点已变化，未写入");
        for component in root.strip_prefix(&self.anchor)?.components() {
            let name = Path::new(component.as_os_str());
            opened.mkdir(name)?;
            opened = opened.child(name)?;
        }
        let actual = opened.identity()?;
        ensure!(state.parent_identity.as_ref().is_none_or(|id| id == &actual), "宿主配置父目录已变化，未写入");
        opened.verify_location(root)?;
        state.parent_identity = Some(actual);
        Ok(opened)
    }

    pub(super) fn install(&mut self) -> Result<bool> { self.apply(true) }
    pub(super) fn detach(&mut self) -> Result<bool> { self.apply(false) }

    pub(super) fn rollback_install(&mut self) -> Result<bool> {
        if self.syntax != Syntax::CopilotMarkdown { return self.detach(); }
        let entries = self.added_entries.clone();
        let changed = self.apply_entries(false, &entries)?;
        self.added_entries.clear();
        Ok(changed)
    }

    fn apply(&mut self, install: bool) -> Result<bool> {
        let entries = self.entries.clone();
        self.apply_entries(install, &entries)
    }

    fn apply_entries(&mut self, install: bool, entries: &[Entry]) -> Result<bool> {
        self.validate_removal_aliases()?;
        let mut state = self.state.lock().map_err(|_| anyhow!("原生配置状态不可用"))?;
        self.validate_state(&state)?;
        let current = state.expected.as_ref().map_or(self.syntax.empty_document(), |s| s.text.as_str());
        let mut next = current.to_owned();
        let mut added = Vec::new();
        for entry in entries {
            let before = next.clone();
            next = match (self.syntax, install) {
                (Syntax::OpenCodeJsonc, true) => jsonc::add_managed(&next, &entry.path, &entry.marker)?,
                (Syntax::OpenCodeJsonc, false) => jsonc::remove_managed(&next, &entry.path, &entry.marker)?,
                (Syntax::HermesYaml, true) => hermes_yaml::enable(&next, &entry.path)?,
                (Syntax::HermesYaml, false) => hermes_yaml::disable_owned(&next, &entry.path)?,
                (Syntax::OpenClawJson5, true) => openclaw_json::enable(&next, &entry.path)?,
                (Syntax::OpenClawJson5, false) => openclaw_json::disable_owned(&next, &entry.path)?,
                (Syntax::DroidMarkdown | Syntax::CopilotMarkdown, true) => agents_md::enable(&next, &entry.path, &entry.marker)?,
                (Syntax::DroidMarkdown | Syntax::CopilotMarkdown, false) => agents_md::disable_owned(&next, &entry.path, &entry.marker)?,
            };
            if install && self.syntax == Syntax::CopilotMarkdown && next != before {
                added.push(entry.clone());
            }
        }
        if next == current { return Ok(false); }
        self.publish(&mut state, next)?;
        for entry in added { if !self.added_entries.contains(&entry) { self.added_entries.push(entry); } }
        Ok(true)
    }

    #[cfg(not(all(unix, any(target_os = "macos", target_os = "linux"))))]
    fn publish(&self, _state: &mut State, _next: String) -> Result<()> {
        anyhow::bail!("此平台尚未支持 OpenCode 原生配置受管写入")
    }

    #[cfg(all(unix, any(target_os = "macos", target_os = "linux")))]
    fn publish(&self, state: &mut State, next: String) -> Result<()> {
        let parent = self.parent(state)?;
        let root = self.path.parent().context("缺少 宿主配置目录")?;
        let name = Path::new(&self.filename);
        let _lock = parent.lock(Path::new(".pleat-instructions.lock"))?;
        ensure!(snapshot(&parent, name)? == state.expected, "宿主配置在执行前变化，已保留");
        let temporary = tempfile::Builder::new().prefix(".pleat-instruction-recovery-").make_in(root, |path| {
            parent.mkdir_exclusive(Path::new(path.file_name().unwrap())).map_err(std::io::Error::other)
        })?;
        let (recovery, retained) = temporary.into_parts();
        let retained = retained.keep()?;
        permissions::private_directory(&recovery)?;
        recovery.verify_location(&retained)?;
        let old = state.expected.clone();
        let previous = Path::new("previous.jsonc");
        let mut file = recovery.create_file(previous, 0o600)?;
        file.write_all(next.as_bytes())?;
        if let Some(old) = &old { old.access.apply(&file)?; }
        file.sync_all()?;
        let created_identity = identity(&file.metadata()?);
        let published_access = permissions::Access::read(&file)?;
        let mut note = recovery.create_file(Path::new("restore.json"), 0o600)?;
        note.write_all(&serde_json::to_vec(&serde_json::json!({
            "schema": 1, "target": self.path, "candidate_or_previous": "previous.jsonc",
            "previous_sha256": old.as_ref().map(|s| format!("{:x}", Sha256::digest(s.text.as_bytes()))),
            "previous_identity": old.as_ref().map(|s| &s.identity),
            "next_sha256": format!("{:x}", Sha256::digest(next.as_bytes())),
            "restore_policy": "Compare hashes and identities before restoring. The staged file is the candidate before publication or after rollback; it is the old config only after a completed exchange. Preserve both on any external change."
        }))?)?;
        note.sync_all()?;
        recovery.sync()?;
        parent.verify_location(root)?;
        recovery.verify_location(&retained)?;
        #[cfg(test)]
        BEFORE_EXCHANGE.with(|hook| { if let Some(callback) = hook.borrow_mut().take() { callback(); } });
        if old.is_some() { recovery.exchange_to(previous, &parent, name)?; }
        else { recovery.rename_to(previous, &parent, name)?; }
        #[cfg(test)]
        AFTER_EXCHANGE.with(|hook| { if let Some(callback) = hook.borrow_mut().take() { callback(); } });
        let published = Snapshot { text: next, identity: created_identity, access: published_access };
        let verify = (|| -> Result<()> {
            parent.verify_location(root)?;
            ensure!(snapshot(&parent, name)?.as_ref() == Some(&published), "宿主配置发布时发生并发修改");
            if old.is_some() { ensure!(snapshot(&recovery, previous)? == old, "宿主原配置在交换前发生并发修改"); }
            #[cfg(test)]
            FAIL_DURABILITY.with(|flag| { if flag.replace(false) { anyhow::bail!("injected native config durability failure"); } Ok(()) })?;
            parent.sync()?;
            Ok(())
        })();
        if let Err(error) = verify {
            // Restore only if our exact newly published inode is still there.
            // Otherwise retain both the user's current file and the old copy.
            let restore = (|| -> Result<bool> {
                parent.verify_location(root)?;
                ensure!(snapshot(&parent, name)?.as_ref() == Some(&published), "配置发布后又有修改，当前文件保留");
                #[cfg(test)]
                BEFORE_ROLLBACK_MOVE.with(|hook| { if let Some(callback) = hook.borrow_mut().take() { callback(); } });
                // NOREPLACE captures the actual object, not the earlier path
                // observation. Never exchange an unverified concurrent object.
                let observed = Path::new("rollback-observed.jsonc");
                parent.rename_to(name, &recovery, observed)?;
                // A raced object may be a symlink, directory, or unreadable
                // file. It is not our candidate; restore it without following.
                let actual = snapshot(&recovery, observed).ok().flatten();
                #[cfg(test)]
                AFTER_ROLLBACK_MOVE.with(|hook| { if let Some(callback) = hook.borrow_mut().take() { callback(); } });
                parent.verify_location(root)?;
                if actual.as_ref() != Some(&published) {
                    // This is a concurrent file, not our candidate. Put it
                    // back only while the active path remains absent.
                    recovery.rename_to(observed, &parent, name)?;
                    parent.sync()?;
                    return Ok(false);
                }
                if old.is_some() { recovery.rename_to(previous, &parent, name)?; }
                // A writer may have created a newer file while absent. The
                // NOREPLACE restoration above cannot overwrite that file.
                parent.sync()?;
                Ok(snapshot(&parent, name)? == old)
            })();
            anyhow::bail!("宿主适配配置失败；{}；恢复材料：{}；{error}",
                if matches!(restore, Ok(true)) { "已恢复原配置" } else { "保留并发配置及恢复版本，未覆盖新活动文件" }, retained.display());
        }
        state.expected = Some(published);
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    static BEFORE_PARENT_OPEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_EXCHANGE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_ROLLBACK_MOVE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static AFTER_ROLLBACK_MOVE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static AFTER_EXCHANGE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static FAIL_DURABILITY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(all(test, unix, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn copilot_rollback_keeps_existing_entries_and_refuses_concurrent_document_edits() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let one = root.join("skills/one");
        let two = root.join("skills/two");
        let mut existing = ConfigPlan::new_copilot_at(&root, &one).unwrap();
        existing.install().unwrap();
        let file = root.join("copilot-instructions.md");
        let before = fs::read(&file).unwrap();
        let mut combined = ConfigPlan::new_copilot_at(&root, &one).unwrap();
        combined.merge(ConfigPlan::new_copilot_at(&root, &two).unwrap()).unwrap();
        combined.install().unwrap();
        combined.rollback_install().unwrap();
        assert_eq!(fs::read(&file).unwrap(), before);
        combined.install().unwrap();
        let concurrent = fs::read_to_string(&file).unwrap() + "\nUSER_CONCURRENT\n";
        fs::write(&file, &concurrent).unwrap();
        assert!(combined.rollback_install().is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), concurrent);
    }

    #[test]
    fn copilot_personal_and_project_instructions_preserve_user_content_and_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for relative in ["custom-copilot-home", "project/.github"] {
            let config = root.join(relative);
            fs::create_dir_all(&config).unwrap();
            let installed = config.join("skills/demo");
            fs::create_dir_all(installed.join("local-data")).unwrap();
            fs::write(installed.join("local-data/ADAPTATION.md"), "PRIVATE_ADAPTATION_BYTES").unwrap();
            let file = config.join("copilot-instructions.md");
            let original = "# User\r\n中文说明  ";
            fs::write(&file, original).unwrap();
            fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
            let mut plan = ConfigPlan::new_copilot_at(&config, &installed).unwrap();
            assert_eq!(plan.path, file);
            assert!(plan.install().unwrap());
            assert!(!plan.install().unwrap());
            let text = fs::read_to_string(&file).unwrap();
            assert!(text.starts_with(original));
            assert!(text.contains("pleat-copilot-adaptation-v1:"));
            assert!(!text.contains("PRIVATE_ADAPTATION_BYTES"));
            assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o640);
            fs::write(&file, format!("{text}\nUser added tail\n")).unwrap();
            assert!(plan.detach().is_err());
            let mut refreshed = ConfigPlan::new_copilot_at(&config, &installed).unwrap();
            assert!(refreshed.detach().unwrap());
            assert_eq!(fs::read_to_string(&file).unwrap(), format!("{original}\nUser added tail\n"));
            assert_eq!(fs::read(installed.join("local-data/ADAPTATION.md")).unwrap(), b"PRIVATE_ADAPTATION_BYTES");
        }
    }

    #[test]
    fn copilot_rejects_symlink_or_edited_instruction_entries() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let installed = root.join("skills/demo");
        let file = root.join("copilot-instructions.md");
        fs::write(root.join("user.md"), "user bytes").unwrap();
        std::os::unix::fs::symlink(root.join("user.md"), &file).unwrap();
        assert!(ConfigPlan::new_copilot_at(&root, &installed).is_err());
        fs::remove_file(&file).unwrap();
        let mut plan = ConfigPlan::new_copilot_at(&root, &installed).unwrap();
        plan.install().unwrap();
        let edited = fs::read_to_string(&file).unwrap().replace("native file reading tool", "user changed this");
        fs::write(&file, &edited).unwrap();
        assert!(ConfigPlan::new_copilot_at(&root, &installed).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), edited);
        assert_eq!(fs::read(root.join("user.md")).unwrap(), b"user bytes");
    }

    #[test]
    fn droid_project_and_personal_entries_use_native_agents_files_and_preserve_user_bytes() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for global in [false, true] {
            let base = root.join(if global { "home" } else { "project" });
            let installation = base.join(".factory/skills/含 空格");
            fs::create_dir_all(installation.join("local-data")).unwrap();
            fs::write(installation.join("local-data/ADAPTATION.md"), "private-source-canary").unwrap();
            let file = if global { base.join(".factory/AGENTS.md") } else { base.join("AGENTS.md") };
            let before = "# User instructions\r\n保留原有内容和尾部空格  ";
            fs::write(&file, before).unwrap();
            fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
            let mut plan = ConfigPlan::droid_at(&installation, global).unwrap();
            assert_eq!(plan.path, file);
            assert!(plan.install().unwrap());
            let actual = fs::read_to_string(&file).unwrap();
            assert!(actual.starts_with(before));
            assert!(actual.contains(installation.join("local-data/ADAPTATION.md").to_str().unwrap()));
            assert!(!actual.contains("private-source-canary"));
            assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o640);
            fs::write(installation.join("local-data/ADAPTATION.md"), "updated-private-canary").unwrap();
            let mut reopened = ConfigPlan::droid_at(&installation, global).unwrap();
            assert!(!reopened.install().unwrap());
            assert!(reopened.detach().unwrap());
            assert_eq!(fs::read_to_string(&file).unwrap(), before);
            assert_eq!(fs::read_to_string(installation.join("local-data/ADAPTATION.md")).unwrap(), "updated-private-canary");
        }
        assert_eq!(ConfigPlan::new_droid(&root.join("project/.factory/skills/demo")).unwrap().path, root.join("project/AGENTS.md"));
        assert!(ConfigPlan::new_droid(&root.join("project/.other/skills/demo")).is_err());
    }

    #[test]
    fn droid_batch_pool_retains_other_installations_and_unmanaged_instructions() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let file = root.join("AGENTS.md");
        fs::write(&file, "user rule\n").unwrap();
        let mut a = ConfigPlan::new_droid(&root.join(".factory/skills/a")).unwrap();
        let mut b = ConfigPlan::new_droid(&root.join(".factory/skills/b")).unwrap();
        let mut pool = Pool::new();
        a.coalesce(&mut pool).unwrap();
        b.coalesce(&mut pool).unwrap();
        assert!(a.install().unwrap());
        assert!(b.install().unwrap());
        assert!(a.detach().unwrap());
        let remaining = fs::read_to_string(&file).unwrap();
        assert!(remaining.starts_with("user rule\n"));
        assert!(!agents_md::contains(&remaining, &a.entries[0].path, &a.entries[0].marker).unwrap());
        assert!(agents_md::contains(&remaining, &b.entries[0].path, &b.entries[0].marker).unwrap());
        assert!(b.detach().unwrap());
        assert_eq!(fs::read_to_string(&file).unwrap(), "user rule\n");
    }

    #[test]
    fn droid_rejects_symlink_user_file_and_external_changes_after_preview() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let installed = root.join(".factory/skills/demo");
        let file = root.join("AGENTS.md");
        fs::write(root.join("unrelated.md"), "user data").unwrap();
        std::os::unix::fs::symlink(root.join("unrelated.md"), &file).unwrap();
        assert!(ConfigPlan::new_droid(&installed).is_err());
        fs::remove_file(&file).unwrap();
        fs::write(&file, "initial user instruction").unwrap();
        let mut plan = ConfigPlan::new_droid(&installed).unwrap();
        fs::write(&file, "user changed after preview").unwrap();
        assert!(plan.install().is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "user changed after preview");
        assert_eq!(fs::read_to_string(root.join("unrelated.md")).unwrap(), "user data");
    }

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir(root.join(".opencode")).unwrap();
        (temp, root)
    }
    fn plan(root: &Path, name: &str) -> ConfigPlan {
        ConfigPlan::new(&root.join(".opencode/skills").join(name)).unwrap()
    }

    #[test]
    fn install_update_uninstall_preserves_user_bytes_and_recovers_old_config() {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, root) = setup();
        let config = root.join(".opencode/opencode.jsonc");
        let before = "// user header\n{\n  \"model\": \"preserved/model\", // keep model\n  \"instructions\": [\"user-rule.md\", /* user tail */ ],\n  \"opaque\": \"example-confidential-field\"\n}\n";
        fs::write(&config, before).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o640)).unwrap();
        let mut p = plan(&root, "含 空格");
        assert!(!format!("{p:?}").contains("example-confidential-field"));
        assert!(p.needs_install());
        assert!(p.install().unwrap());
        let native = fs::read_to_string(&config).unwrap();
        assert!(native.contains("// user header") && native.contains("// keep model") && native.contains("/* user tail */"));
        assert!(native.contains("preserved/model") && native.contains("user-rule.md"));
        assert!(jsonc::is_managed(&native, &p.entries[0].path, &p.entries[0].marker).unwrap());
        assert_eq!(fs::metadata(&config).unwrap().permissions().mode() & 0o777, 0o640);
        assert!(!p.install().unwrap());
        // A fresh plan recognizes ownership across application restarts.
        let mut reopened = plan(&root, "含 空格");
        assert!(reopened.detach().unwrap());
        let native = fs::read_to_string(&config).unwrap();
        assert!(!native.contains("pleat-adaptation-v1:"));
        assert!(native.contains("user-rule.md") && native.contains("preserved/model"));
        assert!(fs::read_dir(root.join(".opencode")).unwrap().filter_map(Result::ok).any(|e|
            e.file_name().to_string_lossy().starts_with(".pleat-instruction-recovery-")
            && fs::read_to_string(e.path().join("previous.jsonc")).ok().as_deref() == Some(before)));
    }

    #[test]
    fn external_same_instruction_is_not_adopted_or_removed() {
        let (_temp, root) = setup();
        let config = root.join(".opencode/opencode.jsonc");
        let mut p = plan(&root, "demo");
        let before = serde_json::json!({"instructions":[p.entries[0].path]}).to_string();
        fs::write(&config, &before).unwrap();
        p = plan(&root, "demo");
        assert!(!p.needs_install());
        assert!(!p.install().unwrap());
        assert!(!p.detach().unwrap());
        assert_eq!(fs::read_to_string(config).unwrap(), before);
    }

    #[test]
    fn shared_preview_tracks_multiple_skill_changes_and_only_removes_its_entry() {
        let (_temp, root) = setup();
        let mut a = plan(&root, "first");
        let mut b = plan(&root, "second");
        let mut pool = Pool::new();
        a.coalesce(&mut pool).unwrap(); b.coalesce(&mut pool).unwrap();
        assert!(a.install().unwrap());
        b.validate().unwrap();
        assert!(b.install().unwrap());
        a.validate().unwrap();
        assert!(a.detach().unwrap());
        let native = fs::read_to_string(&b.path).unwrap();
        assert!(!jsonc::contains(&native, &a.entries[0].path).unwrap());
        assert!(jsonc::contains(&native, &b.entries[0].path).unwrap());
        // Merging plans that already share a state must not self-deadlock.
        b.merge(b.clone()).unwrap();
        assert!(b.detach().unwrap());
    }

    #[test]
    fn preview_and_parent_replacement_never_overwrite_the_new_objects() {
        let (_temp, root) = setup();
        let config = root.join(".opencode/opencode.jsonc");
        fs::write(&config, "{}").unwrap();
        let mut p = plan(&root, "demo");
        fs::rename(&config, config.with_extension("old")).unwrap();
        fs::write(&config, "{\"user\":true}").unwrap();
        assert!(p.install().is_err());
        assert_eq!(fs::read_to_string(&config).unwrap(), "{\"user\":true}");
        let mut p = plan(&root, "demo");
        let old = root.join(".opencode"); let replaced = old.clone();
        BEFORE_PARENT_OPEN.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            fs::rename(&old, old.with_extension("saved")).unwrap();
            fs::create_dir(&old).unwrap();
        })));
        assert!(p.install().is_err());
        assert_eq!(fs::read_dir(replaced).unwrap().count(), 0);
    }

    #[test]
    fn replacement_between_validation_and_exchange_restores_the_exact_user_file() {
        let (_temp, root) = setup();
        let config = root.join(".opencode/opencode.jsonc");
        fs::write(&config, "{\"original\":true}").unwrap();
        let mut p = plan(&root, "demo");
        let concurrent = config.clone();
        BEFORE_EXCHANGE.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            fs::rename(&concurrent, concurrent.with_extension("user-saved")).unwrap();
            fs::write(&concurrent, "{\"concurrent\":true}").unwrap();
        })));
        assert!(p.install().is_err());
        assert_eq!(fs::read_to_string(&config).unwrap(), "{\"concurrent\":true}");
        assert_eq!(fs::read_to_string(config.with_extension("user-saved")).unwrap(), "{\"original\":true}");
    }

    #[test]
    fn changes_after_exchange_are_preserved_with_the_original_backup() {
        let (_temp, root) = setup();
        let config = root.join(".opencode/opencode.jsonc");
        let original = "{\"original\":true}";
        fs::write(&config, original).unwrap();
        let mut p = plan(&root, "demo");
        let concurrent = config.clone();
        AFTER_EXCHANGE.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            fs::write(concurrent, "{\"after-exchange-user\":true}").unwrap();
        })));
        assert!(p.install().is_err());
        assert_eq!(fs::read_to_string(&config).unwrap(), "{\"after-exchange-user\":true}");
        assert!(fs::read_dir(root.join(".opencode")).unwrap().filter_map(Result::ok).any(|e|
            e.file_name().to_string_lossy().starts_with(".pleat-instruction-recovery-")
            && fs::read_to_string(e.path().join("previous.jsonc")).ok().as_deref() == Some(original)));
    }

    #[test]
    fn durability_failure_restores_existing_or_absent_config_and_retry_succeeds() {
        for existed in [false, true] {
            let (_temp, root) = setup();
            let config = root.join(".opencode/opencode.jsonc");
            if existed { fs::write(&config, "{\"user\":true}").unwrap(); }
            let before = fs::read(&config).ok();
            let mut p = plan(&root, "demo");
            FAIL_DURABILITY.with(|flag| flag.set(true));
            assert!(p.install().is_err());
            assert_eq!(fs::read(&config).ok(), before);
            assert!(p.install().unwrap());
            assert!(p.detach().unwrap());
        }
    }

    #[test]
    fn config_symlink_is_not_followed_or_replaced() {
        let (_temp, root) = setup();
        let target = root.join("preserved.jsonc");
        fs::write(&target, "{}").unwrap();
        std::os::unix::fs::symlink(&target, root.join(".opencode/opencode.jsonc")).unwrap();
        assert!(ConfigPlan::new(&root.join(".opencode/skills/demo")).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "{}");
    }

    #[test]
    fn rollback_window_never_overwrites_in_place_or_replaced_user_configs() {
        for existed in [false, true] {
            for replace in [false, true] {
                let (_temp, root) = setup();
                let config = root.join(".opencode/opencode.jsonc");
                if existed { fs::write(&config, "{\"original\":true}").unwrap(); }
                let mut p = plan(&root, "demo");
                let concurrent = config.clone();
                BEFORE_ROLLBACK_MOVE.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
                    if replace { fs::rename(&concurrent, concurrent.with_extension("candidate-saved")).unwrap(); }
                    fs::write(concurrent, "{\"rollback-window-user\":true}").unwrap();
                })));
                FAIL_DURABILITY.with(|flag| flag.set(true));
                let error = p.install().unwrap_err().to_string();
                assert!(!error.contains("已恢复原配置"));
                assert_eq!(fs::read_to_string(&config).unwrap(), "{\"rollback-window-user\":true}");
                if existed {
                    assert!(fs::read_dir(root.join(".opencode")).unwrap().filter_map(Result::ok).any(|e|
                        e.file_name().to_string_lossy().starts_with(".pleat-instruction-recovery-")
                        && fs::read_to_string(e.path().join("previous.jsonc")).ok().as_deref() == Some("{\"original\":true}")));
                }
            }
        }
    }

    #[test]
    fn rollback_restore_preserves_a_new_active_file_created_during_isolation() {
        for existed in [false, true] {
            let (_temp, root) = setup();
            let config = root.join(".opencode/opencode.jsonc");
            if existed { fs::write(&config, "{\"original\":true}").unwrap(); }
            let mut p = plan(&root, "demo");
            let concurrent = config.clone();
            AFTER_ROLLBACK_MOVE.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
                fs::write(concurrent, "{\"newest-active-user\":true}").unwrap();
            })));
            FAIL_DURABILITY.with(|flag| flag.set(true));
            let error = p.install().unwrap_err().to_string();
            assert!(!error.contains("已恢复原配置"));
            assert_eq!(fs::read_to_string(&config).unwrap(), "{\"newest-active-user\":true}");
        }
    }

    #[cfg(target_os = "macos")]
    fn chmod_acl(path: &Path, action: &str, entry: Option<&str>) {
        let mut command = std::process::Command::new("/bin/chmod");
        command.arg(action);
        if let Some(entry) = entry { command.arg(entry); }
        assert!(command.arg(path).status().unwrap().success());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn inherited_acl_never_exposes_native_config_or_retained_candidates() {
        use std::os::unix::fs::PermissionsExt;
        for fail in [false, true] {
            let (_temp, root) = setup();
            let native = root.join(".opencode");
            chmod_acl(&native, "+a", Some("everyone allow read,execute,file_inherit,directory_inherit"));
            let config = native.join("opencode.jsonc");
            fs::write(&config, "{\"opaque\":\"owned-non-secret-fixture\"}").unwrap();
            chmod_acl(&config, "-N", None);
            fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
            let before = permissions::Access::read(&fs::File::open(&config).unwrap()).unwrap();
            assert!(!before.has_acl());
            let mut p = plan(&root, "demo");
            if fail { FAIL_DURABILITY.with(|flag| flag.set(true)); assert!(p.install().is_err()); }
            else { assert!(p.install().unwrap()); }
            let after = permissions::Access::read(&fs::File::open(&config).unwrap()).unwrap();
            assert_eq!(after, before);
            for recovery in fs::read_dir(&native).unwrap().filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().starts_with(".pleat-instruction-recovery-")) {
                let access = permissions::Access::read(&fs::File::open(recovery.path()).unwrap()).unwrap();
                assert_eq!(access.mode(), 0o700);
                assert!(!access.has_acl());
                for entry in fs::read_dir(recovery.path()).unwrap().filter_map(Result::ok) {
                    let access = permissions::Access::read(&fs::File::open(entry.path()).unwrap()).unwrap();
                    assert_eq!(access.mode() & 0o077, 0);
                    assert!(!access.has_acl());
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn intentional_acl_is_preserved_and_acl_change_after_preview_is_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, root) = setup();
        let config = root.join(".opencode/opencode.jsonc");
        fs::write(&config, "{\"user\":true}").unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o640)).unwrap();
        chmod_acl(&config, "+a", Some("everyone allow read"));
        let before = permissions::Access::read(&fs::File::open(&config).unwrap()).unwrap();
        assert!(before.has_acl());
        let mut p = plan(&root, "demo");
        assert!(p.install().unwrap());
        let after = permissions::Access::read(&fs::File::open(&config).unwrap()).unwrap();
        assert_eq!(after, before);
        let mut p = plan(&root, "other");
        let contents = fs::read(&config).unwrap();
        chmod_acl(&config, "-N", None);
        assert!(p.install().is_err());
        assert_eq!(fs::read(&config).unwrap(), contents);
    }


    #[test]
    fn rollback_restores_a_concurrent_symlink_without_following_or_claiming_it() {
        let (_temp, root) = setup();
        let config = root.join(".opencode/opencode.jsonc");
        fs::write(&config, "{}").unwrap();
        let outside = root.join("user-config.jsonc");
        fs::write(&outside, "{\"user-owned\":true}").unwrap();
        let mut p = plan(&root, "demo");
        let concurrent = config.clone(); let source = outside.clone();
        BEFORE_ROLLBACK_MOVE.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            fs::rename(&concurrent, concurrent.with_extension("candidate-saved")).unwrap();
            std::os::unix::fs::symlink(source, concurrent).unwrap();
        })));
        FAIL_DURABILITY.with(|flag| flag.set(true));
        let error = p.install().unwrap_err().to_string();
        assert!(!error.contains("已恢复原配置"));
        assert_eq!(fs::read_link(config).unwrap(), outside);
        assert_eq!(fs::read_to_string(outside).unwrap(), "{\"user-owned\":true}");
    }

}
