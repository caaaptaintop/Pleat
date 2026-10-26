//! Read-only discovery from existing Copilot CLI 1.0.98 managed servers.
use std::path::{Path, PathBuf};

const UNKNOWN: &str = "无法核验 Copilot 实时原生技能来源";

struct VerifiedOrigin {
    config_root: PathBuf,
    user_roots: Vec<PathBuf>,
}

pub(super) fn user_roots(home: &Path, project: &Path, config_root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    origin(home, project, config_root).map(|value| value.user_roots)
}

/// A relative override is a search hint until the same native origin is verified.
pub(super) fn config_root(home: &Path, project: &Path, raw: &str) -> anyhow::Result<PathBuf> {
    let hint = relative_config_hint(project, raw)?;
    origin(home, project, &hint).map(|value| value.config_root)
}

pub(super) fn relative_config_hint(project: &Path, raw: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(project.is_absolute() && !raw.is_empty() && !raw.contains('\0')
        && !Path::new(raw).is_absolute(), "Copilot 配置目录无效");
    Ok(project.join(raw))
}

fn origin(home: &Path, project: &Path, config_root: &Path) -> anyhow::Result<VerifiedOrigin> {
    #[cfg(target_os = "macos")]
    let result = live::roots(home, project, config_root);
    #[cfg(not(target_os = "macos"))]
    let result = { let _ = (home, project, config_root); Err(()) };
    finish(result)
}
fn finish<T>(result: Result<T, ()>) -> anyhow::Result<T> {
    result.map_err(|_| anyhow::anyhow!(UNKNOWN))
}

#[cfg(any(target_os = "macos", all(test, unix)))]
mod live {
    use super::*;
    use serde::Deserialize;
    use serde_json::{Value, json};
    use std::{
        collections::BTreeSet,
        fs::{self, File, Metadata, OpenOptions},
        io::{Read, Write},
        net::{IpAddr, SocketAddr, TcpStream},
        os::unix::fs::{MetadataExt, OpenOptionsExt},
        path::Component,
        time::{Duration, Instant, SystemTime},
    };

    type Check<T> = Result<T, ()>;
    const ENTRY_LIMIT: u64 = 64 * 1024;
    const FRAME_LIMIT: usize = 128 * 1024;
    fn require(ok: bool) -> Check<()> { if ok { Ok(()) } else { Err(()) } }

    // Never derive Debug: this value contains the registry authentication secret.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Registration {
        schema_version: u32, kind: String, pid: i32, host: String, port: u16,
        token: String, session_id: String, started_at: String, copilot_version: String,
    }
    fn registration(bytes: &[u8]) -> Check<Registration> {
        require(bytes.len() <= ENTRY_LIMIT as usize)?;
        let r: Registration = serde_json::from_slice(bytes).map_err(|_| ())?;
        require(r.schema_version == 2 && r.kind == "managed-server" && r.pid > 1
            && matches!(r.host.as_str(), "127.0.0.1" | "::1") && r.port != 0
            && !r.token.is_empty() && !r.token.contains('\0') && !r.session_id.is_empty()
            && !r.started_at.is_empty() && !r.copilot_version.is_empty()
            && !r.copilot_version.contains('\0'))?;
        // The native publisher calls supportCliVersion() without the CLI fallback.
        // Only the authenticated connect result below pins the compatible version.
        Ok(r)
    }
    fn same(a: &Metadata, b: &Metadata) -> bool {
        a.dev() == b.dev() && a.ino() == b.ino() && a.mode() == b.mode()
            && a.uid() == b.uid() && a.nlink() == b.nlink() && a.len() == b.len()
            && a.mtime() == b.mtime() && a.mtime_nsec() == b.mtime_nsec()
            && a.ctime() == b.ctime() && a.ctime_nsec() == b.ctime_nsec()
    }
    fn trusted_dir(path: &Path, uid: u32) -> Check<Metadata> {
        let m = fs::symlink_metadata(path).map_err(|_| ())?;
        require(m.is_dir() && m.uid() == uid && m.mode() & 0o022 == 0)?;
        Ok(m)
    }
    fn trusted_file(m: &Metadata, uid: u32, now: SystemTime) -> Check<()> {
        require(m.is_file() && m.uid() == uid && m.nlink() == 1
            && m.mode() & 0o077 == 0 && m.len() <= ENTRY_LIMIT)?;
        require(now.duration_since(m.modified().map_err(|_| ())?).map_err(|_| ())?
            <= Duration::from_secs(300))
    }
    fn read_registration(path: &Path, uid: u32) -> Check<(File, Metadata, Registration)> {
        let before = fs::symlink_metadata(path).map_err(|_| ())?;
        trusted_file(&before, uid, SystemTime::now())?;
        let mut f = OpenOptions::new().read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path).map_err(|_| ())?;
        require(same(&before, &f.metadata().map_err(|_| ())?))?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut f).take(ENTRY_LIMIT + 1).read_to_end(&mut bytes).map_err(|_| ())?;
        let r = registration(&bytes)?;
        require(same(&before, &f.metadata().map_err(|_| ())?)
            && same(&before, &fs::symlink_metadata(path).map_err(|_| ())?))?;
        Ok((f, before, r))
    }
    fn candidates(servers: &Path) -> Check<Vec<PathBuf>> {
        let mut paths = Vec::new();
        for (index, entry) in fs::read_dir(servers).map_err(|_| ())?.enumerate() {
            require(index < 16)?;
            let path = entry.map_err(|_| ())?.path();
            if path.extension() == Some(std::ffi::OsStr::new("json")) { paths.push(path); }
        }
        require(!paths.is_empty())?;
        paths.sort();
        Ok(paths)
    }

    #[derive(Clone, PartialEq, Eq)]
    struct Process { pid: i32, uid: u32, ruid: u32, secs: u64, micros: u64, exe: PathBuf, cwd: PathBuf }
    fn unchanged(before: &Process, after: &Process) -> Check<()> { require(before == after) }

    #[cfg(target_os = "macos")]
    fn snapshot(pid: i32, uid: u32) -> Check<Process> {
        // libc owns these ABI layouts. Only query the PID supplied by the registry.
        unsafe fn info<T>(pid: i32, flavor: i32) -> Check<T> {
            let mut data = std::mem::MaybeUninit::<T>::zeroed();
            let size = std::mem::size_of::<T>();
            let n = unsafe { libc::proc_pidinfo(pid, flavor, 0, data.as_mut_ptr().cast(), size as i32) };
            require(n == size as i32)?;
            Ok(unsafe { data.assume_init() })
        }
        fn path(bytes: impl Iterator<Item = libc::c_char>) -> Check<PathBuf> {
            use std::os::unix::ffi::OsStringExt;
            let bytes: Vec<u8> = bytes.map(|b| b as u8).collect();
            let end = bytes.iter().position(|b| *b == 0).ok_or(())?;
            let path = PathBuf::from(std::ffi::OsString::from_vec(bytes[..end].to_vec()));
            require(path.is_absolute())?;
            Ok(path)
        }
        let b: libc::proc_bsdinfo = unsafe { info(pid, libc::PROC_PIDTBSDINFO)? };
        require(b.pbi_pid == pid as u32 && b.pbi_uid == uid && b.pbi_ruid == uid
            && b.pbi_start_tvsec != 0 && b.pbi_start_tvusec < 1_000_000)?;
        let v: libc::proc_vnodepathinfo = unsafe { info(pid, libc::PROC_PIDVNODEPATHINFO)? };
        let cwd = path(v.pvi_cdir.vip_path.into_iter().flatten())?.canonicalize().map_err(|_| ())?;
        let mut exe = [0 as libc::c_char; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let n = unsafe { libc::proc_pidpath(pid, exe.as_mut_ptr().cast(), exe.len() as u32) };
        require(n > 0 && (n as usize) < exe.len())?;
        let exe = path(exe.into_iter())?;
        require(exe.file_name() == Some(std::ffi::OsStr::new("copilot")))?;
        Ok(Process { pid, uid, ruid: b.pbi_ruid, secs: b.pbi_start_tvsec,
            micros: b.pbi_start_tvusec, exe, cwd })
    }

    fn timeout(deadline: Instant) -> Check<Duration> {
        let remaining = deadline.checked_duration_since(Instant::now()).ok_or(())?;
        require(!remaining.is_zero())?;
        Ok(remaining.min(Duration::from_millis(120)))
    }
    fn secret(bytes: &[u8], token: &str) -> bool {
        // UTF-8 is mandatory on this wire. str::contains also avoids quadratic byte matching.
        token.is_empty() || std::str::from_utf8(bytes).map_or(true, |s| s.contains(token))
    }
    fn secret_value(v: &Value, token: &str) -> bool {
        match v {
            Value::String(s) => s.contains(token),
            Value::Array(a) => a.iter().any(|v| secret_value(v, token)),
            Value::Object(o) => o.iter().any(|(k, v)| k.contains(token) || secret_value(v, token)),
            _ => false,
        }
    }
    // The injected byte reader is only a framing primitive; tests use in-memory input.
    fn frame(mut read: impl FnMut(&mut [u8]) -> Check<usize>, token: &str) -> Check<Value> {
        fn fill(read: &mut impl FnMut(&mut [u8]) -> Check<usize>, mut bytes: &mut [u8]) -> Check<()> {
            while !bytes.is_empty() {
                let n = read(bytes)?;
                require(n > 0 && n <= bytes.len())?;
                bytes = &mut bytes[n..];
            }
            Ok(())
        }
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            require(header.len() < 8192)?;
            let mut byte = [0];
            fill(&mut read, &mut byte)?;
            header.push(byte[0]);
        }
        require(!secret(&header, token) && header.is_ascii())?;
        let text = std::str::from_utf8(&header).map_err(|_| ())?;
        let mut length = None;
        for line in text[..text.len() - 4].split("\r\n") {
            let (key, value) = line.split_once(':').ok_or(())?;
            require(!key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))?;
            if key.eq_ignore_ascii_case("Content-Length") {
                let value = value.trim();
                require(length.is_none() && !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))?;
                length = Some(value.parse::<usize>().map_err(|_| ())?);
            }
        }
        let length = length.ok_or(())?;
        require(length <= FRAME_LIMIT)?;
        let mut body = vec![0; length];
        fill(&mut read, &mut body)?;
        require(!secret(&body, token))?;
        let value = serde_json::from_slice(&body).map_err(|_| ())?;
        require(!secret_value(&value, token))?;
        Ok(value)
    }
    fn response(value: Value, id: u32) -> Check<Option<Value>> {
        let o = value.as_object().ok_or(())?;
        require(o.get("jsonrpc").and_then(Value::as_str) == Some("2.0"))?;
        if let Some(method) = o.get("method") {
            require(method.is_string() && !o.contains_key("id")
                && !o.contains_key("result") && !o.contains_key("error"))?;
            return Ok(None);
        }
        require(o.get("id").and_then(Value::as_u64) == Some(id as u64) && !o.contains_key("error"))?;
        Ok(Some(o.get("result").ok_or(())?.clone()))
    }
    fn reply(mut read: impl FnMut(&mut [u8]) -> Check<usize>, token: &str, id: u32, frames: &mut u8) -> Check<Value> {
        loop {
            require(*frames < 32)?;
            *frames += 1;
            if let Some(result) = response(frame(&mut read, token)?, id)? { return Ok(result); }
        }
    }
    fn rpc(stream: &mut TcpStream, deadline: Instant, frames: &mut u8,
        token: &str, id: u32, method: &str, params: Value) -> Check<Value> {
        let body = serde_json::to_vec(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))
            .map_err(|_| ())?;
        let mut wire = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        wire.extend(body);
        let mut pending = wire.as_slice();
        while !pending.is_empty() {
            stream.set_write_timeout(Some(timeout(deadline)?)).map_err(|_| ())?;
            let n = stream.write(pending).map_err(|_| ())?;
            require(n > 0)?;
            pending = &pending[n..];
        }
        reply(|bytes| {
            stream.set_read_timeout(Some(timeout(deadline)?)).map_err(|_| ())?;
            stream.read(bytes).map_err(|_| ())
        }, token, id, frames)
    }
    fn handshake(value: &Value) -> Check<()> {
        require(value.get("ok").and_then(Value::as_bool) == Some(true)
            && value.get("protocolVersion").and_then(Value::as_u64) == Some(3)
            && value.get("version").and_then(Value::as_str) == Some("1.0.98"))
    }
    fn normalize(path: &Path) -> Check<PathBuf> {
        require(path.is_absolute())?;
        let mut result = PathBuf::new();
        for c in path.components() {
            match c { Component::CurDir => (), Component::ParentDir => { require(result.pop())?; },
                _ => result.push(c.as_os_str()) }
        }
        Ok(result)
    }
    fn secret_path(path: &Path, token: &str) -> bool {
        token.is_empty() || path.as_os_str().as_encoded_bytes().windows(token.len()).any(|w| w == token.as_bytes())
    }
    fn discovery(value: &Value, home: &Path, config: &Path, process: &Process, token: &str) -> Check<BTreeSet<PathBuf>> {
        require(!secret_path(&process.cwd, token) && !secret_path(&process.exe, token))?;
        let paths = value.get("paths").and_then(Value::as_array).ok_or(())?;
        let mut roots = BTreeSet::new();
        let mut personal = false;
        for entry in paths {
            let raw = entry.get("path").and_then(Value::as_str).ok_or(())?;
            require(!raw.is_empty() && !raw.contains('\0') && !raw.contains(token)
                && entry.get("preferredForCreation").and_then(Value::as_bool).is_some())?;
            let root = normalize(&process.cwd.join(raw))?;
            // Check decoded and resolved paths too; JSON escaping must not hide a secret.
            require(!secret_path(&root, token))?;
            match entry.get("scope").and_then(Value::as_str).ok_or(())? {
                "personal-copilot" => { require(root == normalize(&config.join("skills"))?)?; personal = true; },
                "personal-agents" => require(root == normalize(&home.join(".agents/skills"))?)?,
                "custom" => (), _ => return Err(()),
            }
            roots.insert(root);
        }
        require(personal)?;
        Ok(roots)
    }
    fn agree(current: &mut Option<BTreeSet<PathBuf>>, roots: BTreeSet<PathBuf>) -> Check<()> {
        if let Some(previous) = current { require(*previous == roots) } else { *current = Some(roots); Ok(()) }
    }
    fn matches_project(before: &Process, after: &Process, project: &Path) -> Check<bool> {
        unchanged(before, after)?;
        Ok(before.cwd == project)
    }

    fn verified_exchange<T>(before: &Process, peer: impl FnOnce() -> Check<()>,
        after: impl FnOnce() -> Check<Process>, send: impl FnOnce() -> Check<T>) -> Check<T> {
        peer()?;
        unchanged(before, &after()?)?;
        send()
    }

    #[cfg(target_os = "macos")]
    fn connected_peer(pid: i32, fd: i32, deadline: Instant) -> Check<()> {
        unsafe extern "C" { fn pleat_copilot_peer_owned(pid: i32, client_fd: i32) -> i32; }
        let deadline = deadline.min(Instant::now() + Duration::from_millis(120));
        loop {
            timeout(deadline)?;
            // The SDK C bridge checks this established connection's reversed
            // four-tuple against an accepted socket in this PID, not a listener.
            match unsafe { pleat_copilot_peer_owned(pid, fd) } {
                1 => return Ok(()),
                0 => std::thread::sleep(timeout(deadline)?.min(Duration::from_millis(3))),
                _ => return Err(()),
            }
        }
    }

    #[cfg(target_os = "macos")]
    pub(super) fn roots(home: &Path, project: &Path, config_root: &Path) -> Check<VerifiedOrigin> {
        // Shared across all candidates; filesystem and OS calls remain synchronous.
        let deadline = Instant::now() + Duration::from_millis(1800);
        require(home.is_absolute() && project.is_absolute() && config_root.is_absolute())?;
        let uid = unsafe { libc::getuid() };
        require(uid == unsafe { libc::geteuid() })?;
        let project = project.canonicalize().map_err(|_| ())?;
        let config = config_root.canonicalize().map_err(|_| ())?;
        let servers = config.join("servers");
        let config_meta = trusted_dir(&config, uid)?;
        let servers_meta = trusted_dir(&servers, uid)?;
        let mut result = None;
        for path in candidates(&servers)? {
            timeout(deadline)?;
            let (file, meta, r) = read_registration(&path, uid)?;
            let before = snapshot(r.pid, uid)?;
            require(!secret_path(&path, &r.token) && !secret_path(&before.cwd, &r.token)
                && !secret_path(&before.exe, &r.token))?;
            // A registry entry cannot predate this incarnation of its PID.
            let start = SystemTime::UNIX_EPOCH.checked_add(Duration::new(before.secs, before.micros as u32 * 1000)).ok_or(())?;
            require(start <= meta.modified().map_err(|_| ())?)?;
            if !matches_project(&before, &snapshot(r.pid, uid)?, &project)? {
                require(same(&meta, &file.metadata().map_err(|_| ())?)
                    && same(&meta, &fs::symlink_metadata(&path).map_err(|_| ())?))?;
                continue;
            }
            let host: IpAddr = r.host.parse().map_err(|_| ())?;
            let mut stream = TcpStream::connect_timeout(&SocketAddr::new(host, r.port), timeout(deadline)?)
                .map_err(|_| ())?;
            let mut frames = 0;
            use std::os::fd::AsRawFd;
            let fd = stream.as_raw_fd();
            let value = verified_exchange(&before,
                || connected_peer(r.pid, fd, deadline),
                || snapshot(r.pid, uid),
                || {
                    handshake(&rpc(&mut stream, deadline, &mut frames, &r.token,
                        1, "connect", json!({"token":r.token}))?)?;
                    rpc(&mut stream, deadline, &mut frames, &r.token,
                        2, "skills.getDiscoveryPaths", json!({}))
                })?;
            drop(stream); // Existing user process: disconnect only, never shutdown/spawn/signal.
            let roots = discovery(&value, home, config_root, &before, &r.token)?;
            require(same(&meta, &file.metadata().map_err(|_| ())?)
                && same(&meta, &fs::symlink_metadata(&path).map_err(|_| ())?))?;
            trusted_file(&meta, uid, SystemTime::now())?;
            unchanged(&before, &snapshot(r.pid, uid)?)?;
            agree(&mut result, roots)?;
        }
        require(config_root.canonicalize().map_err(|_| ())? == config
            && same(&config_meta, &trusted_dir(&config, uid)?)
            && same(&servers_meta, &trusted_dir(&servers, uid)?))?;
        timeout(deadline)?;
        Ok(VerifiedOrigin { config_root: config, user_roots: result.ok_or(())?.into_iter().collect() })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{io::Cursor, os::unix::fs::{PermissionsExt, symlink}};
        const TOKEN: &str = "fake-secret-never-print";
        fn fixture() -> Value { json!({"schemaVersion":2,"kind":"managed-server","pid":42,"host":"127.0.0.1",
            "port":12345,"token":TOKEN,"sessionId":"fake","startedAt":"2026-10-24T00:00:00Z","copilotVersion":"1.0.98"}) }
        fn process() -> Process { Process { pid:42, uid:501, ruid:501, secs:100, micros:2,
            exe:"/fake/copilot".into(), cwd:"/native/project".into() } }
        fn wire(body: &[u8]) -> Vec<u8> {
            let mut w = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes(); w.extend(body); w
        }
        fn parse(bytes: Vec<u8>) -> Check<Value> {
            let mut input = Cursor::new(bytes); frame(|out| input.read(out).map_err(|_| ()), TOKEN)
        }
        #[test]
        fn schema_is_exact_and_bounded() {
            let base = fixture();
            assert!(registration(&serde_json::to_vec(&base).unwrap()).is_ok());
            for (key, value) in [("schemaVersion",json!(1)),("pid",json!("42")),("pid",json!(-1)),
                ("host",json!("localhost")),("host",json!("127.0.0.2")),("port",json!(0)),("port",json!(65536)),
                ("token",json!(null)),("startedAt",json!(0)),("copilotVersion",json!("")),
                ("copilotVersion",json!("bad\0version"))] {
                let mut v = base.clone(); v[key] = value;
                assert!(registration(&serde_json::to_vec(&v).unwrap()).is_err());
            }
            let mut v = base; v["host"] = json!("::1"); v["unknown"] = json!(true);
            assert!(registration(&serde_json::to_vec(&v).unwrap()).is_ok());
            assert!(registration(&vec![b' '; ENTRY_LIMIT as usize + 1]).is_err());
        }
        #[test]
        fn registry_version_does_not_replace_authenticated_cli_version() {
            let mut entry = fixture();
            entry["copilotVersion"] = json!("unknown");
            assert!(registration(&serde_json::to_vec(&entry).unwrap()).is_ok());
            assert!(handshake(&json!({"ok":true,"protocolVersion":3,"version":"1.0.98"})).is_ok());
            assert!(handshake(&json!({"ok":true,"protocolVersion":3,"version":"1.0.99"})).is_err());
        }
        #[test]
        fn registry_file_permissions_type_identity_and_freshness() {
            let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("server.json");
            fs::write(&path, serde_json::to_vec(&fixture()).unwrap()).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            let uid = fs::metadata(&path).unwrap().uid();
            let (_file, before, _) = read_registration(&path, uid).unwrap_or_else(|_| panic!("fixture rejected"));
            assert!(read_registration(&path, uid.wrapping_add(1)).is_err());
            assert!(trusted_file(&before, uid, before.modified().unwrap() - Duration::from_secs(1)).is_err());
            assert!(trusted_file(&before, uid, before.modified().unwrap() + Duration::from_secs(301)).is_err());
            let link = dir.path().join("link.json"); symlink(&path, &link).unwrap();
            assert!(read_registration(&link, uid).is_err());
            assert!(read_registration(dir.path(), uid).is_err());
            fs::hard_link(&path, dir.path().join("hard.json")).unwrap();
            assert!(read_registration(&path, uid).is_err());
            fs::remove_file(dir.path().join("hard.json")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
            assert!(read_registration(&path, uid).is_err());
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            fs::rename(&path, dir.path().join("old.json")).unwrap();
            fs::write(&path, b"replacement").unwrap();
            assert!(!same(&before, &fs::metadata(&path).unwrap()));
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            File::options().write(true).open(&path).unwrap().set_len(ENTRY_LIMIT + 1).unwrap();
            assert!(read_registration(&path, uid).is_err());
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o777)).unwrap();
            assert!(trusted_dir(dir.path(), uid).is_err());
        }
        #[test]
        fn registry_enumeration_is_bounded() {
            let dir = tempfile::tempdir().unwrap();
            assert!(candidates(dir.path()).is_err());
            for n in 0..16 { fs::write(dir.path().join(format!("{n}.json")), b"{}").unwrap(); }
            assert_eq!(candidates(dir.path()).unwrap().len(), 16);
            fs::write(dir.path().join("17.json"), b"{}").unwrap();
            assert!(candidates(dir.path()).is_err());
        }
        #[test]
        fn framing_limits_utf8_secret_and_server_requests() {
            assert_eq!(parse(wire("{\"result\":\"中文\"}".as_bytes())).unwrap()["result"], "中文");
            for bytes in [b"Content-Length: 131073\r\n\r\n".to_vec(), b"X: y\r\n\r\n".to_vec(),
                b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}".to_vec(), vec![b'x';8193],
                b"Content-Length: 2\r\n\r\n{".to_vec(),
                wire(&[0xff]), wire(b"{"), wire(format!("{{\"x\":\"{TOKEN}\"}}").as_bytes()),
                wire(br#"{"x":"fake-secret-never-prin\u0074"}"#)] { assert!(parse(bytes).is_err()); }
            assert!(response(json!({"jsonrpc":"2.0","id":1,"method":"tools/call"}),1).is_err());
            assert!(response(json!({"jsonrpc":"2.0","id":2,"result":{}}),1).is_err());
            assert!(response(json!({"jsonrpc":"2.0","id":1,"error":{"message":TOKEN}}),1).is_err());
            assert!(response(json!({"jsonrpc":"2.0","method":"notice"}),1).unwrap().is_none());
            assert!(handshake(&json!({"ok":true,"protocolVersion":3,"version":"1.0.98"})).is_ok());
            assert!(handshake(&json!({"ok":true,"protocolVersion":2,"version":"1.0.98"})).is_err());
            assert!(timeout(Instant::now() - Duration::from_secs(1)).is_err());
            let error = finish(parse(wire(format!("{{\"x\":\"{TOKEN}\"}}").as_bytes())).map(|_| Vec::<PathBuf>::new())).unwrap_err();
            assert_eq!(format!("{error:#}"), UNKNOWN);
            let notice = wire(br#"{"jsonrpc":"2.0","method":"notice"}"#);
            let result = wire(br#"{"jsonrpc":"2.0","id":1,"result":{"ok":true,"protocolVersion":3,"version":"1.0.98"}}"#);
            let mut input = Cursor::new([notice.clone(), result].concat()); let mut frames = 0;
            let result = reply(|out| { let n = out.len().min(3); input.read(&mut out[..n]).map_err(|_| ()) }, TOKEN, 1, &mut frames).unwrap();
            assert!(handshake(&result).is_ok()); assert_eq!(frames, 2);
            let mut input = Cursor::new(notice.repeat(33)); let mut frames = 0;
            assert!(reply(|out| input.read(out).map_err(|_| ()), TOKEN, 1, &mut frames).is_err());
            assert_eq!(frames, 32);
        }
        #[test]
        fn roots_bind_to_native_cwd_and_validate_personal_scopes() {
            let mut value = json!({"paths":[
                {"path":"/profile/skills","scope":"personal-copilot","preferredForCreation":true},
                {"path":"relative-skills","scope":"custom","preferredForCreation":false},
                {"path":"/absolute/custom","scope":"custom","preferredForCreation":false}]});
            let p = process();
            let roots = discovery(&value, Path::new("/home"), Path::new("/profile"), &p, TOKEN).unwrap();
            assert!(roots.contains(Path::new("/native/project/relative-skills")));
            assert!(roots.contains(Path::new("/absolute/custom")));
            let secret_exe = Process { exe: PathBuf::from(format!("/{TOKEN}/copilot")), ..p.clone() };
            assert!(discovery(&value, Path::new("/home"), Path::new("/profile"), &secret_exe, TOKEN).is_err());
            for raw in ["", "bad\0root", TOKEN] { value["paths"][1]["path"] = json!(raw);
                assert!(discovery(&value, Path::new("/home"), Path::new("/profile"), &p, TOKEN).is_err()); }
            value["paths"][1]["path"] = json!("relative-skills");
            value["paths"][0]["path"] = json!("/wrong/skills");
            assert!(discovery(&value, Path::new("/home"), Path::new("/profile"), &p, TOKEN).is_err());
            value["paths"][0]["path"] = json!("/profile/skills"); value["paths"][1]["scope"] = json!("project");
            assert!(discovery(&value, Path::new("/home"), Path::new("/profile"), &p, TOKEN).is_err());
            value["paths"][1]["scope"] = json!("personal-agents");
            assert!(discovery(&value, Path::new("/home"), Path::new("/profile"), &p, TOKEN).is_err());
            value["paths"][1]["path"] = json!("/home/.agents/skills");
            assert!(discovery(&value, Path::new("/home"), Path::new("/profile"), &p, TOKEN).is_ok());
        }
        #[test]
        fn literal_tilde_paths_use_verified_native_cwd_without_home_expansion() {
            let p = process();
            let value = json!({"paths":[
                {"path":"~/profile/skills","scope":"personal-copilot","preferredForCreation":true},
                {"path":"~/custom","scope":"custom","preferredForCreation":false}]});
            let roots = discovery(&value, Path::new("/home"), Path::new("/native/project/~/profile"), &p, TOKEN).unwrap();
            assert!(roots.contains(Path::new("/native/project/~/profile/skills")));
            assert!(roots.contains(Path::new("/native/project/~/custom")));
            assert!(!roots.contains(Path::new("/home/custom")));
            assert!(discovery(&value, Path::new("/home"), Path::new("/home/profile"), &p, TOKEN).is_err());
        }
        #[test]
        fn identity_changes_and_multiple_candidates_fail_closed() {
            let p = process(); assert!(unchanged(&p, &p).is_ok());
            assert!(matches_project(&p, &p, Path::new("/native/project")).unwrap());
            assert!(!matches_project(&p, &p, Path::new("/different/project")).unwrap());
            for changed in [Process { pid:43, ..p.clone() }, Process { secs:101, ..p.clone() },
                Process { micros:3, ..p.clone() }, Process { cwd:"/other".into(), ..p.clone() },
                Process { exe:"/other/copilot".into(), ..p.clone() }, Process { uid:502, ..p.clone() },
                Process { ruid:502, ..p.clone() }] {
                assert!(unchanged(&p, &changed).is_err());
                assert!(matches_project(&p, &changed, Path::new("/different/project")).is_err());
            }
            let mut candidates = None;
            let roots = BTreeSet::from([PathBuf::from("/profile/skills")]);
            assert!(agree(&mut candidates, roots.clone()).is_ok());
            assert!(agree(&mut candidates, roots).is_ok());
            assert!(agree(&mut candidates, BTreeSet::from([PathBuf::from("/other/skills")])).is_err());
        }

        #[test]
        fn credentials_are_sent_only_after_connected_peer_and_process_checks() {
            use std::cell::{Cell, RefCell};
            let p = process();
            let sent = Cell::new(0);
            // Both a different owner after connect and unknown socket metadata
            // fail here, even though the old PID/registry prechecks succeeded.
            for _ in ["port_replaced_after_precheck", "owner_unknown"] {
                assert!(verified_exchange(&p, || Err(()), || Ok(p.clone()),
                    || { sent.set(sent.get() + 1); Ok(()) }).is_err());
                assert_eq!(sent.get(), 0);
            }
            for changed in [Process { pid:43, ..p.clone() }, Process { secs:101, ..p.clone() },
                Process { micros:3, ..p.clone() }, Process { uid:502, ..p.clone() },
                Process { ruid:502, ..p.clone() }, Process { exe:"/other/copilot".into(), ..p.clone() },
                Process { cwd:"/other".into(), ..p.clone() }] {
                assert!(verified_exchange(&p, || Ok(()), || Ok(changed),
                    || { sent.set(sent.get() + 1); Ok(()) }).is_err());
                assert_eq!(sent.get(), 0);
            }
            let order = RefCell::new(Vec::new());
            verified_exchange(&p,
                || { order.borrow_mut().push("connected-peer"); Ok(()) },
                || { order.borrow_mut().push("process-snapshot"); Ok(p.clone()) },
                || { order.borrow_mut().push("token-rpc"); sent.set(sent.get() + 1); Ok(()) }).unwrap();
            assert_eq!(&*order.borrow(), &["connected-peer", "process-snapshot", "token-rpc"]);
            assert_eq!(sent.get(), 1);
        }
    }
}
