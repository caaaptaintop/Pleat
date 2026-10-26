//! SQLite metadata reader with write-denying exclusive and live-WAL paths.
//!
//! The exclusive path needs a native RW handle for POSIX F_WRLCK; the live
//! path opens only O_RDONLY files. SQLite always sees READ_ONLY. Every mutation method is denied, including file controls.
//! Native Unix locking is selected explicitly: no dot-file/proxy lock fallback.
use std::{
    ffi::{c_char, c_int, c_void},
    path::Path,
    ptr,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, config::DbConfig, ffi};

const NAME: &str = "ai-cube-index-readonly-v1";
const LIVE_NAME: &str = "ai-cube-index-live-readonly-v1";
const BUSY_TIMEOUT: Duration = Duration::from_millis(200);
// Conservative metadata-reader limits, not limits on the source database's
// total size. Repeated/short/failed native read requests also spend the budget.
const MAX_WAL_BYTES: i64 = 32 * 1024 * 1024;
const MAX_FILE_READ_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SOURCE_READ_BYTES: u64 = 96 * 1024 * 1024;
const MAX_VALUE_BYTES: c_int = 1024 * 1024;
const MAX_SQL_BYTES: c_int = 64 * 1024;
const INIT_PROGRESS_INTERVAL: c_int = 1000;
const MAX_INIT_PROGRESS_CALLS: u32 = 200;
static REGISTERED: OnceLock<c_int> = OnceLock::new();

/// Opens one read transaction, acquiring the required locks before returning.
/// Errors must be sanitized by the caller; never fall back to the default VFS.
pub(super) fn open_readonly(path: &Path) -> rusqlite::Result<Connection> {
    // Resolve directory aliases (e.g. macOS /var -> /private/var) without
    // following a source-file symlink. The live VFS pins and rechecks this
    // canonical directory, then uses openat with O_NOFOLLOW for every file.
    let reject = || rusqlite::Error::SqliteFailure(ffi::Error::new(ffi::SQLITE_CANTOPEN), None);
    if !std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file()) { return Err(reject()); }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let normalized = parent.canonicalize().map_err(|_| reject())?.join(path.file_name().ok_or_else(reject)?);
    let path = normalized.as_path();
    let exclusive = open_mode(path, false);
    // Contention or an unavailable RW lock handle may use the existing-SHM
    // live reader. Missing/invalid SHM still fails; there is no ordinary or
    // immutable VFS fallback, and corrupt/schema/budget errors are final.
    #[cfg(all(target_pointer_width = "64", any(target_os = "macos", target_os = "linux")))]
    if matches!(exclusive.as_ref().err().and_then(|e| e.sqlite_error_code()),
        Some(ffi::ErrorCode::DatabaseBusy | ffi::ErrorCode::CannotOpen))
    {
        return open_mode(path, true);
    }
    exclusive
}

fn open_mode(path: &Path, live: bool) -> rusqlite::Result<Connection> {
    let rc = *REGISTERED.get_or_init(|| unsafe { register() });
    if rc != ffi::SQLITE_OK {
        return Err(rusqlite::Error::SqliteFailure(ffi::Error::new(rc), None));
    }
    let connection = Connection::open_with_flags_and_vfs(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        if live { LIVE_NAME } else { NAME },
    )?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
    // These are connection-local and do not access schema or source data.
    // SQLite 3.50 has no SQLITE_LIMIT_SCHEMA; the temporary progress budget
    // also bounds initialization of a schema containing many small objects.
    let mut progress_left = MAX_INIT_PROGRESS_CALLS;
    unsafe {
        let db = connection.handle();
        ffi::sqlite3_limit(db, ffi::SQLITE_LIMIT_LENGTH, MAX_VALUE_BYTES);
        ffi::sqlite3_limit(db, ffi::SQLITE_LIMIT_SQL_LENGTH, MAX_SQL_BYTES);
        ffi::sqlite3_progress_handler(
            db,
            INIT_PROGRESS_INTERVAL,
            Some(init_progress),
            (&mut progress_left as *mut u32).cast(),
        );
    }
    // Must precede any schema/WAL access. The SELECT acquires the lock now;
    // the transaction holds it through the caller's schema/path projections.
    let initialized = connection.execute_batch(if live {
        "BEGIN DEFERRED; SELECT 1 FROM sqlite_schema LIMIT 1;"
    } else {
        "PRAGMA locking_mode=EXCLUSIVE; BEGIN DEFERRED; SELECT 1 FROM sqlite_schema LIMIT 1;"
    });
    // Clear the callback before its stack context expires, including on error.
    unsafe {
        ffi::sqlite3_progress_handler(connection.handle(), 0, None, ptr::null_mut());
    }
    initialized?;
    Ok(connection)
}

#[repr(C)]
struct File {
    base: ffi::sqlite3_file,
    inner: *mut ffi::sqlite3_file,
    read_left: u64,
    total_left: *const AtomicU64,
    kind: c_int,
}

unsafe extern "C" fn init_progress(context: *mut c_void) -> c_int {
    // SQLite invokes this synchronously. Context remains live until the
    // callback is cleared, and the callback never calls back into SQLite.
    unsafe {
        let left = &mut *context.cast::<u32>();
        *left = left.saturating_sub(1);
        c_int::from(*left == 0)
    }
}

unsafe fn wrap_file(inner: *mut ffi::sqlite3_file, parent: *mut File, kind: c_int) -> File {
    unsafe {
        let total_left = if parent.is_null() {
            Arc::into_raw(Arc::new(AtomicU64::new(MAX_SOURCE_READ_BYTES)))
        } else {
            let shared = (*parent).total_left;
            Arc::increment_strong_count(shared);
            shared
        };
        File {
            base: ffi::sqlite3_file { pMethods: &METHODS },
            inner,
            read_left: MAX_FILE_READ_BYTES,
            total_left,
            kind,
        }
    }
}

// SAFETY for the callbacks below: SQLite provides initialized VFS arguments,
// valid buffers and a File allocation of szOsFile bytes. The native object is
// separately allocated by sqlite3_malloc64 (native alignment), zeroed, and
// remains owned by File until xClose. Each published File owns exactly one
// Arc strong count for its source read budget, including virtual/aux files.
// Only successful xOpen publishes METHODS.
// The native VFS and our registration live for the process lifetime. Native
// required v1 methods are present; no callback unwinds across the C boundary.
unsafe fn native(vfs: *mut ffi::sqlite3_vfs) -> *mut ffi::sqlite3_vfs {
    unsafe { (*vfs).pAppData.cast() }
}

unsafe fn register() -> c_int {
    unsafe {
        let rc = ffi::sqlite3_initialize();
        if rc != ffi::SQLITE_OK {
            return rc;
        }
        // unix-excl always uses POSIX locks, including when the filesystem
        // would make the default Unix VFS choose a mutating dot/proxy lock.
        let name = if cfg!(unix) { c"unix-excl" } else { c"win32" };
        let base = ffi::sqlite3_vfs_find(name.as_ptr());
        if base.is_null() {
            return ffi::SQLITE_CANTOPEN;
        }
        let mut vfs: ffi::sqlite3_vfs = std::mem::zeroed();
        vfs.iVersion = 1;
        vfs.szOsFile = std::mem::size_of::<File>() as c_int;
        vfs.mxPathname = (*base).mxPathname;
        vfs.zName = c"ai-cube-index-readonly-v1".as_ptr();
        vfs.pAppData = base.cast();
        vfs.xOpen = Some(open);
        vfs.xDelete = Some(delete);
        vfs.xAccess = Some(access);
        vfs.xFullPathname = Some(full_pathname);
        // Dynamic extensions and system-call overrides are not exposed.
        vfs.xRandomness = Some(randomness);
        vfs.xSleep = Some(sleep);
        vfs.xCurrentTime = Some(current_time);
        let vfs = Box::into_raw(Box::new(vfs));
        let rc = ffi::sqlite3_vfs_register(vfs, 0);
        if rc != ffi::SQLITE_OK {
            drop(Box::from_raw(vfs));
        }
        if rc != ffi::SQLITE_OK { return rc; }
        #[cfg(all(target_pointer_width = "64", any(target_os = "macos", target_os = "linux")))]
        { live_wal::register_live(base) }
        #[cfg(not(all(target_pointer_width = "64", any(target_os = "macos", target_os = "linux"))))]
        { rc }
    }
}

unsafe extern "C" fn open(
    vfs: *mut ffi::sqlite3_vfs,
    name: ffi::sqlite3_filename,
    file: *mut ffi::sqlite3_file,
    flags: c_int,
    out: *mut c_int,
) -> c_int {
    unsafe {
        (*file).pMethods = ptr::null();
        let kind = flags & 0x0fff00; // SQLite's documented xOpen file-type bits.
        if name.is_null() || flags & ffi::SQLITE_OPEN_DELETEONCLOSE != 0 {
            return ffi::SQLITE_CANTOPEN;
        }
        let mode = match kind {
            ffi::SQLITE_OPEN_MAIN_DB => {
                if flags
                    & (ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE | ffi::SQLITE_OPEN_URI)
                    != 0
                    || flags & ffi::SQLITE_OPEN_READONLY == 0
                {
                    return ffi::SQLITE_CANTOPEN;
                }
                ffi::SQLITE_OPEN_READWRITE
            }
            ffi::SQLITE_OPEN_WAL | ffi::SQLITE_OPEN_MAIN_JOURNAL => ffi::SQLITE_OPEN_READONLY,
            _ => return ffi::SQLITE_CANTOPEN, // No temporary or attached write DBs.
        };
        // Auxiliary filenames are supplied by SQLite's pager, so its
        // database_file_object API identifies the owning main-file wrapper.
        let parent = if kind == ffi::SQLITE_OPEN_MAIN_DB {
            ptr::null_mut()
        } else {
            let main = ffi::sqlite3_database_file_object(name);
            if main.is_null() || !ptr::eq((*main).pMethods, &METHODS) {
                return ffi::SQLITE_CANTOPEN;
            }
            main.cast::<File>()
        };
        // WAL reads (including an absent WAL) are safe only after the
        // native main-file lock has actually reached EXCLUSIVE.
        if kind == ffi::SQLITE_OPEN_WAL {
            let main = parent.cast::<ffi::sqlite3_file>();
            let mut state = ffi::SQLITE_LOCK_NONE;
            if main.is_null()
                || control(
                    main,
                    ffi::SQLITE_FCNTL_LOCKSTATE,
                    (&mut state as *mut c_int).cast(),
                ) != ffi::SQLITE_OK
                || state != ffi::SQLITE_LOCK_EXCLUSIVE
            {
                return ffi::SQLITE_BUSY;
            }
        }
        let base = native(vfs);
        let inner = ffi::sqlite3_malloc64((*base).szOsFile as u64).cast::<ffi::sqlite3_file>();
        if inner.is_null() {
            return ffi::SQLITE_NOMEM;
        }
        ptr::write_bytes(inner.cast::<u8>(), 0, (*base).szOsFile as usize);
        let mut actual = 0;
        // In particular, never pass CREATE, DELETEONCLOSE, URI or AUTOPROXY.
        // Classify auxiliary handles as native MAIN_DB too. unixOpen's
        // WAL/JOURNAL branch can fchown even a read-only existing file;
        // MAIN_DB avoids that branch. The wrapper still reports the real
        // kind to SQLite, and auxiliary descriptors remain strictly RO.
        let mut rc = ((*base).xOpen.unwrap())(
            base,
            name,
            inner,
            ffi::SQLITE_OPEN_MAIN_DB | mode | ffi::SQLITE_OPEN_NOFOLLOW,
            &mut actual,
        );
        if rc == ffi::SQLITE_OK && kind == ffi::SQLITE_OPEN_WAL {
            // Reject before any WAL frame is scanned or its index allocated.
            let mut bytes = 0;
            rc = ((*(*inner).pMethods).xFileSize.unwrap())(inner, &mut bytes);
            if rc == ffi::SQLITE_OK && (bytes < 0 || bytes > MAX_WAL_BYTES) {
                rc = ffi::SQLITE_TOOBIG;
            }
        }
        if rc != ffi::SQLITE_OK
            || (kind == ffi::SQLITE_OPEN_MAIN_DB && actual & ffi::SQLITE_OPEN_READWRITE == 0)
        {
            if !(*inner).pMethods.is_null() {
                ((*(*inner).pMethods).xClose.unwrap())(inner);
            }
            ffi::sqlite3_free(inner.cast());
            // A cleanly closed WAL database keeps a WAL-mode header but
            // has no log. Under the exclusive main lock, a proven-absent
            // WAL can be represented as an empty read-only virtual file.
            // An unreadable/existing log must fail, never become empty.
            if kind == ffi::SQLITE_OPEN_WAL && wal_is_absent(name) {
                ptr::write(
                    file.cast::<File>(),
                    wrap_file(ptr::null_mut(), parent, kind),
                );
                if !out.is_null() {
                    *out = kind | ffi::SQLITE_OPEN_READONLY;
                }
                return ffi::SQLITE_OK;
            }
            return if rc == ffi::SQLITE_OK {
                ffi::SQLITE_CANTOPEN
            } else {
                rc
            };
        }
        ptr::write(file.cast::<File>(), wrap_file(inner, parent, kind));
        if !out.is_null() {
            *out = kind | ffi::SQLITE_OPEN_READONLY;
        }
        ffi::SQLITE_OK
    }
}

unsafe fn wal_is_absent(name: ffi::sqlite3_filename) -> bool {
    unsafe {
        let name = std::ffi::CStr::from_ptr(name);
        #[cfg(unix)]
        let path = {
            use std::os::unix::ffi::OsStrExt;
            Path::new(std::ffi::OsStr::from_bytes(name.to_bytes()))
        };
        #[cfg(not(unix))]
        let path = match name.to_str() {
            Ok(name) => Path::new(name),
            Err(_) => return false,
        };
        matches!(std::fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    }
}

unsafe extern "C" fn close(file: *mut ffi::sqlite3_file) -> c_int {
    unsafe {
        let inner = (*file.cast::<File>()).inner;
        let rc = if inner.is_null() {
            ffi::SQLITE_OK
        } else {
            ((*(*inner).pMethods).xClose.unwrap())(inner)
        };
        ffi::sqlite3_free(inner.cast());
        drop(Arc::from_raw((*file.cast::<File>()).total_left));
        (*file).pMethods = ptr::null();
        rc
    }
}

unsafe extern "C" fn read(
    file: *mut ffi::sqlite3_file,
    buf: *mut c_void,
    n: c_int,
    offset: i64,
) -> c_int {
    unsafe {
        let file = &mut *file.cast::<File>();
        if n < 0 || offset < 0 || offset.checked_add(i64::from(n)).is_none() {
            return ffi::SQLITE_IOERR_READ;
        }
        if file.kind == ffi::SQLITE_OPEN_WAL && offset + i64::from(n) > MAX_WAL_BYTES {
            return ffi::SQLITE_TOOBIG;
        }
        let bytes = n as u64;
        if bytes > file.read_left
            || (*file.total_left)
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                    left.checked_sub(bytes)
                })
                .is_err()
        {
            return ffi::SQLITE_IOERR_READ;
        }
        file.read_left -= bytes;
        let inner = file.inner;
        if inner.is_null() {
            ptr::write_bytes(buf.cast::<u8>(), 0, n as usize);
            return ffi::SQLITE_IOERR_SHORT_READ;
        }
        ((*(*inner).pMethods).xRead.unwrap())(inner, buf, n, offset)
    }
}
unsafe extern "C" fn size(file: *mut ffi::sqlite3_file, out: *mut i64) -> c_int {
    unsafe {
        let inner = (*file.cast::<File>()).inner;
        if inner.is_null() {
            *out = 0;
            return ffi::SQLITE_OK;
        }
        let rc = ((*(*inner).pMethods).xFileSize.unwrap())(inner, out);
        if rc == ffi::SQLITE_OK
            && (*file.cast::<File>()).kind == ffi::SQLITE_OPEN_WAL
            && (*out < 0 || *out > MAX_WAL_BYTES)
        {
            *out = 0;
            return ffi::SQLITE_TOOBIG;
        }
        rc
    }
}
unsafe extern "C" fn lock(file: *mut ffi::sqlite3_file, level: c_int) -> c_int {
    unsafe {
        let inner = (*file.cast::<File>()).inner;
        if inner.is_null() {
            return ffi::SQLITE_IOERR_LOCK;
        }
        ((*(*inner).pMethods).xLock.unwrap())(inner, level)
    }
}
unsafe extern "C" fn unlock(file: *mut ffi::sqlite3_file, level: c_int) -> c_int {
    unsafe {
        let inner = (*file.cast::<File>()).inner;
        if inner.is_null() {
            return ffi::SQLITE_IOERR_UNLOCK;
        }
        ((*(*inner).pMethods).xUnlock.unwrap())(inner, level)
    }
}
unsafe extern "C" fn reserved(file: *mut ffi::sqlite3_file, out: *mut c_int) -> c_int {
    unsafe {
        let inner = (*file.cast::<File>()).inner;
        if inner.is_null() {
            return ffi::SQLITE_IOERR_CHECKRESERVEDLOCK;
        }
        ((*(*inner).pMethods).xCheckReservedLock.unwrap())(inner, out)
    }
}
unsafe extern "C" fn control(file: *mut ffi::sqlite3_file, op: c_int, arg: *mut c_void) -> c_int {
    // Reject SIZE_HINT, CHUNK_SIZE, MMAP_SIZE, PRAGMA, handle/pointer escapes,
    // atomic-write controls and unknown opcodes. Only native metadata queries.
    if !matches!(
        op,
        ffi::SQLITE_FCNTL_LOCKSTATE | ffi::SQLITE_FCNTL_LAST_ERRNO | ffi::SQLITE_FCNTL_HAS_MOVED
    ) {
        return ffi::SQLITE_NOTFOUND;
    }
    unsafe {
        let inner = (*file.cast::<File>()).inner;
        if inner.is_null() {
            return ffi::SQLITE_NOTFOUND;
        }
        ((*(*inner).pMethods).xFileControl.unwrap())(inner, op, arg)
    }
}
unsafe extern "C" fn sector_size(file: *mut ffi::sqlite3_file) -> c_int {
    unsafe {
        let inner = (*file.cast::<File>()).inner;
        if inner.is_null() {
            return 512;
        }
        ((*(*inner).pMethods).xSectorSize.unwrap())(inner)
    }
}
unsafe extern "C" fn characteristics(_: *mut ffi::sqlite3_file) -> c_int {
    0
}
unsafe extern "C" fn write(_: *mut ffi::sqlite3_file, _: *const c_void, _: c_int, _: i64) -> c_int {
    ffi::SQLITE_READONLY
}
unsafe extern "C" fn truncate(_: *mut ffi::sqlite3_file, _: i64) -> c_int {
    ffi::SQLITE_READONLY
}
unsafe extern "C" fn sync(_: *mut ffi::sqlite3_file, _: c_int) -> c_int {
    ffi::SQLITE_READONLY
}
unsafe extern "C" fn delete(_: *mut ffi::sqlite3_vfs, _: *const c_char, _: c_int) -> c_int {
    ffi::SQLITE_READONLY
}

static METHODS: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
    iVersion: 1,
    xClose: Some(close),
    xRead: Some(read),
    xWrite: Some(write),
    xTruncate: Some(truncate),
    xSync: Some(sync),
    xFileSize: Some(size),
    xLock: Some(lock),
    xUnlock: Some(unlock),
    xCheckReservedLock: Some(reserved),
    xFileControl: Some(control),
    xSectorSize: Some(sector_size),
    xDeviceCharacteristics: Some(characteristics),
    xShmMap: None,
    xShmLock: None,
    xShmBarrier: None,
    xShmUnmap: None,
    xFetch: None,
    xUnfetch: None,
};

unsafe extern "C" fn access(
    vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    flags: c_int,
    out: *mut c_int,
) -> c_int {
    unsafe {
        let base = native(vfs);
        ((*base).xAccess.unwrap())(base, name, flags, out)
    }
}
unsafe extern "C" fn full_pathname(
    vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    n: c_int,
    out: *mut c_char,
) -> c_int {
    unsafe {
        let base = native(vfs);
        ((*base).xFullPathname.unwrap())(base, name, n, out)
    }
}
unsafe extern "C" fn randomness(vfs: *mut ffi::sqlite3_vfs, n: c_int, out: *mut c_char) -> c_int {
    unsafe {
        let base = native(vfs);
        ((*base).xRandomness.unwrap())(base, n, out)
    }
}
unsafe extern "C" fn sleep(vfs: *mut ffi::sqlite3_vfs, us: c_int) -> c_int {
    unsafe {
        let base = native(vfs);
        ((*base).xSleep.unwrap())(base, us)
    }
}
unsafe extern "C" fn current_time(vfs: *mut ffi::sqlite3_vfs, out: *mut f64) -> c_int {
    unsafe {
        let base = native(vfs);
        ((*base).xCurrentTime.unwrap())(base, out)
    }
}

// Live-WAL uses only existing, anchored, regular O_RDONLY files. The exclusive
// path above remains the first choice (including stale/absent SHM recovery).
#[cfg(all(target_pointer_width = "64", any(target_os = "macos", target_os = "linux")))]
mod live_wal {
    // SQLite owns szOsFile bytes aligned by sqlite3_malloc. xOpen publishes
    // IO only after ptr::write constructs LiveFile; xClose drops it once.
    // C buffers and arguments have the SQLite VFS callback lifetimes. SHM
    // pointers remain valid until xShmUnmap; only PROT_READ is exposed. Mutexes
    // serialize shared OFD descriptions, counts never underflow, and no source
    // fd is closed (including failed opens). No callback invokes SQLite again.
    use super::*;
    use std::{
        ffi::{CStr, CString, OsStr},
        fs,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{ffi::OsStrExt, fs::MetadataExt},
        },
        path::PathBuf,
        sync::Mutex,
    };

    // SQLite 3.50.2: wal.c WALINDEX_PGSZ=32768; os_unix.c
    // UNIX_SHM_BASE=120, UNIX_SHM_DMS=128. Main DB PENDING_BYTE is
    // 0x40000000; SHARED_FIRST=PENDING_BYTE+2, SHARED_SIZE=510.
    const REGION: usize = 32768;
    const MAX_SHM: usize = 1024 * 1024;
    const MAX_RETAINED_FDS: usize = 256;
    // POSIX close(2) releases *all process locks* on an inode, including locks
    // owned by a native SQLite connection. Never close a source fd, even on an
    // error path. This bounded cache retains descriptors, NOT locks or maps.
    // OFD locks conflict with native POSIX locks, also within this process;
    // unrelated closes cannot release our OFD locks. Unsupported FS => error.
    static NODES: Mutex<Vec<Arc<Node>>> = Mutex::new(Vec::new());
    struct Node {
        fd: c_int,
        id: (u64, u64),
        counts: Mutex<[u32; 9]>,
    }
    fn id(m: &fs::Metadata) -> (u64, u64) {
        (m.dev(), m.ino())
    }
    fn stat_at(dir: c_int, name: &CStr) -> Result<libc::stat, c_int> {
        let mut st = unsafe { std::mem::zeroed::<libc::stat>() };
        if unsafe { libc::fstatat(dir, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } != 0
            || st.st_mode & libc::S_IFMT != libc::S_IFREG
            || st.st_nlink != 1
        {
            return Err(ffi::SQLITE_CANTOPEN);
        }
        Ok(st)
    }
    fn stat_id(st: &libc::stat) -> (u64, u64) {
        (st.st_dev as u64, st.st_ino as u64)
    }
    fn node(dir: c_int, name: &CStr) -> Result<Arc<Node>, c_int> {
        let before = stat_at(dir, name)?;
        if unsafe { libc::faccessat(dir, name.as_ptr(), libc::R_OK, libc::AT_EACCESS) } != 0 {
            return Err(ffi::SQLITE_CANTOPEN);
        }
        let mut nodes = NODES.lock().map_err(|_| ffi::SQLITE_IOERR)?;
        if let Some(n) = nodes.iter().find(|n| n.id == stat_id(&before)) {
            return Ok(n.clone());
        }
        if nodes.len() >= MAX_RETAINED_FDS {
            return Err(ffi::SQLITE_CANTOPEN);
        }
        let fd = unsafe {
            libc::openat(
                dir,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(ffi::SQLITE_CANTOPEN);
        }
        // Publish retention before *any* subsequent fallible check.
        let mut st = unsafe { std::mem::zeroed::<libc::stat>() };
        let ok = unsafe { libc::fstat(fd, &mut st) } == 0;
        let n = Arc::new(Node {
            fd,
            id: stat_id(&st),
            counts: Mutex::new([0; 9]),
        });
        nodes.push(n.clone());
        let after = stat_at(dir, name)?;
        if !ok
            || st.st_mode & libc::S_IFMT != libc::S_IFREG
            || st.st_nlink != 1
            || n.id != stat_id(&before)
            || n.id != stat_id(&after)
        {
            return Err(ffi::SQLITE_CANTOPEN);
        }
        Ok(n)
    }
    fn os_lock(n: &Node, kind: c_int, start: i64, len: i64) -> c_int {
        let mut fl = unsafe { std::mem::zeroed::<libc::flock>() };
        fl.l_type = kind as _;
        fl.l_whence = libc::SEEK_SET as _;
        fl.l_start = start;
        fl.l_len = len;
        if unsafe { libc::fcntl(n.fd, libc::F_OFD_SETLK, &fl) } == 0 {
            ffi::SQLITE_OK
        } else {
            match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::EAGAIN) | Some(libc::EACCES) => ffi::SQLITE_BUSY,
                _ => ffi::SQLITE_IOERR_LOCK,
            }
        }
    }
    fn conflict(n: &Node, start: i64) -> Result<c_int, c_int> {
        let mut fl = unsafe { std::mem::zeroed::<libc::flock>() };
        fl.l_type = libc::F_WRLCK as _;
        fl.l_whence = libc::SEEK_SET as _;
        fl.l_start = start;
        fl.l_len = 1;
        if unsafe { libc::fcntl(n.fd, libc::F_OFD_GETLK, &mut fl) } != 0 {
            Err(ffi::SQLITE_IOERR_LOCK)
        } else {
            Ok(fl.l_type as c_int)
        }
    }
    fn shared(n: &Node, slot: usize, start: i64, len: i64, acquire: bool) -> c_int {
        let Ok(mut counts) = n.counts.lock() else {
            return ffi::SQLITE_IOERR_LOCK;
        };
        if acquire {
            if counts[slot] == u32::MAX {
                return ffi::SQLITE_IOERR_LOCK;
            }
            if counts[slot] == 0 {
                let rc = os_lock(n, libc::F_RDLCK as c_int, start, len);
                if rc != ffi::SQLITE_OK {
                    return rc;
                }
            }
            counts[slot] += 1;
        } else if counts[slot] > 0 {
            if counts[slot] == 1 {
                let rc = os_lock(n, libc::F_UNLCK as c_int, start, len);
                if rc != ffi::SQLITE_OK {
                    return rc;
                }
            }
            counts[slot] -= 1;
        }
        ffi::SQLITE_OK
    }
    struct Source {
        path: PathBuf,
        directory: fs::File,
        directory_id: (u64, u64),
        base: CString,
        main: Arc<Node>,
        // DB and WAL share this immutable binding, even after xShmUnmap.
        // SQLite can reuse a mapped index without calling xShmMap/xShmLock
        // again, so every actual data I/O must validate this lock domain.
        shm_identity: OnceLock<Arc<Node>>,
    }
    impl Source {
        fn new(name: &CStr) -> Result<Self, c_int> {
            let path = Path::new(OsStr::from_bytes(name.to_bytes()));
            let parent = path.parent().ok_or(ffi::SQLITE_CANTOPEN)?;
            let base = CString::new(path.file_name().ok_or(ffi::SQLITE_CANTOPEN)?.as_bytes())
                .map_err(|_| ffi::SQLITE_CANTOPEN)?;
            let p =
                CString::new(parent.as_os_str().as_bytes()).map_err(|_| ffi::SQLITE_CANTOPEN)?;
            let fd = unsafe {
                libc::open(
                    p.as_ptr(),
                    libc::O_RDONLY
                        | libc::O_DIRECTORY
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC
                        | libc::O_NONBLOCK,
                )
            };
            if fd < 0 {
                return Err(ffi::SQLITE_CANTOPEN);
            }
            let directory = unsafe { fs::File::from_raw_fd(fd) };
            let meta = directory.metadata().map_err(|_| ffi::SQLITE_CANTOPEN)?;
            let main = node(fd, &base)?;
            let source = Self {
                path: path.to_owned(),
                directory,
                directory_id: id(&meta),
                base,
                main,
                shm_identity: OnceLock::new(),
            };
            if !source.valid() {
                return Err(ffi::SQLITE_CANTOPEN);
            }
            Ok(source)
        }
        fn name(&self, suffix: &[u8]) -> CString {
            let mut bytes = self.base.as_bytes().to_vec();
            bytes.extend_from_slice(suffix);
            // Both inputs are NUL-free and SQLite's suffixes are fixed literals.
            unsafe { CString::from_vec_unchecked(bytes) }
        }
        fn valid(&self) -> bool {
            let Some(parent) = self.path.parent() else {
                return false;
            };
            fs::symlink_metadata(parent).is_ok_and(|m| m.is_dir() && id(&m) == self.directory_id)
                && self.valid_node(&self.main, &self.base)
                && self.shm_identity.get().is_none_or(|shm| {
                    self.valid_node(shm, &self.name(b"-shm"))
                })
        }
        fn valid_node(&self, n: &Node, name: &CStr) -> bool {
            stat_at(self.directory.as_raw_fd(), name).is_ok_and(|st| stat_id(&st) == n.id)
        }
    }
    struct Shm {
        node: Arc<Node>,
        maps: Vec<*mut c_void>,
        held: u16,
    }
    impl Shm {
        fn release(&mut self) -> c_int {
            let mut result = ffi::SQLITE_OK;
            for p in self.maps.drain(..) {
                if unsafe { libc::munmap(p, REGION) } != 0 {
                    result = ffi::SQLITE_IOERR_SHMMAP;
                }
            }
            for i in 0..9 {
                let bit = 1 << i;
                if self.held & bit != 0 {
                    let rc = shared(&self.node, i, 120 + i as i64, 1, false);
                    if rc == ffi::SQLITE_OK {
                        self.held &= !bit;
                    } else {
                        result = rc;
                    }
                }
            }
            result
        }
    }
    impl Drop for Shm {
        fn drop(&mut self) {
            self.release();
        }
    }
    #[repr(C)]
    struct LiveFile {
        base: ffi::sqlite3_file,
        source: Arc<Source>,
        data: Arc<Node>,
        name: CString,
        left: u64,
        total: Arc<AtomicU64>,
        main: bool,
        locked: bool,
        shm: Option<Shm>,
    }
    impl LiveFile {
        fn valid(&self) -> bool {
            self.source.valid() && self.source.valid_node(&self.data, &self.name)
        }
        fn spend(&mut self, n: u64) -> bool {
            if n > self.left
                || self
                    .total
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_sub(n))
                    .is_err()
            {
                return false;
            }
            self.left -= n;
            true
        }
    }
    unsafe extern "C" fn open_live(
        _: *mut ffi::sqlite3_vfs,
        name: ffi::sqlite3_filename,
        out: *mut ffi::sqlite3_file,
        flags: c_int,
        actual: *mut c_int,
    ) -> c_int {
        unsafe {
            (*out).pMethods = ptr::null();
            if name.is_null()
                || flags & (ffi::SQLITE_OPEN_DELETEONCLOSE | ffi::SQLITE_OPEN_URI) != 0
            {
                return ffi::SQLITE_CANTOPEN;
            }
            let kind = flags & 0x0fff00;
            let result = (|| -> Result<LiveFile, c_int> {
                let (source, data, local_name, total, main) = if kind == ffi::SQLITE_OPEN_MAIN_DB {
                    if flags & ffi::SQLITE_OPEN_READONLY == 0
                        || flags & (ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE) != 0
                    {
                        return Err(ffi::SQLITE_CANTOPEN);
                    }
                    let source = Arc::new(Source::new(CStr::from_ptr(name))?);
                    (
                        source.clone(),
                        source.main.clone(),
                        source.base.clone(),
                        Arc::new(AtomicU64::new(MAX_SOURCE_READ_BYTES)),
                        true,
                    )
                } else if kind == ffi::SQLITE_OPEN_WAL {
                    let owner = ffi::sqlite3_database_file_object(name);
                    if owner.is_null() || !ptr::eq((*owner).pMethods, &IO) {
                        return Err(ffi::SQLITE_CANTOPEN);
                    }
                    let parent = &*owner.cast::<LiveFile>();
                    if !parent.locked || !parent.valid() {
                        return Err(ffi::SQLITE_CANTOPEN);
                    }
                    let local_name = parent.source.name(b"-wal");
                    if Path::new(OsStr::from_bytes(CStr::from_ptr(name).to_bytes()))
                        != parent
                            .source
                            .path
                            .with_file_name(OsStr::from_bytes(local_name.as_bytes()))
                    {
                        return Err(ffi::SQLITE_CANTOPEN);
                    }
                    let data = node(parent.source.directory.as_raw_fd(), &local_name)?;
                    if data.id == parent.data.id {
                        return Err(ffi::SQLITE_CANTOPEN);
                    }
                    (
                        parent.source.clone(),
                        data,
                        local_name,
                        parent.total.clone(),
                        false,
                    )
                } else {
                    return Err(ffi::SQLITE_CANTOPEN);
                };
                let f = LiveFile {
                    base: ffi::sqlite3_file { pMethods: &IO },
                    source,
                    data,
                    name: local_name,
                    left: MAX_FILE_READ_BYTES,
                    total,
                    main,
                    locked: false,
                    shm: None,
                };
                if !f.valid() {
                    return Err(ffi::SQLITE_CANTOPEN);
                }
                if !main && file_size(&f)? > MAX_WAL_BYTES {
                    return Err(ffi::SQLITE_TOOBIG);
                }
                Ok(f)
            })();
            match result {
                Ok(f) => {
                    ptr::write(out.cast::<LiveFile>(), f);
                    if !actual.is_null() {
                        *actual = kind | ffi::SQLITE_OPEN_READONLY;
                    }
                    ffi::SQLITE_OK
                }
                Err(rc) => rc,
            }
        }
    }
    fn file_size(f: &LiveFile) -> Result<i64, c_int> {
        if !f.valid() {
            return Err(ffi::SQLITE_IOERR_FSTAT);
        }
        let st = stat_at(f.source.directory.as_raw_fd(), &f.name)?;
        if st.st_size < 0 || !f.valid() {
            return Err(ffi::SQLITE_IOERR_FSTAT);
        }
        Ok(st.st_size)
    }
    unsafe extern "C" fn close_live(file: *mut ffi::sqlite3_file) -> c_int {
        unsafe {
            let f = &mut *file.cast::<LiveFile>();
            let shm_rc = f.shm.take().map_or(ffi::SQLITE_OK, |mut sh| sh.release());
            let main_rc = unlock_live(file, ffi::SQLITE_LOCK_NONE);
            let rc = if shm_rc != ffi::SQLITE_OK {
                shm_rc
            } else {
                main_rc
            };
            ptr::drop_in_place(file.cast::<LiveFile>());
            (*file).pMethods = ptr::null();
            rc
        }
    }
    unsafe extern "C" fn read_live(
        file: *mut ffi::sqlite3_file,
        buf: *mut c_void,
        n: c_int,
        offset: i64,
    ) -> c_int {
        unsafe {
            let f = &mut *file.cast::<LiveFile>();
            if n < 0 || offset < 0 || offset.checked_add(n as i64).is_none() || !f.valid() {
                return ffi::SQLITE_IOERR_READ;
            }
            if !f.main && offset + n as i64 > MAX_WAL_BYTES {
                return ffi::SQLITE_TOOBIG;
            }
            if !f.spend(n as u64) {
                return ffi::SQLITE_IOERR_READ;
            }
            let mut done = 0;
            while done < n as usize {
                let got = libc::pread(
                    f.data.fd,
                    buf.cast::<u8>().add(done).cast(),
                    n as usize - done,
                    offset + done as i64,
                );
                // Includes EOF/short-read paths: SQLite may otherwise treat
                // SHORT_READ as usable zero-filled data after a replacement.
                if !f.valid() {
                    return ffi::SQLITE_IOERR_READ;
                }
                if got < 0 {
                    if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                        continue;
                    }
                    return ffi::SQLITE_IOERR_READ;
                }
                if got == 0 {
                    ptr::write_bytes(buf.cast::<u8>().add(done), 0, n as usize - done);
                    return if f.main && offset == 0 && n >= 20 {
                        ffi::SQLITE_CANTOPEN
                    } else {
                        ffi::SQLITE_IOERR_SHORT_READ
                    };
                }
                done += got as usize;
            }
            if !f.valid() {
                return ffi::SQLITE_IOERR_READ;
            }
            // The live path is exclusively for WAL databases, never a relaxed
            // rollback-journal read after a BUSY exclusive attempt.
            if f.main && offset == 0 && n >= 20 {
                let b = std::slice::from_raw_parts(buf.cast::<u8>(), n as usize);
                if &b[..16] != b"SQLite format 3\0" || b[18] != 2 || b[19] != 2 {
                    return ffi::SQLITE_CANTOPEN;
                }
            }
            ffi::SQLITE_OK
        }
    }
    unsafe extern "C" fn size_live(file: *mut ffi::sqlite3_file, out: *mut i64) -> c_int {
        unsafe {
            let f = &*file.cast::<LiveFile>();
            match file_size(f) {
                Ok(n) if f.main || n <= MAX_WAL_BYTES => {
                    *out = n;
                    ffi::SQLITE_OK
                }
                Ok(_) => ffi::SQLITE_TOOBIG,
                Err(rc) => rc,
            }
        }
    }
    unsafe extern "C" fn lock_live(file: *mut ffi::sqlite3_file, level: c_int) -> c_int {
        unsafe {
            let f = &mut *file.cast::<LiveFile>();
            if !f.main || !f.valid() {
                return ffi::SQLITE_IOERR_LOCK;
            }
            if level != ffi::SQLITE_LOCK_SHARED {
                return ffi::SQLITE_READONLY;
            }
            if f.locked {
                return ffi::SQLITE_OK;
            }
            let Ok(mut counts) = f.data.counts.lock() else {
                return ffi::SQLITE_IOERR_LOCK;
            };
            if counts[0] == u32::MAX {
                return ffi::SQLITE_IOERR_LOCK;
            }
            if counts[0] == 0 {
                let rc = os_lock(&f.data, libc::F_RDLCK as c_int, 0x40000000, 1);
                if rc != ffi::SQLITE_OK {
                    return rc;
                }
                let rc = os_lock(&f.data, libc::F_RDLCK as c_int, 0x40000002, 510);
                let released = os_lock(&f.data, libc::F_UNLCK as c_int, 0x40000000, 1);
                if rc != ffi::SQLITE_OK {
                    return rc;
                }
                if released != ffi::SQLITE_OK {
                    os_lock(&f.data, libc::F_UNLCK as c_int, 0x40000002, 510);
                    return released;
                }
            }
            counts[0] += 1;
            f.locked = true;
            ffi::SQLITE_OK
        }
    }
    unsafe extern "C" fn unlock_live(file: *mut ffi::sqlite3_file, level: c_int) -> c_int {
        unsafe {
            let f = &mut *file.cast::<LiveFile>();
            if level == ffi::SQLITE_LOCK_SHARED || !f.locked {
                return ffi::SQLITE_OK;
            }
            if level != ffi::SQLITE_LOCK_NONE {
                return ffi::SQLITE_IOERR_UNLOCK;
            }
            let rc = shared(&f.data, 0, 0x40000002, 510, false);
            if rc == ffi::SQLITE_OK {
                f.locked = false;
            }
            rc
        }
    }
    unsafe extern "C" fn reserved_live(file: *mut ffi::sqlite3_file, out: *mut c_int) -> c_int {
        unsafe {
            let f = &*file.cast::<LiveFile>();
            if !f.valid() {
                return ffi::SQLITE_IOERR_CHECKRESERVEDLOCK;
            }
            match conflict(&f.data, 0x40000001) {
                Ok(kind) => {
                    *out = c_int::from(kind != libc::F_UNLCK as c_int);
                    ffi::SQLITE_OK
                }
                Err(rc) => rc,
            }
        }
    }
    unsafe extern "C" fn control_live(
        file: *mut ffi::sqlite3_file,
        op: c_int,
        out: *mut c_void,
    ) -> c_int {
        unsafe {
            let f = &*file.cast::<LiveFile>();
            match op {
                ffi::SQLITE_FCNTL_LOCKSTATE => {
                    *out.cast::<c_int>() = if f.locked {
                        ffi::SQLITE_LOCK_SHARED
                    } else {
                        ffi::SQLITE_LOCK_NONE
                    };
                    ffi::SQLITE_OK
                }
                ffi::SQLITE_FCNTL_HAS_MOVED => {
                    *out.cast::<c_int>() = c_int::from(!f.valid());
                    ffi::SQLITE_OK
                }
                _ => ffi::SQLITE_NOTFOUND,
            }
        }
    }
    unsafe extern "C" fn shm_map(
        file: *mut ffi::sqlite3_file,
        page: c_int,
        bytes: c_int,
        extend: c_int,
        out: *mut *mut c_void,
    ) -> c_int {
        unsafe {
            *out = ptr::null_mut();
            let f = &mut *file.cast::<LiveFile>();
            if !f.main
                || !f.locked
                || !f.valid()
                || page < 0
                || bytes != REGION as c_int
                || extend != 0
                || page as usize >= MAX_SHM / REGION
            {
                return ffi::SQLITE_IOERR_SHMMAP;
            }
            if f.shm.is_none() {
                let name = f.source.name(b"-shm");
                let n = match node(f.source.directory.as_raw_fd(), &name) {
                    Ok(n) => n,
                    Err(rc) => return rc,
                };
                let wal = match stat_at(f.source.directory.as_raw_fd(), &f.source.name(b"-wal")) {
                    Ok(st) => stat_id(&st),
                    Err(rc) => return rc,
                };
                if n.id == f.data.id || n.id == wal {
                    return ffi::SQLITE_CANTOPEN;
                }
                // A valid writable owner (or an already attached reader) must
                // keep the DMS alive. Never rebuild or trust abandoned SHM.
                let Ok(mut counts) = n.counts.lock() else {
                    return ffi::SQLITE_IOERR_LOCK;
                };
                if counts[8] == 0 {
                    let owner = conflict(&n, 128);
                    if owner != Ok(libc::F_RDLCK as c_int) {
                        return ffi::SQLITE_IOERR_SHMOPEN;
                    }
                }
                if counts[8] == u32::MAX {
                    return ffi::SQLITE_IOERR_LOCK;
                }
                if counts[8] == 0 {
                    let rc = os_lock(&n, libc::F_RDLCK as c_int, 128, 1);
                    if rc != ffi::SQLITE_OK {
                        return rc;
                    }
                }
                counts[8] += 1;
                drop(counts);
                f.shm = Some(Shm {
                    node: n,
                    maps: Vec::new(),
                    held: 1 << 8,
                });
            }
            let name = f.source.name(b"-shm");
            let sh = f.shm.as_ref().unwrap();
            let fixed = f.source.shm_identity.get_or_init(|| sh.node.clone());
            if fixed.id != sh.node.id || !f.valid() {
                return ffi::SQLITE_IOERR_SHMMAP;
            }
            let st = match stat_at(f.source.directory.as_raw_fd(), &name) {
                Ok(st) => st,
                Err(rc) => return rc,
            };
            if stat_id(&st) != sh.node.id
                || st.st_size < 0
                || st.st_size as usize > MAX_SHM
                || (page as usize + 1) * REGION > st.st_size as usize
            {
                return ffi::SQLITE_IOERR_SHMSIZE;
            }
            let target = page as usize;
            while f.shm.as_ref().unwrap().maps.len() <= target {
                if !f.spend(REGION as u64) {
                    return ffi::SQLITE_IOERR_SHMMAP;
                }
                let sh = f.shm.as_mut().unwrap();
                let p = libc::mmap(
                    ptr::null_mut(),
                    REGION,
                    libc::PROT_READ,
                    libc::MAP_SHARED,
                    sh.node.fd,
                    (sh.maps.len() * REGION) as i64,
                );
                if p == libc::MAP_FAILED {
                    return ffi::SQLITE_IOERR_SHMMAP;
                }
                sh.maps.push(p);
            }
            // Also checks repeated/cached maps; never publish a pointer from
            // a lock domain whose source name was replaced during mapping.
            if !f.valid() {
                return ffi::SQLITE_IOERR_SHMMAP;
            }
            *out = f.shm.as_ref().unwrap().maps[target];
            // walIndexPageRealloc sets WAL_SHM_RDONLY and converts this to OK.
            // walTryBeginRead then ONLY uses existing readmarks and OS locks.
            ffi::SQLITE_READONLY
        }
    }
    unsafe extern "C" fn shm_lock(
        file: *mut ffi::sqlite3_file,
        offset: c_int,
        n: c_int,
        flags: c_int,
    ) -> c_int {
        unsafe {
            let f = &mut *file.cast::<LiveFile>();
            let valid = f.valid();
            let Some(sh) = f.shm.as_mut() else {
                return ffi::SQLITE_IOERR_SHMLOCK;
            };
            if !(0..8).contains(&offset) || n != 1 {
                return ffi::SQLITE_IOERR_SHMLOCK;
            }
            let slot = offset as usize;
            let bit = 1 << slot;
            if flags == ffi::SQLITE_SHM_UNLOCK | ffi::SQLITE_SHM_SHARED {
                if sh.held & bit == 0 {
                    return ffi::SQLITE_OK;
                }
                let rc = shared(&sh.node, slot, 120 + offset as i64, 1, false);
                if rc == ffi::SQLITE_OK {
                    sh.held &= !bit;
                }
                return rc;
            }
            if flags != ffi::SQLITE_SHM_LOCK | ffi::SQLITE_SHM_SHARED {
                return ffi::SQLITE_READONLY;
            }
            if !valid || !f.source.valid_node(&sh.node, &f.source.name(b"-shm")) {
                return ffi::SQLITE_IOERR_SHMLOCK;
            }
            if sh.held & bit != 0 {
                return ffi::SQLITE_OK;
            }
            let rc = shared(&sh.node, slot, 120 + offset as i64, 1, true);
            if rc == ffi::SQLITE_OK {
                sh.held |= bit;
            }
            rc
        }
    }
    unsafe extern "C" fn barrier(_: *mut ffi::sqlite3_file) {
        std::sync::atomic::fence(Ordering::SeqCst);
    }
    unsafe extern "C" fn unmap(file: *mut ffi::sqlite3_file, _: c_int) -> c_int {
        unsafe {
            (*file.cast::<LiveFile>())
                .shm
                .take()
                .map_or(ffi::SQLITE_OK, |mut sh| sh.release())
        }
    }
    unsafe extern "C" fn sector(_: *mut ffi::sqlite3_file) -> c_int {
        512
    }
    static IO: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
        iVersion: 2,
        xClose: Some(close_live),
        xRead: Some(read_live),
        xWrite: Some(write),
        xTruncate: Some(truncate),
        xSync: Some(sync),
        xFileSize: Some(size_live),
        xLock: Some(lock_live),
        xUnlock: Some(unlock_live),
        xCheckReservedLock: Some(reserved_live),
        xFileControl: Some(control_live),
        xSectorSize: Some(sector),
        xDeviceCharacteristics: Some(characteristics),
        xShmMap: Some(shm_map),
        xShmLock: Some(shm_lock),
        xShmBarrier: Some(barrier),
        xShmUnmap: Some(unmap),
        xFetch: None,
        xUnfetch: None,
    };
    #[cfg(test)]
    pub(super) fn proof_read(path: &Path) -> Vec<u8> {
        let canonical = path
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .join(path.file_name().unwrap());
        let c = CString::new(canonical.as_os_str().as_bytes()).unwrap();
        let source = Source::new(&c).unwrap();
        let n = stat_at(source.directory.as_raw_fd(), &source.base)
            .unwrap()
            .st_size as usize;
        let mut bytes = vec![0; n];
        assert_eq!(
            unsafe { libc::pread(source.main.fd, bytes.as_mut_ptr().cast(), n, 0) },
            n as isize
        );
        bytes
    }
    pub(super) unsafe fn register_live(base: *mut ffi::sqlite3_vfs) -> c_int {
        unsafe {
            let mut vfs: ffi::sqlite3_vfs = std::mem::zeroed();
            vfs.iVersion = 1;
            vfs.szOsFile = std::mem::size_of::<LiveFile>() as c_int;
            vfs.mxPathname = (*base).mxPathname;
            vfs.zName = c"ai-cube-index-live-readonly-v1".as_ptr();
            vfs.pAppData = base.cast();
            vfs.xOpen = Some(open_live);
            vfs.xDelete = Some(delete);
            vfs.xAccess = Some(access);
            vfs.xFullPathname = Some(full_pathname);
            vfs.xRandomness = Some(randomness);
            vfs.xSleep = Some(sleep);
            vfs.xCurrentTime = Some(current_time);
            let vfs = Box::into_raw(Box::new(vfs));
            let rc = ffi::sqlite3_vfs_register(vfs, 0);
            if rc != ffi::SQLITE_OK {
                drop(Box::from_raw(vfs));
            }
            rc
        }
    }
    #[cfg(test)]
    mod shm_identity_regressions {
        use super::*;
        use std::process::Command;

        fn spawn_helper(db: &Path, mode: &str) {
            let name = format!(
                "{}::shm_identity_foreign_helper",
                module_path!().split_once("::").unwrap().1
            );
            let out = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &name, "--ignored", "--nocapture"])
                .env("AI_CUBE_SHM_IDENTITY_DB", db)
                .env("AI_CUBE_SHM_IDENTITY_MODE", mode)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "helper {mode}: {} {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            println!("helper {mode}: passed");
        }
        unsafe fn raw(c: &Connection, op: c_int) -> *mut LiveFile {
            unsafe {
                let mut p: *mut ffi::sqlite3_file = ptr::null_mut();
                assert_eq!(
                    ffi::sqlite3_file_control(
                        c.handle(),
                        c"main".as_ptr(),
                        op,
                        (&mut p as *mut *mut ffi::sqlite3_file).cast()
                    ),
                    ffi::SQLITE_OK
                );
                assert!(!p.is_null() && ptr::eq((*p).pMethods, &IO));
                p.cast()
            }
        }
        fn fixture(wal: bool) -> (tempfile::TempDir, Connection, PathBuf) {
            let root = tempfile::tempdir().unwrap();
            let db = root.path().canonicalize().unwrap().join("catalog.db");
            let w = Connection::open(&db).unwrap();
            w.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
                .unwrap();
            w.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE project(worktree TEXT); CREATE TABLE cold(value TEXT); INSERT INTO project VALUES('/v1'); INSERT INTO cold VALUES('/v1'); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
            if wal {
                w.execute("UPDATE cold SET value='/wal-v1'", []).unwrap();
            }
            (root, w, db)
        }
        fn replacement_case(wal: bool) {
            let (_root, w, db) = fixture(wal);
            let c = open_readonly(&db).unwrap();
            let initial: String = c
                .query_row("SELECT worktree FROM project", [], |r| r.get(0))
                .unwrap();
            assert_eq!(initial, "/v1");
            assert!(!c.is_autocommit());
            let main_id = id(&fs::metadata(&db).unwrap());
            let wal_path = db.with_file_name("catalog.db-wal");
            let wal_id = id(&fs::metadata(&wal_path).unwrap());
            let (old_shm, data_node) = unsafe {
                let main = raw(&c, ffi::SQLITE_FCNTL_FILE_POINTER);
                let data = if wal {
                    raw(&c, ffi::SQLITE_FCNTL_JOURNAL_POINTER)
                } else {
                    main
                };
                // Establish the formerly unguarded cold page's exact read path.
                // sqlite_schema is already initialized; this query cannot load cold's data page.
                let page: i64 = c
                    .query_row(
                        "SELECT rootpage FROM sqlite_schema WHERE name='cold'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert!(page > 1);
                (
                    (*main).shm.as_ref().unwrap().node.clone(),
                    (*data).data.clone(),
                )
            };
            drop(w);
            fs::rename(
                db.with_file_name("catalog.db-shm"),
                db.with_file_name("previous-shm"),
            )
            .unwrap();
            spawn_helper(&db, if wal { "append" } else { "checkpoint" });
            assert_eq!(id(&fs::metadata(&db).unwrap()), main_id);
            assert_eq!(id(&fs::metadata(&wal_path).unwrap()), wal_id);
            assert_ne!(
                id(&fs::metadata(db.with_file_name("catalog.db-shm")).unwrap()),
                old_shm.id
            );
            let result = c.query_row("SELECT value FROM cold", [], |r| r.get::<_, String>(0));
            println!("initial_project={initial}; cold={result:?}; wal_case={wal}");
            drop(c);
            assert_eq!(
                *old_shm.counts.lock().unwrap(),
                [0; 9],
                "old SHM locks released after error"
            );
            if !wal {
                assert_eq!(
                    *data_node.counts.lock().unwrap(),
                    [0; 9],
                    "old main lock released"
                );
            }
            spawn_helper(&db, "probe-released");
            assert!(
                result.is_err(),
                "cold {} I/O must reject replaced SHM, got {result:?}",
                if wal { "WAL" } else { "main" }
            );
        }
        #[test]
        fn shm_identity_main_cold_page_replacement_is_rejected() {
            replacement_case(false);
        }
        #[test]
        fn shm_identity_wal_cold_page_replacement_is_rejected() {
            replacement_case(true);
        }
        #[test]
        fn shm_identity_read_size_and_repeat_map_reject_replacement_but_unlock_works() {
            let (_root, w, db) = fixture(true);
            let c = open_readonly(&db).unwrap();
            let (main, wal) = unsafe {
                (
                    raw(&c, ffi::SQLITE_FCNTL_FILE_POINTER),
                    raw(&c, ffi::SQLITE_FCNTL_JOURNAL_POINTER),
                )
            };
            let old = unsafe { (*main).shm.as_ref().unwrap().node.clone() };
            drop(w);
            fs::rename(
                db.with_file_name("catalog.db-shm"),
                db.with_file_name("previous-shm"),
            )
            .unwrap();
            // No native writer is needed for direct callback checks. Another inode
            // at the original name isolates identity from an absent-sidecar error.
            fs::write(db.with_file_name("catalog.db-shm"), vec![0; REGION]).unwrap();
            unsafe {
                for f in [main, wal] {
                    let mut bytes = [0x5a; 8];
                    let mut n = -1;
                    assert_eq!(
                        read_live(f.cast(), bytes.as_mut_ptr().cast(), 8, 0),
                        ffi::SQLITE_IOERR_READ
                    );
                    assert_eq!(bytes, [0x5a; 8]);
                    assert_eq!(size_live(f.cast(), &mut n), ffi::SQLITE_IOERR_FSTAT);
                    assert_eq!(n, -1);
                }
                let mut page = ptr::null_mut();
                assert_eq!(
                    shm_map(main.cast(), 0, REGION as c_int, 0, &mut page),
                    ffi::SQLITE_IOERR_SHMMAP
                );
                assert!(page.is_null());
                let held = (*main).shm.as_ref().unwrap().held;
                for slot in 0..8 {
                    if held & (1 << slot) != 0 {
                        assert_eq!(
                            shm_lock(
                                main.cast(),
                                slot,
                                1,
                                ffi::SQLITE_SHM_UNLOCK | ffi::SQLITE_SHM_SHARED
                            ),
                            ffi::SQLITE_OK
                        );
                    }
                }
                assert_eq!(unmap(main.cast(), 0), ffi::SQLITE_OK);
                // A cached WAL handle must keep the source binding after the
                // main handle unmaps. Reattaching cannot silently rebind it.
                for f in [main, wal] {
                    let mut n = -1;
                    assert_eq!(size_live(f.cast(), &mut n), ffi::SQLITE_IOERR_FSTAT);
                    assert_eq!(n, -1);
                }
                page = ptr::null_mut();
                assert_eq!(
                    shm_map(main.cast(), 0, REGION as c_int, 0, &mut page),
                    ffi::SQLITE_IOERR_SHMMAP
                );
                assert!(page.is_null());
            }
            drop(c);
            assert_eq!(*old.counts.lock().unwrap(), [0; 9]);
            spawn_helper(&db, "probe-released");
        }
        #[test]
        #[ignore = "child native fixture writer or old-inode byte-lock probe"]
        fn shm_identity_foreign_helper() {
            let db = PathBuf::from(std::env::var_os("AI_CUBE_SHM_IDENTITY_DB").unwrap());
            let mode = std::env::var("AI_CUBE_SHM_IDENTITY_MODE").unwrap();
            if mode == "probe-released" {
                use std::os::unix::fs::OpenOptionsExt;
                for (path, start, len) in [
                    (db.with_file_name("previous-shm"), 120, 9),
                    (db.clone(), 0x40000002, 510),
                ] {
                    let f = fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .custom_flags(libc::O_NOFOLLOW)
                        .open(path)
                        .unwrap();
                    let mut lock = unsafe { std::mem::zeroed::<libc::flock>() };
                    lock.l_type = libc::F_WRLCK as _;
                    lock.l_whence = libc::SEEK_SET as _;
                    lock.l_start = start;
                    lock.l_len = len;
                    assert_eq!(
                        unsafe { libc::fcntl(f.as_raw_fd(), libc::F_SETLK, &lock) },
                        0,
                        "old byte locks must be released"
                    );
                }
                return;
            }
            let w = Connection::open(&db).unwrap();
            w.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
                .unwrap();
            w.busy_timeout(Duration::ZERO).unwrap();
            w.execute_batch("PRAGMA wal_autocheckpoint=0; UPDATE cold SET value='/v2'")
                .unwrap();
            if mode == "checkpoint" {
                let result = w
                    .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, i64>(2)?,
                        ))
                    })
                    .unwrap();
                assert_eq!(result, (0, 0, 0));
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{io::Write, os::unix::fs::PermissionsExt, process::Command};
        fn fixture() -> (tempfile::TempDir, Connection, PathBuf) {
            let root = tempfile::tempdir().unwrap();
            let db = root.path().canonicalize().unwrap().join("catalog.db");
            let w = Connection::open(&db).unwrap();
            w.busy_timeout(Duration::ZERO).unwrap();
            w.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
                .unwrap();
            w.execute_batch("PRAGMA journal_mode=WAL;PRAGMA wal_autocheckpoint=0;CREATE TABLE project(worktree TEXT);PRAGMA wal_checkpoint(TRUNCATE);INSERT INTO project VALUES('/v1');").unwrap();
            (root, w, db)
        }
        fn read(c: &Connection) -> Vec<String> {
            c.prepare("SELECT worktree FROM project ORDER BY rowid")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        }
        fn snap(
            root: &Path,
        ) -> std::collections::BTreeMap<PathBuf, (Option<Vec<u8>>, super::super::tests::Stamp)>
        {
            super::super::tests::snapshot_with(root, proof_read)
        }
        fn live(db: &Path) -> Connection {
            super::super::open_mode(db, true).unwrap()
        }
        unsafe fn file(c: &Connection) -> *mut LiveFile {
            unsafe {
                let mut p: *mut ffi::sqlite3_file = ptr::null_mut();
                assert_eq!(
                    ffi::sqlite3_file_control(
                        c.handle(),
                        c"main".as_ptr(),
                        ffi::SQLITE_FCNTL_FILE_POINTER,
                        (&mut p as *mut *mut ffi::sqlite3_file).cast()
                    ),
                    ffi::SQLITE_OK
                );
                assert!(ptr::eq((*p).pMethods, &IO));
                p.cast()
            }
        }
        #[test]
        fn live_ro_files_and_mutation_controls_preserve_all_source_stamps() {
            let (root, w, db) = fixture();
            for name in ["catalog.db", "catalog.db-wal", "catalog.db-shm"] {
                fs::set_permissions(db.with_file_name(name), fs::Permissions::from_mode(0o444))
                    .unwrap();
            }
            let before = snap(root.path());
            let c = open_readonly(&db).unwrap();
            assert_eq!(read(&c), ["/v1"]);
            assert!(c.is_readonly("main").unwrap());
            unsafe {
                let p = file(&c).cast::<ffi::sqlite3_file>();
                let io = &*(*p).pMethods;
                assert_eq!(
                    io.xWrite.unwrap()(p, c"x".as_ptr().cast(), 1, 0),
                    ffi::SQLITE_READONLY
                );
                assert_eq!(io.xTruncate.unwrap()(p, 0), ffi::SQLITE_READONLY);
                assert_eq!(io.xSync.unwrap()(p, 0), ffi::SQLITE_READONLY);
                for op in [
                    ffi::SQLITE_FCNTL_PRAGMA,
                    ffi::SQLITE_FCNTL_MMAP_SIZE,
                    ffi::SQLITE_FCNTL_FILE_POINTER,
                    ffi::SQLITE_FCNTL_SIZE_HINT,
                    99999,
                ] {
                    assert_eq!(
                        io.xFileControl.unwrap()(p, op, ptr::null_mut()),
                        ffi::SQLITE_NOTFOUND
                    );
                }
                assert_eq!(
                    io.xShmLock.unwrap()(p, 4, 1, ffi::SQLITE_SHM_LOCK | ffi::SQLITE_SHM_EXCLUSIVE),
                    ffi::SQLITE_READONLY
                );
                let mut map = ptr::null_mut();
                assert_ne!(
                    io.xShmMap.unwrap()(p, 0, 32768, 1, &mut map),
                    ffi::SQLITE_OK
                );
                assert!(map.is_null());
            }
            assert!(
                c.execute("INSERT INTO project VALUES('/forbidden')", [])
                    .is_err()
            );
            drop(c);
            assert_eq!(snap(root.path()), before);
            w.execute("INSERT INTO project VALUES('/writer-alive')", [])
                .unwrap();
        }
        #[test]
        fn readers_hold_snapshot_across_other_close_checkpoint_and_reset() {
            let (_root, w, db) = fixture();
            let a = live(&db);
            let b = live(&db);
            assert_eq!(read(&a), ["/v1"]);
            w.execute("INSERT INTO project VALUES('/v2')", []).unwrap();
            drop(b);
            assert_eq!(read(&a), ["/v1"]);
            let checkpoint = || {
                w.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })
                .unwrap()
            };
            assert_eq!(checkpoint().0, 1);
            assert_eq!(read(&a), ["/v1"]);
            drop(a);
            assert_eq!(checkpoint().0, 0);
            let c = live(&db);
            assert_eq!(read(&c), ["/v1", "/v2"]);
            drop(c);
            for i in 0..8 {
                w.execute("INSERT INTO project VALUES(?1)", [format!("/reset-{i}")])
                    .unwrap();
                let r = live(&db);
                assert_eq!(read(&r).len(), 3 + i);
                drop(r);
                assert_eq!(checkpoint().0, 0);
            }
        }
        #[test]
        fn missing_stale_or_short_shm_fails_without_recovery() {
            let (root, w, db) = fixture();
            drop(w);
            let before = snap(root.path());
            assert!(open_mode(&db, true).is_err());
            assert_eq!(snap(root.path()), before);
            // Exclusive fallback is still applicable for an abandoned source.
            assert_eq!(read(&open_readonly(&db).unwrap()), ["/v1"]);
            fs::remove_file(db.with_file_name("catalog.db-shm")).unwrap();
            let before = snap(root.path());
            assert!(open_mode(&db, true).is_err());
            assert_eq!(snap(root.path()), before);
            let (root, w, db) = fixture();
            let h = fs::OpenOptions::new()
                .write(true)
                .open(db.with_file_name("catalog.db-shm"))
                .unwrap();
            h.set_len(8).unwrap();
            let before = snap(root.path());
            assert!(open_mode(&db, true).is_err());
            assert_eq!(snap(root.path()), before);
            drop(w);
            drop(h);
        }
        #[test]
        fn invalid_header_or_no_readmark_is_error_without_shared_writes() {
            for header in [true, false] {
                let (root, w, db) = fixture();
                let mut h = fs::OpenOptions::new()
                    .write(true)
                    .open(db.with_file_name("catalog.db-shm"))
                    .unwrap();
                use std::io::{Seek, SeekFrom};
                if header {
                    h.write_all(&[0; 96]).unwrap();
                } else {
                    h.seek(SeekFrom::Start(100)).unwrap();
                    for _ in 0..5 {
                        h.write_all(&u32::MAX.to_ne_bytes()).unwrap();
                    }
                }
                let before = snap(root.path());
                assert!(open_mode(&db, true).is_err());
                assert_eq!(snap(root.path()), before);
                drop(w);
                drop(h);
            }
        }
        #[test]
        fn parent_replacement_rejects_cold_metadata_page() {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("source");
            fs::create_dir(&dir).unwrap();
            let db = dir.canonicalize().unwrap().join("catalog.db");
            let w = Connection::open(&db).unwrap();
            w.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
                .unwrap();
            w.execute_batch("PRAGMA journal_mode=WAL;CREATE TABLE project(worktree TEXT);INSERT INTO project VALUES('/old');").unwrap();
            let c = live(&db);
            fs::rename(&dir, root.path().join("old-source")).unwrap();
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("catalog.db"), b"different-source").unwrap();
            assert!(
                c.query_row("SELECT worktree FROM project", [], |r| r
                    .get::<_, String>(0))
                    .is_err()
            );
            drop(c);
            drop(w);
        }
        #[test]
        fn symlink_fifo_and_aux_identity_replacement_are_refused() {
            for suffix in ["", "-wal", "-shm"] {
                let (_root, w, db) = fixture();
                let original = db.with_file_name(format!("catalog.db{suffix}"));
                let moved = db.with_file_name(format!("saved{suffix}"));
                fs::rename(&original, &moved).unwrap();
                std::os::unix::fs::symlink(&moved, &original).unwrap();
                assert!(open_mode(&db, true).is_err());
                assert_eq!(fs::read_link(&original).unwrap(), moved);
                drop(w);
            }
            let (_root, w, db) = fixture();
            let c = live(&db);
            let wal = db.with_file_name("catalog.db-wal");
            fs::rename(&wal, db.with_file_name("saved-wal")).unwrap();
            fs::write(&wal, b"replacement").unwrap();
            assert!(
                c.query_row("SELECT worktree FROM project", [], |r| r
                    .get::<_, String>(0))
                    .is_err()
            );
            drop(c);
            drop(w);
            let root = tempfile::tempdir().unwrap();
            let fifo = root.path().join("fifo.db");
            let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            let start = std::time::Instant::now();
            assert!(open_mode(&fifo, true).is_err());
            assert!(start.elapsed() < Duration::from_secs(1));
        }
        #[test]
        fn live_limits_and_budget_enforcement_are_not_relaxed() {
            let (root, _w, db) = fixture();
            let before = snap(root.path());
            let c = live(&db);
            assert!(
                c.prepare(&format!(
                    "SELECT 1 /*{}*/",
                    "x".repeat(MAX_SQL_BYTES as usize)
                ))
                .is_err()
            );
            assert!(
                c.query_row("SELECT zeroblob(?1)", [MAX_VALUE_BYTES + 1], |r| r
                    .get::<_, Vec<u8>>(0))
                    .is_err()
            );
            unsafe {
                let p = file(&c);
                let mut buf = [0x5a; 8];
                let initial = (*p).total.load(Ordering::Relaxed);
                let left = (*p).left;
                (*p).left = 4;
                assert_eq!(
                    read_live(p.cast(), buf.as_mut_ptr().cast(), 8, 0),
                    ffi::SQLITE_IOERR_READ
                );
                assert_eq!(buf, [0x5a; 8]);
                assert_eq!((*p).total.load(Ordering::Relaxed), initial);
                (*p).left = left;
                (*p).total.store(4, Ordering::Relaxed);
                assert_eq!(
                    read_live(p.cast(), buf.as_mut_ptr().cast(), 8, 0),
                    ffi::SQLITE_IOERR_READ
                );
                assert_eq!(buf, [0x5a; 8]);
            }
            drop(c);
            assert_eq!(snap(root.path()), before);
        }
        #[test]
        fn multiple_readonly_shm_regions_find_late_metadata_frames() {
            let root = tempfile::tempdir().unwrap();
            let db = root.path().canonicalize().unwrap().join("catalog.db");
            let w = Connection::open(&db).unwrap();
            w.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
                .unwrap();
            w.execute_batch("PRAGMA page_size=512;PRAGMA journal_mode=WAL;PRAGMA wal_autocheckpoint=0;CREATE TABLE project(worktree TEXT);BEGIN").unwrap();
            let path = format!("/{}", "x".repeat(4095));
            for _ in 0..2000 {
                w.execute("INSERT INTO project VALUES(?1)", [&path])
                    .unwrap();
            }
            w.execute_batch("COMMIT").unwrap();
            let before = snap(root.path());
            let c = live(&db);
            assert_eq!(
                c.query_row("SELECT worktree FROM project WHERE rowid=2000", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
                path
            );
            unsafe {
                assert!((*file(&c)).shm.as_ref().unwrap().maps.len() > 1);
            }
            drop(c);
            assert_eq!(snap(root.path()), before);
        }
        #[test]
        fn cached_descriptors_do_not_bypass_new_open_permissions() {
            use std::io::{Read, Seek, SeekFrom};
            let (root, w, db) = fixture();
            let c = live(&db);
            drop(c);
            let wal = db.with_file_name("catalog.db-wal");
            let mut h = fs::File::open(&wal).unwrap();
            fs::set_permissions(&wal, fs::Permissions::from_mode(0o000)).unwrap();
            let mut capture = || {
                super::super::tests::snapshot_with(root.path(), |path| {
                    if path.file_name() == wal.file_name() {
                        let mut bytes = Vec::new();
                        h.seek(SeekFrom::Start(0)).unwrap();
                        h.read_to_end(&mut bytes).unwrap();
                        bytes
                    } else {
                        proof_read(path)
                    }
                })
            };
            let before = capture();
            assert!(open_mode(&db, true).is_err());
            assert_eq!(capture(), before);
            drop(w);
            drop(h);
        }
        #[test]
        fn live_oversized_wal_shm_and_schema_are_errors_unchanged() {
            for suffix in ["-wal", "-shm"] {
                let (root, w, db) = fixture();
                let path = db.with_file_name(format!("catalog.db{suffix}"));
                let h = fs::OpenOptions::new().write(true).open(&path).unwrap();
                h.set_len(if suffix == "-wal" {
                    MAX_WAL_BYTES as u64 + 1
                } else {
                    MAX_SHM as u64 + 1
                })
                .unwrap();
                let before = snap(root.path());
                assert!(open_mode(&db, true).is_err());
                assert_eq!(snap(root.path()), before);
                drop(w);
                drop(h);
            }
            let (root, w, db) = fixture();
            w.execute_batch(&format!(
                "CREATE VIEW huge AS /*{}*/ SELECT 1",
                "x".repeat(MAX_SQL_BYTES as usize + 1)
            ))
            .unwrap();
            let before = snap(root.path());
            assert!(open_mode(&db, true).is_err());
            assert_eq!(snap(root.path()), before);
        }
        #[test]
        fn readonly_live_open_reports_header_publication_window_and_recovers_after_publish() {
            use std::os::unix::fs::FileExt;

            let (_root, w, db) = fixture();
            let anchor = live(&db);
            let wal = PathBuf::from(format!("{}-wal", db.display()));
            let shm = PathBuf::from(format!("{}-shm", db.display()));
            // Own synthetic fixture only. SQLite 3.50.2 walIndexWriteHdr
            // publishes two 48-byte headers with a barrier between the copies.
            let publisher = std::fs::OpenOptions::new().read(true).write(true).open(shm).unwrap();
            let mut previous = [0u8; 96];
            publisher.read_exact_at(&mut previous, 0).unwrap();
            assert_eq!(&previous[..48], &previous[48..]);
            w.execute("INSERT INTO project VALUES('/published')", []).unwrap();
            let mut headers = [0u8; 96];
            publisher.read_exact_at(&mut headers, 0).unwrap();
            assert_eq!(&headers[..48], &headers[48..]);
            assert_ne!(&previous[..48], &headers[..48]);
            let original_db = std::fs::read(&db).unwrap();
            let original_wal = std::fs::read(&wal).unwrap();
            struct Restore<'a>(&'a std::fs::File, [u8; 96]);
            impl Drop for Restore<'_> {
                fn drop(&mut self) { let _ = self.0.write_all_at(&self.1, 0); }
            }
            let restore = Restore(&publisher, headers);
            // Both copies are valid native generations: copy 1 is still old
            // while copy 2 has the new committed generation.
            let mut publishing = headers;
            publishing[..48].copy_from_slice(&previous[..48]);
            publisher.write_all_at(&publishing, 0).unwrap();
            let result = open_mode(&db, true);
            let code = match result {
                Err(rusqlite::Error::SqliteFailure(error, _)) => error.extended_code,
                _ => panic!("readonly publication window must be reported as unavailable"),
            };
            let mut after_read = [0u8; 96];
            publisher.read_exact_at(&mut after_read, 0).unwrap();
            publisher.write_all_at(&headers, 0).unwrap();
            assert_eq!(code, ffi::SQLITE_READONLY_RECOVERY);
            assert_eq!(after_read, publishing);
            assert_eq!(std::fs::read(&db).unwrap(), original_db);
            assert_eq!(std::fs::read(wal).unwrap(), original_wal);
            let ready = live(&db);
            assert_eq!(read(&ready), ["/v1", "/published"]);
            assert_eq!(read(&anchor), ["/v1"]);
            drop(ready);
            drop(restore);
            drop(anchor);
            drop(w);
            drop(publisher);
        }

        #[test]
        fn concurrent_readers_are_consistent_during_native_commits_and_checkpoints() {
            let (_root, w, db) = fixture();
            let anchor = live(&db);
            let ready = std::sync::Barrier::new(3);
            let committed = std::sync::Barrier::new(3);
            std::thread::scope(|scope| {
                let mut readers = Vec::new();
                for _ in 0..2 {
                    let path = &db;
                    let ready = &ready;
                    let committed = &committed;
                    readers.push(scope.spawn(move || {
                        // Establish the read snapshot before publishing starts.
                        // Opening within a two-copy WAL header publication can
                        // legitimately return SQLITE_READONLY_RECOVERY; the
                        // dedicated publication-window test covers that case.
                        let c = live(path);
                        let before = read(&c);
                        ready.wait();
                        committed.wait();
                        for _ in 0..8 {
                            std::thread::yield_now();
                            assert_eq!(read(&c), before);
                            assert!(!before.is_empty());
                        }
                    }));
                }
                ready.wait();
                for i in 0..32 {
                    w.execute("INSERT INTO project VALUES(?1)", [format!("/commit-{i}")])
                        .unwrap();
                    if i == 0 { committed.wait(); }
                    w.execute_batch("PRAGMA wal_checkpoint(PASSIVE)").unwrap();
                }
                for thread in readers {
                    thread.join().unwrap();
                }
            });
            assert_eq!(read(&anchor), ["/v1"]);
            drop(anchor);
            assert_eq!(
                w.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        #[test]
        fn native_writer_lock_survives_reader_close_and_error() {
            let (_root, w, db) = fixture();
            w.execute_batch("BEGIN IMMEDIATE").unwrap();
            let c = live(&db);
            assert_eq!(read(&c), ["/v1"]);
            drop(c);
            // Failed opens must retain their source descriptors too.
            let mut f = fs::OpenOptions::new()
                .write(true)
                .open(db.with_file_name("catalog.db-shm"))
                .unwrap();
            use std::io::{Seek, SeekFrom};
            f.seek(SeekFrom::Start(100)).unwrap();
            for _ in 0..5 {
                f.write_all(&u32::MAX.to_ne_bytes()).unwrap();
            }
            assert!(open_mode(&db, true).is_err());
            let test = format!(
                "{}::native_lock_probe",
                module_path!().split_once("::").unwrap().1
            );
            let child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &test, "--ignored", "--nocapture"])
                .env("AI_CUBE_LIVE_LOCK_DB", &db)
                .output()
                .unwrap();
            assert!(
                child.status.success(),
                "{}",
                String::from_utf8_lossy(&child.stdout)
            );
            w.execute_batch("ROLLBACK").unwrap();
            drop(w);
            drop(f);
        }
        #[test]
        #[ignore = "child verifies native writer lock in a different process"]
        fn native_lock_probe() {
            let path = std::env::var_os("AI_CUBE_LIVE_LOCK_DB").unwrap();
            let c = CString::new(Path::new(&path).as_os_str().as_bytes()).unwrap();
            let source = Source::new(&c).unwrap();
            let n = node(source.directory.as_raw_fd(), &source.name(b"-shm")).unwrap();
            assert_eq!(conflict(&n, 120), Ok(libc::F_WRLCK as c_int));
        }
        #[test]
        fn fd_retention_is_bounded_in_fresh_process() {
            let test = format!(
                "{}::retention_probe",
                module_path!().split_once("::").unwrap().1
            );
            let out = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &test, "--ignored", "--nocapture"])
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
        #[test]
        #[ignore = "child exhausts only its own bounded descriptor cache"]
        fn retention_probe() {
            let root = tempfile::tempdir().unwrap();
            let dir = fs::File::open(root.path()).unwrap();
            for i in 0..MAX_RETAINED_FDS + 1 {
                let name = CString::new(format!("f{i}")).unwrap();
                fs::write(root.path().join(format!("f{i}")), b"x").unwrap();
                assert_eq!(node(dir.as_raw_fd(), &name).is_ok(), i < MAX_RETAINED_FDS);
            }
            assert!(node(dir.as_raw_fd(), c"f0").is_ok());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        fs,
        time::{Instant, SystemTime},
    };

    #[derive(Debug, PartialEq, Eq)]
    pub(super) struct Stamp {
        len: u64,
        modified: Option<SystemTime>,
        created: Option<SystemTime>,
        readonly: bool,
        #[cfg(unix)]
        unix: (u64, u64, u64, u32, u32, u32, u64, u64, i64, i64, i64, i64),
    }
    fn snapshot(root: &Path) -> BTreeMap<std::path::PathBuf, (Option<Vec<u8>>, Stamp)> {
        snapshot_with(root, |path| fs::read(path).unwrap())
    }
    pub(super) fn snapshot_with(
        root: &Path,
        mut read: impl FnMut(&Path) -> Vec<u8>,
    ) -> BTreeMap<std::path::PathBuf, (Option<Vec<u8>>, Stamp)> {
        let paths = std::iter::once(root.to_owned()).chain(
            fs::read_dir(root)
                .unwrap()
                .map(|entry| entry.unwrap().path()),
        );
        paths
            .map(|path| {
                let bytes = path.is_file().then(|| read(&path));
                let meta = fs::metadata(&path).unwrap();
                // Reads can update atime in the kernel. Compare write/change
                // metadata and exact bytes instead; never restore source times.
                #[cfg(unix)]
                let unix = {
                    use std::os::unix::fs::MetadataExt;
                    (
                        meta.dev(),
                        meta.ino(),
                        meta.nlink(),
                        meta.mode(),
                        meta.uid(),
                        meta.gid(),
                        meta.rdev(),
                        meta.blocks(),
                        meta.mtime(),
                        meta.mtime_nsec(),
                        meta.ctime(),
                        meta.ctime_nsec(),
                    )
                };
                (
                    path.strip_prefix(root).unwrap().to_owned(),
                    (
                        bytes,
                        Stamp {
                            len: meta.len(),
                            modified: meta.modified().ok(),
                            created: meta.created().ok(),
                            readonly: meta.permissions().readonly(),
                            #[cfg(unix)]
                            unix,
                        },
                    ),
                )
            })
            .collect()
    }
    fn wal_fixture(root: &Path, no_shm: bool) -> Connection {
        let writer = Connection::open(root.join("catalog.db")).unwrap();
        writer
            .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
            .unwrap();
        if no_shm {
            writer
                .execute_batch("PRAGMA locking_mode=EXCLUSIVE")
                .unwrap();
        }
        writer
            .execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
             CREATE TABLE paths(path TEXT); PRAGMA wal_checkpoint(TRUNCATE);
             INSERT INTO paths VALUES('/wal-only-commit');",
            )
            .unwrap();
        writer
    }
    fn read_path(reader: &Connection) -> String {
        reader
            .query_row("SELECT path FROM paths LIMIT 1", [], |row| row.get(0))
            .unwrap()
    }
    fn assert_wal_unchanged(no_shm: bool, checkpointed: bool) {
        let root = tempfile::tempdir().unwrap();
        let writer = wal_fixture(root.path(), no_shm);
        if checkpointed {
            writer.execute_batch("PRAGMA wal_checkpoint(FULL)").unwrap();
        }
        drop(writer);
        assert!(root.path().join("catalog.db-wal").is_file());
        assert_eq!(root.path().join("catalog.db-shm").exists(), !no_shm);
        let before = snapshot(root.path());
        let reader = open_readonly(&root.path().join("catalog.db")).unwrap();
        assert!(reader.is_readonly("main").unwrap());
        assert!(
            reader
                .db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE)
                .unwrap()
        );
        let timeout: i64 = reader
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert_eq!(timeout, 200);
        assert_eq!(read_path(&reader), "/wal-only-commit");
        assert!(
            reader
                .execute("INSERT INTO paths VALUES('/must-not-write')", [])
                .is_err()
        );
        drop(reader);
        assert_eq!(snapshot(root.path()), before);
    }
    #[test]
    fn wal_commit_without_shm_has_no_source_mutations() {
        assert_wal_unchanged(true, false);
    }
    #[test]
    fn wal_commit_with_shm_has_no_source_mutations() {
        assert_wal_unchanged(false, false);
    }
    #[test]
    fn closing_checkpointed_wal_does_not_clean_up_aux() {
        assert_wal_unchanged(false, true);
    }
    #[test]
    fn missing_database_and_write_open_are_denied_without_mutations() {
        let root = tempfile::tempdir().unwrap();
        let before = snapshot(root.path());
        assert!(open_readonly(&root.path().join("missing.db")).is_err());
        assert!(
            Connection::open_with_flags_and_vfs(
                root.path().join("missing.db"),
                OpenFlags::default(),
                NAME,
            )
            .is_err()
        );
        assert_eq!(snapshot(root.path()), before);
    }
    #[test]
    fn io_mutation_controls_shm_and_fetch_are_denied() {
        let root = tempfile::tempdir().unwrap();
        drop(wal_fixture(root.path(), true));
        let reader = open_readonly(&root.path().join("catalog.db")).unwrap();
        let before = snapshot(root.path());
        unsafe {
            let mut file: *mut ffi::sqlite3_file = ptr::null_mut();
            assert_eq!(
                ffi::sqlite3_file_control(
                    reader.handle(),
                    c"main".as_ptr(),
                    ffi::SQLITE_FCNTL_FILE_POINTER,
                    (&mut file as *mut *mut ffi::sqlite3_file).cast()
                ),
                ffi::SQLITE_OK
            );
            assert!(!file.is_null());
            let io = &*(*file).pMethods;
            assert_eq!(io.iVersion, 1);
            assert!(io.xShmMap.is_none() && io.xShmLock.is_none() && io.xShmUnmap.is_none());
            assert!(io.xFetch.is_none() && io.xUnfetch.is_none());
            assert_eq!(
                io.xWrite.unwrap()(file, c"x".as_ptr().cast(), 1, 0),
                ffi::SQLITE_READONLY
            );
            assert_eq!(io.xTruncate.unwrap()(file, 0), ffi::SQLITE_READONLY);
            assert_eq!(
                io.xSync.unwrap()(file, ffi::SQLITE_SYNC_NORMAL),
                ffi::SQLITE_READONLY
            );
            // Passing NULL also proves these opcodes are not delegated.
            for op in [
                ffi::SQLITE_FCNTL_SIZE_HINT,
                ffi::SQLITE_FCNTL_CHUNK_SIZE,
                ffi::SQLITE_FCNTL_MMAP_SIZE,
                ffi::SQLITE_FCNTL_PRAGMA,
                ffi::SQLITE_FCNTL_FILE_POINTER,
                ffi::SQLITE_FCNTL_JOURNAL_POINTER,
                ffi::SQLITE_FCNTL_BEGIN_ATOMIC_WRITE,
                ffi::SQLITE_FCNTL_COMMIT_ATOMIC_WRITE,
                ffi::SQLITE_FCNTL_LOCK_TIMEOUT,
                ffi::SQLITE_FCNTL_WIN32_SET_HANDLE,
                99999,
            ] {
                assert_eq!(
                    io.xFileControl.unwrap()(file, op, ptr::null_mut()),
                    ffi::SQLITE_NOTFOUND
                );
            }
            let vfs = ffi::sqlite3_vfs_find(c"ai-cube-index-readonly-v1".as_ptr());
            assert_eq!(
                (*vfs).xDelete.unwrap()(vfs, c"unused".as_ptr(), 1),
                ffi::SQLITE_READONLY
            );
        }
        assert_eq!(read_path(&reader), "/wal-only-commit");
        drop(reader);
        assert_eq!(snapshot(root.path()), before);
    }
    #[cfg(all(target_pointer_width = "64", any(target_os = "macos", target_os = "linux")))]
    #[test]
    fn live_connection_reads_bounded_without_mutations() {
        let root = tempfile::tempdir().unwrap();
        let owner = wal_fixture(root.path(), false);
        let before = snapshot_with(root.path(), live_wal::proof_read);
        let start = Instant::now();
        let reader = open_readonly(&root.path().join("catalog.db")).unwrap();
        assert_eq!(read_path(&reader), "/wal-only-commit");
        drop(reader);
        assert!(start.elapsed() < Duration::from_secs(2));
        println!(
            "live-owner contention: {:?} (busy_timeout=200 ms)",
            start.elapsed()
        );
        assert_eq!(snapshot_with(root.path(), live_wal::proof_read), before);
        owner
            .execute("INSERT INTO paths VALUES('/owner-still-works')", [])
            .unwrap();
        drop(owner);
        let reader = open_readonly(&root.path().join("catalog.db")).unwrap();
        assert_eq!(
            reader
                .query_row("SELECT count(*) FROM paths", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
    #[cfg(unix)]
    #[test]
    fn rw_unavailable_fails_without_readonly_fallback_or_mutations() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        drop(wal_fixture(root.path(), true));
        let db = root.path().join("catalog.db");
        fs::set_permissions(&db, fs::Permissions::from_mode(0o444)).unwrap();
        let before = snapshot(root.path());
        assert!(open_readonly(&db).is_err());
        assert_eq!(snapshot(root.path()), before);
    }
    #[test]
    fn closed_wal_without_aux_remains_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("catalog.db");
        let writer = Connection::open(&db).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE paths(path TEXT); INSERT INTO paths VALUES('/checkpointed');").unwrap();
        drop(writer);
        assert!(!root.path().join("catalog.db-wal").exists());
        let before = snapshot(root.path());
        let reader = open_readonly(&db).unwrap();
        assert_eq!(read_path(&reader), "/checkpointed");
        drop(reader);
        assert_eq!(snapshot(root.path()), before);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_wal_is_not_treated_as_absent() {
        use std::{
            io::{Read, Seek, SeekFrom},
            os::unix::fs::PermissionsExt,
        };
        let root = tempfile::tempdir().unwrap();
        drop(wal_fixture(root.path(), true));
        let wal = root.path().join("catalog.db-wal");
        // Preopen only our own fixture so its bytes can still be compared
        // after mode 000 denies fresh opens. No chmod/restore during the read.
        let mut handle = fs::File::open(&wal).unwrap();
        fs::set_permissions(&wal, fs::Permissions::from_mode(0o000)).unwrap();
        let mut capture = || {
            snapshot_with(root.path(), |path| {
                if path != wal {
                    return fs::read(path).unwrap();
                }
                handle.seek(SeekFrom::Start(0)).unwrap();
                let mut bytes = Vec::new();
                handle.read_to_end(&mut bytes).unwrap();
                bytes
            })
        };
        let before = capture();
        assert!(open_readonly(&root.path().join("catalog.db")).is_err());
        assert_eq!(capture(), before);
    }
    #[cfg(unix)]
    #[test]
    fn readonly_wal_can_be_read_without_permission_changes() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        drop(wal_fixture(root.path(), true));
        fs::set_permissions(
            root.path().join("catalog.db-wal"),
            fs::Permissions::from_mode(0o444),
        )
        .unwrap();
        let before = snapshot(root.path());
        let reader = open_readonly(&root.path().join("catalog.db")).unwrap();
        assert_eq!(read_path(&reader), "/wal-only-commit");
        drop(reader);
        assert_eq!(snapshot(root.path()), before);
    }
    #[cfg(all(target_pointer_width = "64", any(target_os = "macos", target_os = "linux")))]
    #[test]
    fn foreign_process_contention_preserves_source_and_owner() {
        use std::io::{BufRead, BufReader, Write};
        use std::process::{Command, Stdio};
        let root = tempfile::tempdir().unwrap();
        drop(wal_fixture(root.path(), false));
        let test_name = format!(
            "{}::foreign_lock_owner",
            module_path!().split_once("::").unwrap().1
        );
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &test_name, "--ignored", "--nocapture"])
            .env("AI_CUBE_WAL_PROOF_OWNER", root.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut lines = BufReader::new(stdout).lines();
        while !lines
            .next()
            .expect("owner exited before ready")
            .unwrap()
            .contains("WAL_OWNER_READY")
        {}
        let before = snapshot(root.path());
        let start = Instant::now();
        let reader = open_readonly(&root.path().join("catalog.db")).unwrap();
        assert_eq!(read_path(&reader), "/wal-only-commit");
        drop(reader);
        assert!(start.elapsed() < Duration::from_secs(2));
        println!(
            "foreign-owner contention: {:?} (busy_timeout=200 ms)",
            start.elapsed()
        );
        // Always release the child even if the comparison below fails.
        let after = snapshot(root.path());
        child.stdin.take().unwrap().write_all(b"release\n").unwrap();
        for line in lines {
            println!("{}", line.unwrap());
        }
        assert!(child.wait().unwrap().success());
        assert_eq!(after, before);
        let reader = open_readonly(&root.path().join("catalog.db")).unwrap();
        assert_eq!(
            reader
                .query_row("SELECT count(*) FROM paths", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
    #[test]
    #[ignore = "helper process for foreign_process_contention_preserves_source_and_owner"]
    fn foreign_lock_owner() {
        use std::io::Write;
        let Some(root) = std::env::var_os("AI_CUBE_WAL_PROOF_OWNER") else {
            return;
        };
        let owner = Connection::open(Path::new(&root).join("catalog.db")).unwrap();
        owner
            .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
            .unwrap();
        assert_eq!(read_path(&owner), "/wal-only-commit");
        println!("WAL_OWNER_READY");
        std::io::stdout().flush().unwrap();
        let mut release = String::new();
        std::io::stdin().read_line(&mut release).unwrap();
        assert_eq!(release, "release\n");
        owner
            .execute("INSERT INTO paths VALUES('/foreign-owner-still-works')", [])
            .unwrap();
    }

    #[test]
    fn connection_value_and_sql_limits_are_local_and_enforced() {
        let root = tempfile::tempdir().unwrap();
        drop(wal_fixture(root.path(), true));
        let before = snapshot(root.path());
        let reader = open_readonly(&root.path().join("catalog.db")).unwrap();
        unsafe {
            assert_eq!(
                ffi::sqlite3_limit(reader.handle(), ffi::SQLITE_LIMIT_LENGTH, -1),
                MAX_VALUE_BYTES
            );
            assert_eq!(
                ffi::sqlite3_limit(reader.handle(), ffi::SQLITE_LIMIT_SQL_LENGTH, -1),
                MAX_SQL_BYTES
            );
        }
        let oversized_sql = format!("SELECT 1 /*{}*/", "x".repeat(MAX_SQL_BYTES as usize));
        assert!(reader.prepare(&oversized_sql).is_err());
        // Synthetic value only; no table/body is read by this statement.
        assert!(
            reader
                .query_row("SELECT zeroblob(?1)", [MAX_VALUE_BYTES + 1], |row| row
                    .get::<_, Vec<u8>>(0))
                .is_err()
        );
        assert_eq!(read_path(&reader), "/wal-only-commit");
        // No process-global limit has been changed.
        let unrelated = Connection::open_in_memory().unwrap();
        unsafe {
            assert!(
                ffi::sqlite3_limit(unrelated.handle(), ffi::SQLITE_LIMIT_LENGTH, -1)
                    > MAX_VALUE_BYTES
            );
        }
        drop(reader);
        assert_eq!(snapshot(root.path()), before);
    }
    #[test]
    fn oversized_wal_is_refused_before_recovery_without_source_mutation() {
        let root = tempfile::tempdir().unwrap();
        drop(wal_fixture(root.path(), true));
        let wal = root.path().join("catalog.db-wal");
        // Extend only this generated log. No real source copying or parsing.
        fs::OpenOptions::new()
            .write(true)
            .open(&wal)
            .unwrap()
            .set_len(MAX_WAL_BYTES as u64 + 1)
            .unwrap();
        let before = snapshot(root.path());
        let error = open_readonly(&root.path().join("catalog.db")).unwrap_err();
        assert_eq!(error.sqlite_error_code(), Some(ffi::ErrorCode::TooBig));
        assert_eq!(snapshot(root.path()), before);
    }
    #[test]
    fn oversized_schema_sql_and_row_fail_during_initial_access_unchanged() {
        for bytes in [MAX_SQL_BYTES as usize + 1, MAX_VALUE_BYTES as usize + 1] {
            let root = tempfile::tempdir().unwrap();
            let db = root.path().join("catalog.db");
            let writer = Connection::open(&db).unwrap();
            writer
                .execute_batch(&format!(
                    "CREATE VIEW huge AS /*{}*/ SELECT 1;",
                    "x".repeat(bytes)
                ))
                .unwrap();
            assert!(
                writer
                    .query_row(
                        "SELECT length(sql) FROM sqlite_schema WHERE name='huge'",
                        [],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap()
                    > bytes as i64
            );
            drop(writer);
            let before = snapshot(root.path());
            assert!(open_readonly(&db).is_err());
            assert_eq!(snapshot(root.path()), before);
        }
    }
    #[test]
    fn many_small_schema_objects_hit_initial_progress_budget_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("catalog.db");
        let writer = Connection::open(&db).unwrap();
        writer.execute_batch("BEGIN").unwrap();
        for i in 0..30_000 {
            writer
                .execute_batch(&format!("CREATE VIEW v{i} AS SELECT 1"))
                .unwrap();
        }
        writer.execute_batch("COMMIT").unwrap();
        drop(writer);
        let before = snapshot(root.path());
        let start = Instant::now();
        let error = open_readonly(&db).unwrap_err();
        assert_eq!(
            error.sqlite_error_code(),
            Some(ffi::ErrorCode::OperationInterrupted)
        );
        println!(
            "30,000-object schema: interrupted after {:?}",
            start.elapsed()
        );
        assert_eq!(snapshot(root.path()), before);
    }
    #[test]
    fn per_file_and_shared_source_budgets_refuse_reads_before_native_io() {
        let root = tempfile::tempdir().unwrap();
        drop(wal_fixture(root.path(), true));
        let before = snapshot(root.path());
        let reader = open_readonly(&root.path().join("catalog.db")).unwrap();
        unsafe {
            let mut main: *mut ffi::sqlite3_file = ptr::null_mut();
            let mut wal: *mut ffi::sqlite3_file = ptr::null_mut();
            for (op, out) in [
                (ffi::SQLITE_FCNTL_FILE_POINTER, &mut main),
                (ffi::SQLITE_FCNTL_JOURNAL_POINTER, &mut wal),
            ] {
                assert_eq!(
                    ffi::sqlite3_file_control(
                        reader.handle(),
                        c"main".as_ptr(),
                        op,
                        (out as *mut *mut ffi::sqlite3_file).cast()
                    ),
                    ffi::SQLITE_OK
                );
            }
            assert!(!main.is_null() && !wal.is_null());
            assert!(ptr::eq((*main).pMethods, &METHODS) && ptr::eq((*wal).pMethods, &METHODS));
            let main = main.cast::<File>();
            let wal = wal.cast::<File>();
            assert_eq!((*main).total_left, (*wal).total_left);
            let total = &*(*main).total_left;
            let initial_total = total.load(Ordering::Relaxed);
            assert!(initial_total < MAX_SOURCE_READ_BYTES);
            let mut buf = [0; 8];
            assert_eq!(
                read(main.cast(), buf.as_mut_ptr().cast(), 8, 0),
                ffi::SQLITE_OK
            );
            assert_eq!(&buf, b"SQLite f");
            assert_eq!(total.load(Ordering::Relaxed), initial_total - 8);
            let file_left = (*main).read_left;
            (*main).read_left = 4;
            buf.fill(0x5a);
            assert_eq!(
                read(main.cast(), buf.as_mut_ptr().cast(), 8, 0),
                ffi::SQLITE_IOERR_READ
            );
            assert_eq!(buf, [0x5a; 8]); // Native read did not run.
            assert_eq!(total.load(Ordering::Relaxed), initial_total - 8);
            (*main).read_left = file_left;
            total.store(4, Ordering::Relaxed);
            for file in [main, wal] {
                assert_eq!(
                    read(file.cast(), buf.as_mut_ptr().cast(), 8, 0),
                    ffi::SQLITE_IOERR_READ
                );
                assert_eq!(buf, [0x5a; 8]);
                assert_eq!(total.load(Ordering::Relaxed), 4); // No underflow.
            }
            // The live size/offset checks must also remain active after open.
            assert_eq!(
                read(wal.cast(), buf.as_mut_ptr().cast(), 8, MAX_WAL_BYTES),
                ffi::SQLITE_TOOBIG
            );
        }
        drop(reader);
        assert_eq!(snapshot(root.path()), before);
    }
}
