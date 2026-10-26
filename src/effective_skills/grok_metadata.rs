//! Read only the optional header needed for Grok invocation flags. Native
//! `inspect` supplies the catalog; this does not rediscover or read skill bodies.
use std::{fs::{self, Metadata, OpenOptions}, io::Read, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Flags {
    pub user_invocable: bool,
    pub disable_model_invocation: bool,
}

impl Default for Flags {
    fn default() -> Self { Self { user_invocable: true, disable_model_invocation: false } }
}

const LIMIT: usize = 64 * 1024;

fn same_file(a: &Metadata, b: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.is_file() && b.is_file() && a.dev() == b.dev() && a.ino() == b.ino()
            && a.len() == b.len() && a.mtime() == b.mtime() && a.mtime_nsec() == b.mtime_nsec()
            && a.ctime() == b.ctime() && a.ctime_nsec() == b.ctime_nsec()
    }
    #[cfg(not(unix))]
    { let _ = (a, b); false }
}

pub(super) fn read(path: &Path) -> Option<Flags> {
    let before = fs::metadata(path).ok()?;
    if !before.is_file() { return None; }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    // Managed source links are supported; validate the opened regular file and
    // mapping at observed checkpoints, rather than claiming an atomic snapshot.
    let mut file = options.open(path).ok()?;
    if !same_file(&before, &file.metadata().ok()?) { return None; }
    let flags = parse(&mut file)?;
    let after = file.metadata().ok()?;
    let current = fs::metadata(path).ok()?;
    (same_file(&before, &after) && same_file(&after, &current)).then_some(flags)
}

fn parse(reader: &mut impl Read) -> Option<Flags> {
    let mut header = Vec::new();
    // Stop immediately when the prefix cannot be a YAML delimiter. A body-only
    // command never requires reading its first line or the rest of its body.
    let mut byte = [0];
    for expected in b"---" {
        match reader.read(&mut byte).ok()? {
            0 => return Some(Flags::default()),
            _ if byte[0] != *expected => return Some(Flags::default()),
            _ => header.push(byte[0]),
        }
    }
    if reader.read(&mut byte).ok()? == 0 { return Some(Flags::default()); }
    header.push(byte[0]);
    match byte[0] {
        b'\n' => {}
        b'\r' => {
            if reader.read(&mut byte).ok()? != 1 || byte[0] != b'\n' { return Some(Flags::default()); }
            header.push(byte[0]);
        }
        _ => return Some(Flags::default()),
    }
    let frontmatter_start = header.len();
    let mut line_start = frontmatter_start;
    let mut closed = false;
    while header.len() < LIMIT {
        if reader.read(&mut byte).ok()? == 0 {
            closed = &header[line_start..] == b"---";
            break;
        }
        header.push(byte[0]);
        if byte[0] != b'\n' { continue; }
        if matches!(&header[line_start..], b"---\n" | b"---\r\n") { closed = true; break; }
        line_start = header.len();
    }
    if !closed { return None; }
    let frontmatter = std::str::from_utf8(&header[frontmatter_start..line_start]).ok()?;
    let fields = match serde_yaml::from_str(frontmatter).ok()? {
        serde_yaml::Value::Mapping(fields) => fields,
        serde_yaml::Value::Null => serde_yaml::Mapping::new(),
        _ => return None,
    };
    let flag = |name: &str, default| {
        fields.get(serde_yaml::Value::String(name.into()))
            .map_or(Some(default), serde_yaml::Value::as_bool)
    };
    Some(Flags {
        user_invocable: flag("user-invocable", true)?,
        disable_model_invocation: flag("disable-model-invocation", false)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn flags_do_not_read_body_and_omitted_values_keep_native_defaults() {
        let bytes = b"---\nname: demo\ndescription: fixture\nuser-invocable: false\n---\nPRIVATE BODY MUST NOT BE READ";
        let mut cursor = Cursor::new(bytes);
        assert_eq!(parse(&mut cursor), Some(Flags { user_invocable: false, disable_model_invocation: false }));
        assert_eq!(&bytes[cursor.position() as usize..], b"PRIVATE BODY MUST NOT BE READ");
        let mut body = Cursor::new(b"body-only command whose remainder must not be read");
        assert_eq!(parse(&mut body), Some(Flags::default()));
        assert_eq!(body.position(), 1);
        assert_eq!(parse(&mut Cursor::new(b"---\n---\nbody")), Some(Flags::default()));
        assert_eq!(parse(&mut Cursor::new(b"---\r\nuser-invocable: true\r\ndisable-model-invocation: true\r\n---")),
            Some(Flags { user_invocable: true, disable_model_invocation: true }));
    }

    #[test]
    fn malformed_or_oversized_headers_stay_unknown() {
        for text in ["---\nuser-invocable: 'false'\n---\n", "---\ndisable-model-invocation: [true]\n---\n", "---\nunterminated"] {
            assert_eq!(parse(&mut Cursor::new(text)), None);
        }
        let oversized = format!("---\nname: {}\n---\n", "x".repeat(LIMIT));
        assert_eq!(parse(&mut Cursor::new(oversized)), None);
    }

    #[cfg(unix)]
    #[test]
    fn managed_links_work_but_non_regular_paths_do_not_block() {
        use std::os::unix::{ffi::OsStrExt, fs::symlink};
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::write(&source, "---\nname: demo\n---\nbody").unwrap();
        let link = temp.path().join("link");
        symlink(&source, &link).unwrap();
        assert_eq!(read(&link), Some(Flags::default()));
        assert_eq!(read(temp.path()), None);
        let fifo = temp.path().join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert_eq!(read(&fifo), None);
    }
}
