//! Descriptor-bound access metadata for native configuration transactions.
//! ACL bytes come only from the OS, remain in memory, and are never formatted.
use anyhow::{Result, anyhow, ensure};
use std::{fmt, fs};
#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd};

const MAX_ACL: usize = 64 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Access {
    uid: u32,
    gid: u32,
    mode: u32,
    acl: Option<Vec<u8>>,
}
impl fmt::Debug for Access {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeConfigAccess").field("uid", &self.uid).field("gid", &self.gid)
            .field("mode", &self.mode).field("acl_bytes", &self.acl.as_ref().map(Vec::len)).finish()
    }
}

impl Access {
    #[cfg(unix)]
    pub(super) fn read(file: &fs::File) -> Result<Self> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = file.metadata()?;
        Ok(Self { uid: meta.uid(), gid: meta.gid(), mode: meta.permissions().mode() & 0o7777,
            acl: platform::read(file.as_raw_fd())? })
    }
    pub(super) fn mode(&self) -> u32 { self.mode }
    pub(super) fn has_acl(&self) -> bool { self.acl.is_some() }
    #[cfg(not(unix))]
    pub(super) fn read(_: &fs::File) -> Result<Self> { Err(anyhow!("此平台尚未核验原生配置 ACL")) }


    #[cfg(not(unix))]
    pub(super) fn apply(&self, _: &fs::File) -> Result<()> { Err(anyhow!("此平台尚未核验文件 ACL 写入")) }

    /// Restore the original owner/group, mode and ACL, then verify the result.
    #[cfg(unix)]
    pub(super) fn apply(&self, file: &fs::File) -> Result<()> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let current = file.metadata()?;
        if current.uid() != self.uid || current.gid() != self.gid {
            ensure!(unsafe { libc::fchown(file.as_raw_fd(), self.uid, self.gid) } == 0,
                "无法保留原生配置所有者与组，未发布");
        }
        file.set_permissions(fs::Permissions::from_mode(self.mode))?;
        platform::write(file.as_raw_fd(), self.acl.as_deref())?;
        ensure!(&Self::read(file)? == self, "无法精确保留原生配置访问权限，未发布");
        Ok(())
    }
}

/// Must precede writing any configuration body, including a retained candidate.
#[cfg(unix)]
pub(super) fn private_directory(directory: &impl AsFd) -> Result<()> {
    let fd = directory.as_fd().as_raw_fd();
    platform::private_directory(fd)?;
    ensure!(unsafe { libc::fchmod(fd, 0o700) } == 0, "无法建立私有原生配置恢复目录");
    ensure!(platform::read(fd)?.is_none(), "原生配置恢复目录仍有扩展访问权限，未写入正文");
    let stat = rustix::fs::fstat(directory)?;
    ensure!(stat.st_mode & 0o7777 == 0o700 && stat.st_uid == unsafe { libc::geteuid() },
        "原生配置恢复目录访问权限无效，未写入正文");
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn private_directory(_: &crate::package_fs::Directory) -> Result<()> {
    Err(anyhow!("此平台尚未核验私有恢复目录 ACL"))
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::ffi::{c_int, c_void};
    // SDK sys/acl.h: extended ACL=0x100. These libc ABI declarations are not
    // included by the Rust libc crate. No pointer escapes its owning wrapper.
    unsafe extern "C" {
        fn acl_get_fd_np(fd: c_int, kind: c_int) -> *mut c_void;
        fn acl_set_fd_np(fd: c_int, acl: *mut c_void, kind: c_int) -> c_int;
        fn acl_init(count: c_int) -> *mut c_void;
        fn acl_size(acl: *mut c_void) -> isize;
        fn acl_copy_ext(out: *mut c_void, acl: *mut c_void, size: isize) -> isize;
        fn acl_copy_int(bytes: *const c_void) -> *mut c_void;
        fn acl_free(acl: *mut c_void) -> c_int;
    }
    const EXTENDED: c_int = 0x100;
    struct Acl(*mut c_void);
    impl Drop for Acl { fn drop(&mut self) { unsafe { acl_free(self.0); } } }

    pub(super) fn read(fd: c_int) -> Result<Option<Vec<u8>>> {
        let raw = unsafe { acl_get_fd_np(fd, EXTENDED) };
        if raw.is_null() {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) { return Ok(None); }
            return Err(anyhow!("无法读取原生配置 ACL，未执行覆盖"));
        }
        let acl = Acl(raw);
        let size = unsafe { acl_size(acl.0) };
        ensure!(size > 0 && size as usize <= MAX_ACL, "原生配置 ACL 超过读取上限");
        let mut bytes = vec![0u8; size as usize];
        let copied = unsafe { acl_copy_ext(bytes.as_mut_ptr().cast(), acl.0, size) };
        ensure!(copied == size, "无法取得完整原生配置 ACL，未执行覆盖");
        Ok(Some(bytes))
    }
    pub(super) fn write(fd: c_int, bytes: Option<&[u8]>) -> Result<()> {
        let raw = unsafe { match bytes {
            Some(bytes) => acl_copy_int(bytes.as_ptr().cast()),
            None => acl_init(0),
        } };
        ensure!(!raw.is_null(), "无法准备原生配置 ACL，未发布");
        let acl = Acl(raw);
        ensure!(unsafe { acl_set_fd_np(fd, acl.0, EXTENDED) } == 0,
            "无法保留原生配置 ACL，未发布");
        Ok(())
    }
    pub(super) fn private_directory(fd: c_int) -> Result<()> { write(fd, None) }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::ffi::{CStr, c_int};
    const ACCESS: &CStr = c"system.posix_acl_access";
    const DEFAULT: &CStr = c"system.posix_acl_default";
    fn get(fd: c_int, name: &CStr) -> Result<Option<Vec<u8>>> {
        let length = unsafe { libc::fgetxattr(fd, name.as_ptr(), std::ptr::null_mut(), 0) };
        if length < 0 {
            if matches!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ENODATA | libc::ENOTSUP)) { return Ok(None); }
            return Err(anyhow!("无法读取原生配置 ACL，未执行覆盖"));
        }
        ensure!(length as usize <= MAX_ACL, "原生配置 ACL 超过读取上限");
        let mut bytes = vec![0; length as usize];
        let actual = unsafe { libc::fgetxattr(fd, name.as_ptr(), bytes.as_mut_ptr().cast(), bytes.len()) };
        ensure!(actual == length, "原生配置 ACL 读取时变化，未执行覆盖");
        Ok(Some(bytes))
    }
    fn remove(fd: c_int, name: &CStr) -> Result<()> {
        let rc = unsafe { libc::fremovexattr(fd, name.as_ptr()) };
        ensure!(rc == 0 || matches!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ENODATA | libc::ENOTSUP)),
            "无法移除非预期继承 ACL，未写入正文");
        Ok(())
    }
    pub(super) fn read(fd: c_int) -> Result<Option<Vec<u8>>> { get(fd, ACCESS) }
    pub(super) fn write(fd: c_int, bytes: Option<&[u8]>) -> Result<()> {
        if let Some(bytes) = bytes {
            ensure!(unsafe { libc::fsetxattr(fd, ACCESS.as_ptr(), bytes.as_ptr().cast(), bytes.len(), 0) } == 0,
                "无法保留原生配置 ACL，未发布");
        } else { remove(fd, ACCESS)?; }
        Ok(())
    }
    pub(super) fn private_directory(fd: c_int) -> Result<()> {
        remove(fd, DEFAULT)?; remove(fd, ACCESS)?;
        ensure!(get(fd, DEFAULT)?.is_none(), "恢复目录仍有继承 ACL，未写入正文");
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    use super::*;
    pub(super) fn read(_: i32) -> Result<Option<Vec<u8>>> { Err(anyhow!("此平台尚未核验原生配置 ACL")) }
    pub(super) fn write(_: i32, _: Option<&[u8]>) -> Result<()> { Err(anyhow!("此平台尚未核验原生配置 ACL")) }
    pub(super) fn private_directory(_: i32) -> Result<()> { Err(anyhow!("此平台尚未核验私有配置目录 ACL")) }
}
