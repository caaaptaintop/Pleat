//! Bounded ordinary configuration reads; author: admin. No provider policy.
use std::{fs, path::Path};

// Raw I/O shared with Codex; each provider keeps its own missing-file, format,
// identity and diagnostic policy. Never open a replacement FIFO in blocking mode.
pub(crate) const REGULAR_CONFIG_LIMIT: u64 = 1024 * 1024;

pub(crate) enum RegularFileReadError { NotRegular, TooLarge, Unavailable }

pub(crate) struct RegularFileBytes {
    pub(crate) bytes: Vec<u8>,
    pub(crate) opened: fs::Metadata,
    pub(crate) finished: fs::Metadata,
}

pub(crate) fn read_regular_file_bytes(
    path: &Path,
    before_open: impl FnOnce(),
) -> Result<RegularFileBytes, RegularFileReadError> {
    read_regular_file_bytes_with_limit(path, REGULAR_CONFIG_LIMIT, before_open)
}

pub(crate) fn read_regular_file_bytes_with_limit(
    path: &Path,
    limit: u64,
    before_open: impl FnOnce(),
) -> Result<RegularFileBytes, RegularFileReadError> {
    before_open();
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = options.open(path).map_err(|_| RegularFileReadError::Unavailable)?;
    let opened = file.metadata().map_err(|_| RegularFileReadError::Unavailable)?;
    if !opened.is_file() { return Err(RegularFileReadError::NotRegular); }
    if opened.len() > limit { return Err(RegularFileReadError::TooLarge); }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(&mut file, limit.saturating_add(1)), &mut bytes)
        .map_err(|_| RegularFileReadError::Unavailable)?;
    if bytes.len() as u64 > limit { return Err(RegularFileReadError::TooLarge); }
    let finished = file.metadata().map_err(|_| RegularFileReadError::Unavailable)?;
    Ok(RegularFileBytes { bytes, opened, finished })
}

pub(crate) fn same_regular_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    if !a.is_file() || !b.is_file() || a.len() != b.len() { return false; }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev() && a.ino() == b.ino() && a.mtime() == b.mtime()
            && a.mtime_nsec() == b.mtime_nsec() && a.ctime() == b.ctime() && a.ctime_nsec() == b.ctime_nsec()
    }
    #[cfg(not(unix))]
    { matches!((a.modified(), b.modified()), (Ok(a), Ok(b)) if a == b) }
}

