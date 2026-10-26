// Package operations must not follow a replaced parent directory into private
// data. On Unix each component is resolved relative to an open directory fd.
use crate::update_protection::{Manifest, Node};

#[cfg(test)]
thread_local! {
    static AFTER_PARENT_OPEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
}
use anyhow::{Context, Result, bail};
use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path},
};

#[cfg(unix)]
use rustix::fs::{AtFlags, FileType, Mode, OFlags};

pub(crate) struct Directory {
    #[cfg(unix)]
    file: fs::File,
    #[cfg(not(unix))]
    path: std::path::PathBuf,
}

#[cfg(unix)]
impl std::os::fd::AsFd for Directory {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> { std::os::fd::AsFd::as_fd(&self.file) }
}

pub(crate) struct FileLock {
    file: fs::File,
    owner: u32,
}

impl FileLock {
    pub(crate) fn from_locked(file: fs::File) -> Self {
        Self { file, owner: std::process::id() }
    }
}

impl std::ops::Deref for FileLock {
    type Target = fs::File;
    fn deref(&self) -> &Self::Target { &self.file }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Closing alone can leave a fork-inherited description holding flock.
        // Only the acquiring process ends this guard's lock ownership.
        if self.owner == std::process::id() { let _ = self.file.unlock(); }
    }
}

pub(crate) fn is_not_found(error: &anyhow::Error) -> bool {
    #[cfg(unix)]
    if error.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::NOENT) {
        return true;
    }
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

// A destination created after a preflight check must never be replaced, even
// during rollback. Unix parents are resolved through directory descriptors.
pub(crate) fn rename_exclusive(source: &Path, destination: &Path) -> Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let root = Directory::open(Path::new("/"))?;
        fn absolute(path: &Path) -> Result<std::path::PathBuf> {
            let path = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir()?.join(path) };
            #[cfg(target_os = "macos")]
            for alias in ["/var", "/tmp"] {
                if let Ok(rest) = path.strip_prefix(alias) {
                    return Ok(Path::new("/private").join(alias.trim_start_matches('/')).join(rest));
                }
            }
            Ok(path)
        }
        let source = absolute(source)?;
        let (from, from_name) = root.parent(source.strip_prefix("/")?, false)?;
        let target = absolute(destination)?;
        let (to, to_name) = root.parent(target.strip_prefix("/")?, false)?;
        rustix::fs::renameat_with(&from, from_name.as_str(), &to, to_name.as_str(), rustix::fs::RenameFlags::NOREPLACE)?;
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileW(existing: *const u16, new: *const u16) -> i32;
        }
        let from: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = destination.as_os_str().encode_wide().chain(Some(0)).collect();
        if unsafe { MoveFileW(from.as_ptr(), to.as_ptr()) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    bail!("此平台尚未支持排他移动，未覆盖任何目标")
}

impl Directory {
    /// Flush directory entries on Unix using the already verified descriptor.
    /// Windows storage files are flushed individually before atomic replacement.
    pub(crate) fn sync(&self) -> Result<()> {
        #[cfg(unix)]
        self.file.sync_all()?;
        Ok(())
    }

    /// Resolve every absolute component without following directory links.
    /// Proma writes retain these descriptors throughout a transaction.
    pub(crate) fn open_anchored(path: &Path) -> Result<Self> {
        #[cfg(unix)]
        {
            if !path.is_absolute() { bail!("目录必须是绝对路径"); }
            #[cfg(target_os = "macos")]
            let normalized = ["/var", "/tmp"].into_iter().find_map(|alias| path.strip_prefix(alias).ok().map(|tail| Path::new("/private").join(alias.trim_start_matches('/')).join(tail))).unwrap_or_else(|| path.to_path_buf());
            #[cfg(target_os = "macos")]
            let path = normalized.as_path();
            let mut directory = Self::open(Path::new("/"))?;
            for component in path.components() {
                match component {
                    Component::RootDir => {},
                    Component::Normal(name) => directory = directory.child(Path::new(name))?,
                    _ => bail!("目录必须是安全绝对路径"),
                }
            }
            Ok(directory)
        }
        #[cfg(not(unix))]
        bail!("此平台尚未支持 Proma 受管写入")
    }

    pub(crate) fn child(&self, path: &Path) -> Result<Self> {
        #[cfg(unix)]
        {
            let (parent, name) = self.parent(path, false)?;
            let fd = rustix::fs::openat(&parent, name.as_str(), OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())?;
            Ok(Self { file: fd.into() })
        }
        #[cfg(not(unix))]
        { let _ = path; bail!("此平台尚未支持 Proma 受管写入") }
    }

    pub(crate) fn identity(&self) -> Result<String> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let m = self.file.metadata()?;
            Ok(format!("{}:{}", m.dev(), m.ino()))
        }
        #[cfg(not(unix))]
        bail!("此平台尚未支持 Proma 受管写入")
    }

    pub(crate) fn verify_location(&self, path: &Path) -> Result<()> {
        if Self::open_anchored(path)?.identity()? != self.identity()? { bail!("目录身份发生变化，未执行写入"); }
        Ok(())
    }

    pub(crate) fn lock(&self, name: &Path) -> Result<FileLock> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let (parent, name) = self.parent(name, false)?;
            let file: fs::File = rustix::fs::openat(&parent, name.as_str(), OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK, Mode::from_raw_mode(0o600))?.into();
            if !file.metadata()?.is_file() { bail!("Proma 工作区锁不是普通文件"); }
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 { bail!("Proma 工作区已有写操作，请稍后重试"); }
            Ok(FileLock::from_locked(file))
        }
        #[cfg(not(unix))]
        { let _ = name; bail!("此平台尚未支持 Proma 受管写入") }
    }

    pub(crate) fn rename_to(&self, from: &Path, destination: &Self, to: &Path) -> Result<()> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let (from_parent, from_name) = self.parent(from, false)?;
            let (to_parent, to_name) = destination.parent(to, false)?;
            rustix::fs::renameat_with(&from_parent, from_name.as_str(), &to_parent, to_name.as_str(), rustix::fs::RenameFlags::NOREPLACE)?;
            Ok(())
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        { let _ = (from, destination, to); bail!("此平台尚未支持 Proma 受管写入") }
    }

    /// Atomic exchange keeps the previous native configuration available for
    /// identity validation and recovery without a missing-file window.
    pub(crate) fn exchange_to(&self, from: &Path, destination: &Self, to: &Path) -> Result<()> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let (from_parent, from_name) = self.parent(from, false)?;
            let (to_parent, to_name) = destination.parent(to, false)?;
            rustix::fs::renameat_with(&from_parent, from_name.as_str(), &to_parent, to_name.as_str(), rustix::fs::RenameFlags::EXCHANGE)?;
            Ok(())
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        { let _ = (from, destination, to); bail!("此平台尚未支持原生配置原子交换"); }
    }

    pub(crate) fn observation(&self, path: &Path) -> Result<String> {
        #[cfg(unix)]
        {
            use sha2::{Digest, Sha256};
            let (parent, name) = self.parent(path, false)?;
            let stat = match rustix::fs::statat(&parent, name.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => stat,
                Err(rustix::io::Errno::NOENT) => return Ok("absent".into()),
                Err(error) => return Err(error.into()),
            };
            let identity = format!("{}:{}", stat.st_dev, stat.st_ino);
            let bytes = match FileType::from_raw_mode(stat.st_mode) {
                FileType::Symlink => serde_json::to_vec(&(identity, rustix::fs::readlinkat(&parent, name.as_str(), Vec::new())?.to_str().context("链接目标不是 UTF-8")?))?,
                FileType::Directory => {
                    let child = self.child(path)?;
                    serde_json::to_vec(&(child.identity()?, child.installation_snapshot()?))?
                }
                _ => bail!("安装位置不是技能目录"),
            };
            Ok(format!("{:x}", Sha256::digest(bytes)))
        }
        #[cfg(not(unix))]
        { let _ = path; bail!("此平台尚未支持 Proma 受管写入") }
    }

    pub(crate) fn directory_names(&self) -> Result<Vec<String>> {
        #[cfg(unix)]
        {
            let mut directory = rustix::fs::Dir::read_from(&self.file)?;
            let mut names = Vec::new();
            while let Some(entry) = directory.read() {
                let entry = entry?;
                let name = entry.file_name();
                let label = name.to_str().context("目录名称不是 UTF-8")?;
                if matches!(label, "." | "..") { continue; }
                let stat = rustix::fs::statat(&self.file, name, AtFlags::SYMLINK_NOFOLLOW)?;
                if FileType::from_raw_mode(stat.st_mode) == FileType::Directory { names.push(label.to_owned()); }
            }
            names.sort();
            Ok(names)
        }
        #[cfg(not(unix))]
        bail!("此平台尚未支持 Proma 受管写入")
    }
    pub(crate) fn open(path: &Path) -> Result<Self> {
        #[cfg(unix)]
        {
            let fd = rustix::fs::open(
                path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            Ok(Self { file: fd.into() })
        }
        #[cfg(not(unix))]
        {
            if !fs::symlink_metadata(path)?.is_dir() {
                bail!("包目录不是实体目录");
            }
            Ok(Self { path: path.into() })
        }
    }

    #[cfg(unix)]
    fn parent(&self, path: &Path, create: bool) -> Result<(fs::File, String)> {
        let mut parts = Vec::new();
        for part in path.components() {
            let Component::Normal(value) = part else {
                bail!("包文件路径必须是安全相对路径");
            };
            parts.push(value.to_str().context("包路径不是 UTF-8")?.to_owned());
        }
        let name = parts.pop().context("包文件路径为空")?;
        let mut parent = self.file.try_clone()?;
        for part in &parts {
            if create {
                match rustix::fs::mkdirat(&parent, part.as_str(), Mode::from_raw_mode(0o755)) {
                    Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            parent = rustix::fs::openat(
                &parent,
                part.as_str(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?
            .into();
        }
        #[cfg(test)]
        AFTER_PARENT_OPEN.with(|hook| {
            let callback = hook.borrow_mut().take();
            if let Some(callback) = callback {
                callback();
            }
        });
        // Validate the visible chain again, but perform the operation through
        // the already-open fd. A later rename cannot redirect it to a target.
        let mut visible = self.file.try_clone()?;
        for part in &parts {
            visible = rustix::fs::openat(
                &visible,
                part.as_str(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?
            .into();
        }
        use std::os::unix::fs::MetadataExt;
        let actual = parent.metadata()?;
        let expected = visible.metadata()?;
        if actual.dev() != expected.dev() || actual.ino() != expected.ino() {
            bail!("包文件父目录在操作期间发生变化");
        }
        Ok((parent, name))
    }

    #[cfg(not(unix))]
    fn resolved(&self, path: &Path) -> Result<std::path::PathBuf> {
        let mut result = self.path.clone();
        for part in path.components() {
            if !matches!(part, Component::Normal(_)) {
                bail!("包文件路径必须是安全相对路径");
            }
            result.push(part.as_os_str());
            if fs::symlink_metadata(&result).is_ok_and(|m| m.file_type().is_symlink()) {
                bail!("拒绝跟随包文件链接");
            }
        }
        Ok(result)
    }

    pub(crate) fn read(&self, path: &Path) -> Result<fs::File> {
        #[cfg(unix)]
        let file: fs::File = {
            let (parent, name) = self.parent(path, false)?;
            rustix::fs::openat(
                &parent,
                name.as_str(),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::empty(),
            )?
            .into()
        };
        #[cfg(not(unix))]
        let file = fs::File::open(self.resolved(path)?)?;
        if !file.metadata()?.is_file() {
            bail!("包节点不是普通文件");
        }
        Ok(file)
    }

    #[cfg(unix)]
    pub(crate) fn mkdir_exclusive(&self, path: &Path) -> Result<Self> {
        let (parent, name) = self.parent(path, false)?;
        rustix::fs::mkdirat(&parent, name.as_str(), Mode::from_raw_mode(0o700))?;
        self.child(path)
    }

    pub(crate) fn mkdir(&self, path: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            let (parent, name) = self.parent(path, true)?;
            match rustix::fs::mkdirat(&parent, name.as_str(), Mode::from_raw_mode(0o755)) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(error) => return Err(error.into()),
            }
            rustix::fs::openat(
                &parent,
                name.as_str(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(self.resolved(path)?)?;
        Ok(())
    }

    pub(crate) fn create_file(&self, path: &Path, mode: u32) -> Result<fs::File> {
        #[cfg(unix)]
        {
            let (parent, name) = self.parent(path, true)?;
            let file: fs::File = rustix::fs::openat(
                &parent,
                name.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )?
            .into();
            Self::file_mode(&file, u32::from(mode))?;
            Ok(file)
        }
        #[cfg(not(unix))]
        {
            let p = self.resolved(path)?;
            fs::create_dir_all(p.parent().context("包文件缺少父目录")?)?;
            let file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(p)?;
            Self::file_mode(&file, u32::from(mode))?;
            Ok(file)
        }
    }

    fn file_mode(file: &fs::File, mode: u32) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(mode))?;
        }
        #[cfg(not(unix))]
        {
            let mut p = file.metadata()?.permissions();
            p.set_readonly(mode != 0);
            file.set_permissions(p)?;
        }
        Ok(())
    }

    pub(crate) fn chmod_directory(&self, path: &Path, mode: u32) -> Result<()> {
        #[cfg(unix)]
        {
            let (parent, name) = self.parent(path, false)?;
            let file: fs::File = rustix::fs::openat(
                &parent,
                name.as_str(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?
            .into();
            Self::file_mode(&file, u32::from(mode))?;
        }
        #[cfg(not(unix))]
        {
            let p = self.resolved(path)?;
            let mut m = fs::metadata(&p)?.permissions();
            m.set_readonly(mode != 0);
            fs::set_permissions(p, m)?;
        }
        Ok(())
    }

    pub(crate) fn remove(&self, path: &Path, node: &Node) -> Result<()> {
        #[cfg(unix)]
        {
            let (parent, name) = self.parent(path, false)?;
            let flags = if matches!(node, Node::Directory { .. }) {
                AtFlags::REMOVEDIR
            } else {
                AtFlags::empty()
            };
            rustix::fs::unlinkat(&parent, name.as_str(), flags)?;
        }
        #[cfg(not(unix))]
        {
            let p = self.path.join(path);
            if matches!(
                node,
                Node::Directory { .. }
                    | Node::Symlink {
                        directory: true,
                        ..
                    }
            ) {
                fs::remove_dir(p)?;
            } else {
                fs::remove_file(p)?;
            }
        }
        Ok(())
    }

    pub(crate) fn symlink(&self, path: &Path, target: &str, directory: bool) -> Result<()> {
        #[cfg(unix)]
        {
            let _ = directory;
            let (parent, name) = self.parent(path, true)?;
            rustix::fs::symlinkat(target, &parent, name.as_str())?;
        }
        #[cfg(windows)]
        if directory {
            std::os::windows::fs::symlink_dir(target, self.resolved(path)?)?;
        } else {
            std::os::windows::fs::symlink_file(target, self.resolved(path)?)?;
        }
        Ok(())
    }

    pub(crate) fn write(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        #[cfg(unix)]
        {
            let (parent, name) = self.parent(path, true)?;
            let mode = match rustix::fs::statat(&parent, name.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
                Ok(m) if FileType::from_raw_mode(m.st_mode) == FileType::RegularFile => {
                    m.st_mode & 0o7777
                }
                Ok(_) => bail!("拒绝覆盖非实体包文件"),
                Err(rustix::io::Errno::NOENT) => 0o644,
                Err(e) => return Err(e.into()),
            };
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let nonce = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let temp = format!(
                ".kitter-write-{}-{nonce}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos()
            );
            let mut file: fs::File = rustix::fs::openat(
                &parent,
                temp.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )?
            .into();
            let result = (|| -> Result<()> {
                file.write_all(bytes)?;
                Self::file_mode(&file, u32::from(mode))?;
                file.sync_all()?;
                rustix::fs::renameat(&parent, temp.as_str(), &parent, name.as_str())?;
                Ok(())
            })();
            if result.is_err() {
                let _ = rustix::fs::unlinkat(&parent, temp.as_str(), AtFlags::empty());
            }
            result
        }
        #[cfg(not(unix))]
        {
            fs::write(self.resolved(path)?, bytes)?;
            Ok(())
        }
    }

    #[cfg(unix)]
    pub(crate) fn snapshot(&self) -> Result<Manifest> {
        self.snapshot_impl(false)
    }

    #[cfg(unix)]
    pub(crate) fn installation_snapshot(&self) -> Result<Manifest> {
        self.snapshot_impl(true)
    }

    #[cfg(not(unix))]
    pub(crate) fn installation_snapshot(&self) -> Result<Manifest> {
        bail!("此平台尚未支持 Proma 受管写入")
    }

    #[cfg(unix)]
    fn snapshot_impl(&self, include_references: bool) -> Result<Manifest> {
        fn scan(dir: &fs::File, prefix: &Path, out: &mut Manifest, include_references: bool) -> Result<()> {
            let mut entries = rustix::fs::Dir::read_from(dir)?;
            while let Some(entry) = entries.read() {
                let entry = entry?;
                let name = entry.file_name();
                let label = name.to_str().context("包路径不是 UTF-8")?;
                if matches!(label, "." | "..") {
                    continue;
                }
                let protected = if include_references {
                    label.eq_ignore_ascii_case("local-data") || label.eq_ignore_ascii_case(".git")
                } else { matches!(label, "local-data" | ".git") };
                if protected && !include_references { continue; }
                let path = prefix.join(label);
                let key = path.to_str().context("包路径不是 UTF-8")?.to_owned();
                let stat = rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)?;
                let node = match FileType::from_raw_mode(stat.st_mode) {
                    FileType::Directory => {
                        let child: fs::File = rustix::fs::openat(
                            dir,
                            name,
                            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                            Mode::empty(),
                        )?
                        .into();
                        use std::os::unix::fs::PermissionsExt;
                        let mode = child.metadata()?.permissions().mode() & 0o7777;
                        if !protected { scan(&child, &path, out, include_references)?; }
                        Node::Directory { mode }
                    }
                    FileType::RegularFile => {
                        let mut file: fs::File = rustix::fs::openat(
                            dir,
                            name,
                            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                            Mode::empty(),
                        )?
                        .into();
                        if !file.metadata()?.is_file() {
                            bail!("包文件类型发生变化");
                        }
                        use sha2::{Digest, Sha256};
                        use std::os::unix::fs::PermissionsExt;
                        let mode = file.metadata()?.permissions().mode() & 0o7777;
                        let mut hash = Sha256::new();
                        let mut buffer = [0u8; 65536];
                        while !protected {
                            let count = file.read(&mut buffer)?;
                            if count == 0 {
                                break;
                            }
                            hash.update(&buffer[..count]);
                        }
                        Node::File {
                            sha256: format!("{:x}", hash.finalize()),
                            mode,
                        }
                    }
                    FileType::Symlink => Node::Symlink {
                        target: rustix::fs::readlinkat(dir, name, Vec::new())?
                            .to_str()
                            .context("链接目标不是 UTF-8")?
                            .to_owned(),
                        directory: false,
                    },
                    _ => bail!("无法安全更新特殊文件：{key}"),
                };
                out.insert(key, node);
            }
            Ok(())
        }
        let mut result = Manifest::new();
        scan(&self.file, Path::new(""), &mut result, include_references)?;
        Ok(result)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn exclusive_rename_preserves_existing_files_directories_and_links() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let from = temp.path().join("from");
        let to = temp.path().join("to");
        fs::write(&from, "original").unwrap();
        fs::write(&to, "concurrent").unwrap();
        assert!(rename_exclusive(&from, &to).is_err());
        assert_eq!(fs::read_to_string(&from).unwrap(), "original");
        assert_eq!(fs::read_to_string(&to).unwrap(), "concurrent");
        fs::remove_file(&to).unwrap();
        fs::create_dir(&to).unwrap();
        assert!(rename_exclusive(&from, &to).is_err());
        fs::remove_dir(&to).unwrap();
        let link = temp.path().join("link");
        symlink("missing-private-target", &link).unwrap();
        rename_exclusive(&link, &to).unwrap();
        assert_eq!(fs::read_link(&to).unwrap(), Path::new("missing-private-target"));
    }
    #[test]
    fn parent_link_swap_cannot_read_or_modify_the_external_target() {
        use std::os::unix::fs::symlink;
        for write in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let package = temp.path().join("package");
            let external = temp.path().join("external");
            fs::create_dir_all(package.join("refs")).unwrap();
            fs::create_dir(&external).unwrap();
            fs::write(package.join("refs/note"), "owned data").unwrap();
            fs::write(external.join("note"), "external data").unwrap();
            let directory = Directory::open(&package).unwrap();
            let p = package.clone();
            let e = external.clone();
            AFTER_PARENT_OPEN.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    fs::rename(p.join("refs"), p.join("refs-original")).unwrap();
                    symlink(e, p.join("refs")).unwrap();
                }))
            });
            if write {
                assert!(
                    directory
                        .write(Path::new("refs/note"), b"new content")
                        .is_err()
                );
            } else {
                assert!(directory.read(Path::new("refs/note")).is_err());
            }
            assert_eq!(
                fs::read_to_string(external.join("note")).unwrap(),
                "external data"
            );
            assert_eq!(
                fs::read_to_string(package.join("refs-original/note")).unwrap(),
                "owned data"
            );
        }
    }

    #[test]
    fn copying_retains_raw_links_directory_modes_and_excludes_private_nodes() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let dest = temp.path().join("dest");
        fs::create_dir_all(source.join("refs")).unwrap();
        fs::write(source.join("refs/item"), "fixed asset").unwrap();
        fs::set_permissions(source.join("refs"), fs::Permissions::from_mode(0o555)).unwrap();
        symlink("local-data/missing", source.join("alias")).unwrap();
        fs::create_dir(source.join("local-data")).unwrap();
        fs::write(source.join("local-data/private"), "private fixture").unwrap();
        copy_tree(&source, &dest).unwrap();
        assert_eq!(
            fs::read_link(dest.join("alias")).unwrap(),
            Path::new("local-data/missing")
        );
        assert_eq!(
            fs::metadata(dest.join("refs"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o555
        );
        assert!(!dest.join("local-data").exists());
    }
}

pub(crate) fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    let input = Directory::open(source)?;
    #[cfg(unix)]
    let manifest = input.snapshot()?;
    #[cfg(not(unix))]
    let manifest = crate::update_protection::snapshot(source)?;
    fs::create_dir_all(destination)?;
    let output = Directory::open(destination)?;
    for (path, node) in &manifest {
        let p = Path::new(path);
        match node {
            Node::Directory { .. } => output.mkdir(p)?,
            Node::File { mode, .. } => {
                let mut source = input.read(p)?;
                let mut dest = output.create_file(p, *mode)?;
                std::io::copy(&mut source, &mut dest)?;
            }
            Node::Symlink { target, directory } => output.symlink(p, target, *directory)?,
        }
    }
    for (path, node) in manifest.iter().rev() {
        if let Node::Directory { mode } = node {
            output.chmod_directory(Path::new(path), *mode)?;
        }
    }
    if crate::update_protection::snapshot(destination)? != manifest {
        bail!("来源复制期间文件发生变化，未替换技能");
    }
    Ok(())
}
