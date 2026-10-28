//! Native `grok inspect --json` catalog, without a second config/trust engine.
//! Only the allowlisted projection leaves this module. No raw stdout/stderr,
//! account files, shell/bootstrap invocation, trust grant or persistent cache.
//! Inspect is report-only (remote kill-switch absent), not model/body evidence.

use std::{
    ffi::OsStr,
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Seek, SeekFrom, ErrorKind},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::DiscoveryContext;
#[cfg(all(unix, not(test)))]
use crate::agents::grok::grok_config_root;

const MAX_STDOUT: usize = 2 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(4);
const MAX_PACKAGE_JSON: usize = 64 * 1024;
const MAX_PATH_BYTES: usize = 64 * 1024;
const MAX_PATH_ENTRIES: usize = 512;
const MAX_SKILLS: usize = 8192;
const MAX_NATIVE_BYTES: u64 = 256 * 1024 * 1024;
// The local candidate only executes the official artifact actually verified in
// this compatibility run (Grok 1.0.50 macOS arm64). Other artifacts stay unknown
// until their official release identity and native behavior are verified.
const VERIFIED_NATIVE_SHA256: &str = "af41b1d65d4116ff649a689ad9d070d4c2abe255737d46ba0b3e8161345696e6";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Diagnostic {
    InvalidContext,
    ConfigRootUnknown,
    CliUnavailable,
    NativeIdentityUnknown,
    LaunchFailed,
    ProcessFailed,
    TimedOut,
    OutputLimit,
    InvalidReport,
    ConfigurationWarning,
    CwdMismatch,
    UnsupportedPlatform,
}

impl Diagnostic {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::InvalidContext => "Grok 原生检查上下文无效，状态未知",
            Self::ConfigRootUnknown => "Grok 配置根无法安全解析，状态未知",
            Self::CliUnavailable => "Grok 当前 PATH 没有可用命令，状态未知",
            Self::NativeIdentityUnknown => "Grok 当前命令不匹配已验证的官方原生版本，状态未知",
            Self::LaunchFailed => "Grok 原生检查无法启动，状态未知",
            Self::ProcessFailed => "Grok 原生检查未正常完成，状态未知",
            Self::TimedOut => "Grok 原生检查超时，状态未知",
            Self::OutputLimit => "Grok 原生检查输出超过上限，状态未知",
            Self::InvalidReport => "Grok 原生报告字段或格式无效，状态未知",
            Self::ConfigurationWarning => "Grok 原生报告存在配置警告，状态未知",
            Self::CwdMismatch => "Grok 原生报告工作目录不匹配，状态未知",
            Self::UnsupportedPlatform => "Grok 原生检查尚不支持此平台，状态未知",
        }
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}
impl std::error::Error for Diagnostic {}

/// These are the ONLY report fields returned/serialized, not a full inspect DTO.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeCatalog {
    pub grok_version: String,
    pub cwd: PathBuf,
    pub project_root: Option<PathBuf>,
    pub project_trusted: bool,
    pub skills: Vec<NativeSkill>,
}

impl NativeCatalog {
    /// Unknown sources may retain native path hints, but are never verified skills.
    pub(crate) fn has_unknown_sources(&self) -> bool {
        self.skills.iter().any(|skill| skill.source.kind == SourceKind::Unknown)
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeSkill {
    pub name: String,
    pub description: String,
    pub source: NativeSource,
    pub user_invocable: bool,
    /// Missing in native JSON means false; explicit wrong types are errors.
    pub disabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collides_with: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invocable_as: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum SourceKind {
    Builtin, Bundled, Server, Project, User, Plugin, ConfigToml,
    ClaudeJson, McpJson, Cli, Managed, Unknown,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct NativeSource {
    #[serde(rename = "type")]
    pub kind: SourceKind,
    /// Valid native label, including future kinds; never inferred from a path.
    pub source_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireCatalog {
    grok_version: String,
    cwd: PathBuf,
    project_root: Option<PathBuf>,
    project_trusted: bool,
    skills: Vec<WireSkill>,
    // Inspect can exit zero after falling back from invalid config. Never
    // represent such a catalog as a clean, effective configuration. Contents
    // are skipped, not returned or included in diagnostics.
    #[serde(default)]
    config_warnings: Vec<serde::de::IgnoredAny>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireSkill {
    name: String,
    description: String,
    source: Value,
    user_invocable: bool,
    #[serde(default)]
    disabled: bool,
    collides_with: Option<String>,
    invocable_as: Option<String>,
}

/// Pure bounded parser. Wrong required field types -> Err (unknown catalog),
/// unknown/invalid sources -> Unknown with NO guessed or relative path.
/// No disabled/model-invocation inference, re-normalizing or re-deduplication.
/// Source/body availability and disable-model-invocation require the host's
/// separate bounded frontmatter read; inspect alone does not prove invocation.
pub(crate) fn parse_json(bytes: &[u8]) -> Result<NativeCatalog, Diagnostic> {
    if bytes.len() > MAX_STDOUT { return Err(Diagnostic::OutputLimit); }
    let report: WireCatalog = serde_json::from_slice(bytes).map_err(|_| Diagnostic::InvalidReport)?;
    if !report.config_warnings.is_empty() { return Err(Diagnostic::ConfigurationWarning); }
    if !text(&report.grok_version, 128, false) || !absolute_path(&report.cwd)
        || report.project_root.as_ref().is_some_and(|path| !absolute_path(path))
        || report.skills.len() > MAX_SKILLS
    { return Err(Diagnostic::InvalidReport); }
    let mut skills = Vec::with_capacity(report.skills.len());
    for skill in report.skills {
        if !text(&skill.name, 4096, false) || !text(&skill.description, 64 * 1024, true)
            || skill.collides_with.as_ref().is_some_and(|s| !text(s, 4096, false))
            || skill.invocable_as.as_ref().is_some_and(|s| !text(s, 4096, false))
        { return Err(Diagnostic::InvalidReport); }
        skills.push(NativeSkill {
            name: skill.name, description: skill.description,
            source: parse_source(&skill.source), user_invocable: skill.user_invocable,
            disabled: skill.disabled, collides_with: skill.collides_with,
            invocable_as: skill.invocable_as,
        });
    }
    Ok(NativeCatalog {
        grok_version: report.grok_version, cwd: report.cwd,
        project_root: report.project_root, project_trusted: report.project_trusted, skills,
    })
}

fn text(value: &str, max: usize, multiline: bool) -> bool {
    // Empty descriptions are valid; identity/version strings must not be empty.
    value.len() <= max && (multiline || !value.is_empty())
        && !value.chars().any(|ch| ch.is_control() && !(multiline && matches!(ch, '\n' | '\r' | '\t')))
}

fn absolute_path(path: &Path) -> bool {
    path.to_str().is_some_and(|s| s.len() <= 16 * 1024 && !s.chars().any(char::is_control))
        && path.is_absolute()
}

fn parse_source(value: &Value) -> NativeSource {
    let label = value.get("type").and_then(Value::as_str).filter(|label| text(label, 128, false));
    let mut kind = match label {
        Some("builtin") => SourceKind::Builtin, Some("bundled") => SourceKind::Bundled,
        Some("server") => SourceKind::Server, Some("project") => SourceKind::Project,
        Some("user") => SourceKind::User, Some("plugin") => SourceKind::Plugin,
        Some("configToml") => SourceKind::ConfigToml, Some("claudeJson") => SourceKind::ClaudeJson,
        Some("mcpJson") => SourceKind::McpJson, Some("cli") => SourceKind::Cli,
        Some("managed") => SourceKind::Managed, _ => SourceKind::Unknown,
    };
    let path = match value.get("path") {
        Some(Value::String(raw)) if absolute_path(Path::new(raw)) => Some(PathBuf::from(raw)),
        None | Some(Value::Null) if matches!(kind, SourceKind::Builtin | SourceKind::Managed) => None,
        _ => { kind = SourceKind::Unknown; None },
    };
    let snake = value.get("plugin_name");
    let camel = value.get("pluginName");
    let plugin_name = if label == Some("plugin") || snake.is_some() || camel.is_some() {
        let name = if snake.is_some() && camel.is_some() && snake != camel { None }
            else { snake.or(camel).and_then(Value::as_str).filter(|name| text(name, 4096, false)) };
        match name {
            Some(name) => Some(name.to_owned()),
            None => { kind = SourceKind::Unknown; None },
        }
    } else { None };
    NativeSource { kind, source_type: label.unwrap_or("unknown").to_owned(), path, plugin_name }
}

/// Product entry point. Samples GROK_HOME through the shared root helper and
/// PATH only to select its FIRST active `grok`. Child inherits necessary env;
/// HOME is the context home and GROK_HOME is frozen to that same selected root.
/// The Rust host must leave this Child's reaping exclusively to this module;
/// see OwnedChild's process-wide contract (WNOWAIT is not a reaper lock).
/// This function is never called by the isolated test definitions below.
#[cfg(all(unix, not(test)))]
pub(super) fn read(context: &DiscoveryContext) -> Result<NativeCatalog, Diagnostic> {
    if !absolute_path(&context.cwd) || !absolute_path(&context.home) {
        return Err(Diagnostic::InvalidContext);
    }
    let root = grok_config_root(&context.home).map_err(|_| Diagnostic::ConfigRootUnknown)?;
    let cwd = context.cwd.canonicalize().map_err(|_| Diagnostic::InvalidContext)?;
    if !cwd.is_dir() { return Err(Diagnostic::InvalidContext); }
    let lookup_cwd = std::env::current_dir().map_err(|_| Diagnostic::CliUnavailable)?;
    let path = std::env::var_os("PATH");
    let native = resolve_native(path.as_deref(), &lookup_cwd)?;
    let bytes = run_inspect(&native, &cwd, &context.home, &root, TIMEOUT, MAX_STDOUT)?;
    parse_for_cwd(&bytes, &cwd)
}

fn parse_for_cwd(bytes: &[u8], cwd: &Path) -> Result<NativeCatalog, Diagnostic> {
    let catalog = parse_json(bytes)?;
    if catalog.cwd != cwd { return Err(Diagnostic::CwdMismatch); }
    if catalog.grok_version != "1.0.50" { return Err(Diagnostic::NativeIdentityUnknown); }
    Ok(catalog)
}

// Unit tests use explicit native-report fixtures or fake owned executables.
// Merely running the ordinary test suite must not auto-start an installed host.
#[cfg(all(unix, test))]
pub(super) fn read(_context: &DiscoveryContext) -> Result<NativeCatalog, Diagnostic> {
    Err(Diagnostic::CliUnavailable)
}

#[cfg(not(unix))]
pub(super) fn read(_context: &DiscoveryContext) -> Result<NativeCatalog, Diagnostic> {
    Err(Diagnostic::UnsupportedPlatform)
}

/// Private capability: arbitrary executable paths/arguments cannot enter the
/// product runner. The held fd + metadata detect replacement before spawn.
/// This is PATH/package identity, not cryptographic publisher attestation.
#[cfg(unix)]
struct NativeExecutable {
    path: PathBuf,
    file: File,
    stamp: Metadata,
}

#[cfg(unix)]
fn resolve_native(path: Option<&OsStr>, lookup_cwd: &Path) -> Result<NativeExecutable, Diagnostic> {
    resolve_native_with_digest(path, lookup_cwd, VERIFIED_NATIVE_SHA256)
}

#[cfg(unix)]
fn resolve_native_with_digest(path: Option<&OsStr>, lookup_cwd: &Path, expected: &str) -> Result<NativeExecutable, Diagnostic> {
    use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
    let path = path.ok_or(Diagnostic::CliUnavailable)?;
    if path.as_encoded_bytes().len() > MAX_PATH_BYTES || !lookup_cwd.is_absolute() {
        return Err(Diagnostic::NativeIdentityUnknown);
    }
    for (index, dir) in std::env::split_paths(path).enumerate() {
        if index >= MAX_PATH_ENTRIES { return Err(Diagnostic::NativeIdentityUnknown); }
        let candidate = if dir.is_absolute() { dir.join("grok") } else { lookup_cwd.join(&dir).join("grok") };
        let meta = match fs::metadata(&candidate) {
            Ok(meta) => meta,
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(_) => return Err(Diagnostic::NativeIdentityUnknown),
        };
        if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 { continue; }
        // Never auto-execute a workspace-relative PATH entry while inspecting
        // another project. An active relative command also shadows later bins.
        if !dir.is_absolute() { return Err(Diagnostic::NativeIdentityUnknown); }
        let cpath = std::ffi::CString::new(candidate.as_os_str().as_bytes())
            .map_err(|_| Diagnostic::NativeIdentityUnknown)?;
        if unsafe { libc::access(cpath.as_ptr(), libc::X_OK) } != 0 { continue; }
        // An active unrecognized command shadows later installations. Stop here.
        let active = candidate.canonicalize().map_err(|_| Diagnostic::NativeIdentityUnknown)?;
        let native = if active.file_name() == Some(OsStr::new("grok-native")) {
            active
        } else {
            if active.file_name() != Some(OsStr::new("grok")) {
                return Err(Diagnostic::NativeIdentityUnknown);
            }
            let bin = active.parent().ok_or(Diagnostic::NativeIdentityUnknown)?;
            if bin.file_name() != Some(OsStr::new("bin")) {
                return Err(Diagnostic::NativeIdentityUnknown);
            }
            let package = bin.parent().ok_or(Diagnostic::NativeIdentityUnknown)?.join("package.json");
            let bytes = read_regular(&package, MAX_PACKAGE_JSON)?;
            let value: Value = serde_json::from_slice(&bytes).map_err(|_| Diagnostic::NativeIdentityUnknown)?;
            if value.get("name").and_then(Value::as_str) != Some("@xai-official/grok")
                || value.pointer("/bin/grok").and_then(Value::as_str) != Some("bin/grok")
            { return Err(Diagnostic::NativeIdentityUnknown); }
            bin.join("grok-native").canonicalize().map_err(|_| Diagnostic::NativeIdentityUnknown)?
        };
        let (mut file, stamp) = open_regular(&native)?;
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic).map_err(|_| Diagnostic::NativeIdentityUnknown)?;
        if !native_magic(magic) || !same_file(&stamp, &file.metadata().map_err(|_| Diagnostic::NativeIdentityUnknown)?) {
            return Err(Diagnostic::NativeIdentityUnknown);
        }
        if stamp.permissions().mode() & 0o111 == 0 || stamp.permissions().mode() & 0o6000 != 0 {
            return Err(Diagnostic::NativeIdentityUnknown);
        }
        if stamp.len() > MAX_NATIVE_BYTES { return Err(Diagnostic::NativeIdentityUnknown); }
        file.seek(SeekFrom::Start(0)).map_err(|_| Diagnostic::NativeIdentityUnknown)?;
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let count = file.read(&mut buffer).map_err(|_| Diagnostic::NativeIdentityUnknown)?;
            if count == 0 { break; }
            total += count as u64;
            if total > MAX_NATIVE_BYTES { return Err(Diagnostic::NativeIdentityUnknown); }
            digest.update(&buffer[..count]);
        }
        if format!("{:x}", digest.finalize()) != expected || total != stamp.len()
            || !same_file(&stamp, &file.metadata().map_err(|_| Diagnostic::NativeIdentityUnknown)?)
            || !same_file(&stamp, &fs::symlink_metadata(&native).map_err(|_| Diagnostic::NativeIdentityUnknown)?)
        { return Err(Diagnostic::NativeIdentityUnknown); }
        return Ok(NativeExecutable { path: native, file, stamp });
    }
    Err(Diagnostic::CliUnavailable)
}

fn native_magic(magic: [u8; 4]) -> bool {
    matches!(magic, [0x7f, b'E', b'L', b'F'] | [0xfe,0xed,0xfa,0xce] | [0xce,0xfa,0xed,0xfe]
        | [0xfe,0xed,0xfa,0xcf] | [0xcf,0xfa,0xed,0xfe] | [0xca,0xfe,0xba,0xbe]
        | [0xbe,0xba,0xfe,0xca] | [0xca,0xfe,0xba,0xbf] | [0xbf,0xba,0xfe,0xca])
}

#[cfg(unix)]
fn open_regular(path: &Path) -> Result<(File, Metadata), Diagnostic> {
    use std::os::unix::fs::OpenOptionsExt;
    let before = fs::symlink_metadata(path).map_err(|_| Diagnostic::NativeIdentityUnknown)?;
    if !before.is_file() { return Err(Diagnostic::NativeIdentityUnknown); }
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path).map_err(|_| Diagnostic::NativeIdentityUnknown)?;
    let stamp = file.metadata().map_err(|_| Diagnostic::NativeIdentityUnknown)?;
    if !stamp.is_file() || !same_file(&before, &stamp) {
        return Err(Diagnostic::NativeIdentityUnknown);
    }
    Ok((file, stamp))
}

#[cfg(unix)]
fn read_regular(path: &Path, limit: usize) -> Result<Vec<u8>, Diagnostic> {
    let (file, stamp) = open_regular(path)?;
    if stamp.len() > limit as u64 { return Err(Diagnostic::NativeIdentityUnknown); }
    let mut bytes = Vec::new();
    (&file).take(limit as u64 + 1).read_to_end(&mut bytes).map_err(|_| Diagnostic::NativeIdentityUnknown)?;
    let after = file.metadata().map_err(|_| Diagnostic::NativeIdentityUnknown)?;
    let path_after = fs::symlink_metadata(path).map_err(|_| Diagnostic::NativeIdentityUnknown)?;
    if bytes.len() > limit || stamp.len() != bytes.len() as u64
        || !same_file(&stamp, &after) || !same_file(&stamp, &path_after)
    { return Err(Diagnostic::NativeIdentityUnknown); }
    Ok(bytes)
}

#[cfg(unix)]
fn same_file(a: &Metadata, b: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino() && a.mode() == b.mode()
        && a.len() == b.len() && a.mtime() == b.mtime() && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime() && a.ctime_nsec() == b.ctime_nsec()
}

#[cfg(unix)]
// Host contract: this module exclusively owns reaping this Child until it
// transfers it to its wait-only reaper. No SIGCHLD=SIG_IGN/SA_NOCLDWAIT, global
// waitpid(-1)/waitid(P_ALL), or competing wait for this PID is permitted.
// WNOWAIT detects an already lost pin; it cannot defeat an external reaper
// racing AFTER the check. Rust's Child ownership alone does not enforce this
// process-wide contract. Keep the leader unreaped through group cleanup.
struct OwnedChild {
    child: Option<Child>,
    pgid: libc::pid_t,
    group_owned: bool,
    leader_exited: bool,
    #[cfg(test)]
    test_hooks: CleanupTestHooks,
}

#[cfg(all(unix, test))]
#[derive(Default)]
struct CleanupTestHooks {
    probe: Option<fn(libc::pid_t, libc::pid_t) -> Result<bool, Diagnostic>>,
    signal: Option<Box<dyn FnMut(libc::pid_t) -> Result<(), i32>>>,
    fail_nonblocking: bool,
}

#[cfg(all(unix, any(target_os = "macos", test)))]
fn checked_member_count(count: i32, capacity: usize, error: Option<i32>) -> Result<usize, Diagnostic> {
    if count < 0 || count as usize >= capacity
        || (count == 0 && error.is_some_and(|error| error != 0 && error != libc::ESRCH))
    { return Err(Diagnostic::ProcessFailed); }
    Ok(count as usize)
}

#[cfg(all(unix, any(target_os = "macos", test)))]
fn checked_pid_info(size: i32, expected: i32, error: Option<i32>) -> Result<bool, Diagnostic> {
    if size == expected { return Ok(true); }
    if size == 0 && error == Some(libc::ESRCH) { return Ok(false); }
    Err(Diagnostic::ProcessFailed)
}

// Darwin's killpg excludes zombies and can return EPERM for a group containing
// only an unreaped, exited leader (xnu/bsd/kern/kern_sig.c:killpg1). Read only
// bounded status metadata before deciding whether a signal is necessary.
#[cfg(target_os = "macos")]
fn group_has_live_members(pgid: libc::pid_t, exited_leader: libc::pid_t) -> Result<bool, Diagnostic> {
    let mut members = [0i32; 256];
    unsafe { *libc::__error() = 0; }
    let count = unsafe { libc::proc_listpgrppids(pgid, members.as_mut_ptr().cast(),
        std::mem::size_of_val(&members) as i32) };
    // Unlike proc_listpids, this libproc wrapper returns PID COUNT, not bytes.
    let count = checked_member_count(count, members.len(), std::io::Error::last_os_error().raw_os_error())?;
    for pid in members.into_iter().take(count) {
        if pid == 0 || pid == exited_leader { continue; }
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let expected = std::mem::size_of_val(&info) as i32;
        let size = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0,
            (&mut info as *mut libc::proc_bsdinfo).cast(), expected) };
        if !checked_pid_info(size, expected, std::io::Error::last_os_error().raw_os_error())? { continue; }
        if info.pbi_pgid == pgid as u32 && info.pbi_status != libc::SZOMB { return Ok(true); }
    }
    Ok(false)
}

#[cfg(unix)]
impl OwnedChild {
    fn live_members(&self) -> Result<bool, Diagnostic> {
        #[cfg(test)]
        if let Some(probe) = self.test_hooks.probe { return probe(self.pgid, self.pgid); }
        #[cfg(target_os = "macos")]
        { group_has_live_members(self.pgid, self.pgid) }
        #[cfg(not(target_os = "macos"))]
        { Ok(true) }
    }

    fn kill_group(&mut self) -> Result<(), Diagnostic> {
        if !self.group_owned { return Ok(()); }
        // Shared by finish and every early-return Drop, including failures
        // before the read loop has ever polled exited(). ECHILD revokes the pin.
        self.exited()?;
        let probe = if self.leader_exited { self.live_members() } else { Ok(true) };
        if probe == Ok(false) {
            self.group_owned = false;
            return Ok(());
        }
        // Unknown status is a catalog failure, not permission to abandon live
        // descendants. Recheck ownership after probing, before the sole signal.
        self.exited()?;
        self.group_owned = false; // Consume the attempt even on EPERM; no retry.
        let pgid = self.pgid;
        let send = || {
            if unsafe { libc::kill(-pgid, libc::SIGKILL) } == 0 { Ok(()) }
            else { Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0)) }
        };
        #[cfg(test)]
        let signal = match self.test_hooks.signal.as_mut() {
            Some(signal) => signal(self.pgid), None => send(),
        };
        #[cfg(not(test))]
        let signal = send();
        match signal {
            Ok(()) | Err(libc::ESRCH) => {},
            #[cfg(target_os = "macos")]
            Err(libc::EPERM) if self.leader_exited && self.live_members() == Ok(false) => {},
            Err(_) => return Err(Diagnostic::ProcessFailed),
        }
        // Even successful cleanup cannot turn a failed status query into a
        // verified catalog. Retain the original fixed diagnostic after reaping.
        probe.map(|_| ())
    }

    fn exited(&mut self) -> Result<bool, Diagnostic> {
        let id = self.child.as_ref().ok_or(Diagnostic::ProcessFailed)?.id();
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(libc::P_PID, id as libc::id_t, &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT)
        };
        if result == 0 {
            let exited = info.si_signo == libc::SIGCHLD;
            self.leader_exited |= exited;
            return Ok(exited);
        }
        let error = std::io::Error::last_os_error();
        // EINTR and other observation errors are not positive ownership proof:
        // fail closed here instead of allowing a signal on an unchecked PID.
        if error.raw_os_error() == Some(libc::ECHILD) {
            // An external reaper invalidated our ownership pin. Never send a
            // later signal to a PGID whose leader may already be recycled.
            self.group_owned = false;
            self.child.take(); // Do not later wait on a possibly reused PID.
        }
        Err(Diagnostic::ProcessFailed)
    }

    fn finish(mut self) -> Result<ExitStatus, Diagnostic> {
        let cleanup = self.kill_group();
        // Only called after waitid(WNOWAIT) established the leader has exited.
        let status = self.child.take().ok_or(Diagnostic::ProcessFailed)?.wait()
            .map_err(|_| Diagnostic::ProcessFailed);
        cleanup?;
        status
    }
}

#[cfg(unix)]
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.kill_group();
        let Some(mut child) = self.child.take() else { return; };
        let deadline = Instant::now() + Duration::from_millis(200);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Err(error) if error.raw_os_error() == Some(libc::ECHILD) => return,
                Err(_) => break,
                Ok(None) if Instant::now() >= deadline => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        // Cleanup was attempted; a failed signal is never retried. A delayed
        // exit must not turn the UI timeout into an unbounded wait. Transfer
        // only this owned Child to a reaper; it never reads output or sends
        // any further group signal.
        let _ = std::thread::Builder::new().name("grok-inspect-reaper".into())
            .spawn(move || { let _ = child.wait(); });
    }
}

#[cfg(unix)]
fn spawn_owned(native: &NativeExecutable, cwd: &Path, home: &Path, root: &Path) -> Result<OwnedChild, Diagnostic> {
    use std::os::unix::process::CommandExt;
    let current = fs::symlink_metadata(&native.path).map_err(|_| Diagnostic::NativeIdentityUnknown)?;
    let held = native.file.metadata().map_err(|_| Diagnostic::NativeIdentityUnknown)?;
    if !same_file(&native.stamp, &current) || !same_file(&native.stamp, &held) {
        return Err(Diagnostic::NativeIdentityUnknown);
    }
    let child = Command::new(&native.path)
        .args(["inspect", "--json"])
        .current_dir(cwd).env("HOME", home).env("GROK_HOME", root)
        .stdin(Stdio::null()).stderr(Stdio::null()).stdout(Stdio::piped())
        .process_group(0).spawn().map_err(|_| Diagnostic::LaunchFailed)?;
    let pgid = child.id() as libc::pid_t;
    Ok(OwnedChild { child: Some(child), pgid, group_owned: true, leader_exited: false,
        #[cfg(test)] test_hooks: CleanupTestHooks::default() })
}

#[cfg(unix)]
fn run_inspect(native: &NativeExecutable, cwd: &Path, home: &Path, root: &Path,
    timeout: Duration, limit: usize) -> Result<Vec<u8>, Diagnostic>
{
    let deadline = Instant::now() + timeout;
    let child = spawn_owned(native, cwd, home, root)?;
    collect_inspect(child, deadline, limit)
}

#[cfg(unix)]
fn collect_inspect(mut child: OwnedChild, deadline: Instant, limit: usize) -> Result<Vec<u8>, Diagnostic> {
    use std::os::fd::AsRawFd;
    let mut stdout = child.child.as_mut().ok_or(Diagnostic::ProcessFailed)?
        .stdout.take().ok_or(Diagnostic::ProcessFailed)?;
    let fd = stdout.as_raw_fd();
    #[cfg(test)]
    if child.test_hooks.fail_nonblocking { return Err(Diagnostic::ProcessFailed); }
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(Diagnostic::ProcessFailed);
    }
    let mut bytes = Vec::new();
    let mut eof = false;
    let mut chunk = [0u8; 8192];
    loop {
        if Instant::now() >= deadline { return Err(Diagnostic::TimedOut); }
        if !eof {
            let max_read = chunk.len().min(limit.saturating_sub(bytes.len()).saturating_add(1));
            match stdout.read(&mut chunk[..max_read]) {
                Ok(0) => eof = true,
                Ok(count) => {
                    if count > limit.saturating_sub(bytes.len()) { return Err(Diagnostic::OutputLimit); }
                    bytes.extend_from_slice(&chunk[..count]);
                    continue; // Includes a deadline check even for endless output.
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(_) => return Err(Diagnostic::ProcessFailed),
            }
        }
        if child.exited()? && eof {
            let status = child.finish()?;
            if !status.success() { return Err(Diagnostic::ProcessFailed); }
            return Ok(bytes);
        }
        std::thread::sleep(Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::fixture_path;

    fn report() -> Value {
        serde_json::json!({"grokVersion":"1.0.50","cwd":fixture_path("/synthetic/project"),
            "projectRoot":fixture_path("/synthetic/project"),"projectTrusted":false,
            "skills":[{"name":"exact-Native_Name","description":"synthetic\nonly",
                "source":{"type":"user","path":fixture_path("/synthetic/custom/skills/a/SKILL.md")},
                "userInvocable":true}]})
    }
    fn parse(value: &Value) -> Result<NativeCatalog, Diagnostic> {
        parse_json(&serde_json::to_vec(value).unwrap())
    }

    #[test]
    fn projection_has_only_allowed_fields_and_defaults_omitted_disabled() {
        let mut value = report();
        value["permissions"] = serde_json::json!({"SECRET":"never-return"});
        value["skills"][0]["body"] = Value::from("SECRET_BODY");
        value["skills"][0]["source"]["token"] = Value::from("SECRET_TOKEN");
        let parsed = parse(&value).unwrap();
        assert!(!parsed.project_trusted && !parsed.skills[0].disabled);
        assert_eq!(parsed.skills[0].name, "exact-Native_Name");
        assert!(!parsed.has_unknown_sources());
        let encoded = serde_json::to_string(&parsed).unwrap();
        assert!(!encoded.contains("SECRET") && !encoded.contains("permissions"));
        value["projectTrusted"] = Value::from(true);
        value["skills"][0]["disabled"] = Value::from(true);
        value["skills"][0]["collidesWith"] = Value::from("login");
        value["skills"][0]["invocableAs"] = Value::from("user:login");
        let parsed = parse(&value).unwrap();
        assert!(parsed.project_trusted && parsed.skills[0].disabled);
        assert_eq!(parsed.skills[0].invocable_as.as_deref(), Some("user:login"));
    }

    #[test]
    fn wrong_types_missing_required_fields_warnings_and_invalid_json_are_unknown() {
        for (pointer, bad) in [("grokVersion", Value::Null), ("projectTrusted", Value::from("true")),
            ("skills", Value::Bool(false)), ("cwd", Value::from("relative"))]
        {
            let mut value = report(); value[pointer] = bad;
            assert_eq!(parse(&value).unwrap_err(), Diagnostic::InvalidReport);
        }
        for (key, bad) in [("name", Value::Null), ("description", Value::Bool(false)),
            ("disabled", Value::Null), ("userInvocable", Value::from("false")),
            ("collidesWith", Value::from(9)), ("invocableAs", Value::Bool(true))]
        {
            let mut value = report(); value["skills"][0][key] = bad;
            assert_eq!(parse(&value).unwrap_err(), Diagnostic::InvalidReport);
        }
        let mut value = report(); value.as_object_mut().unwrap().remove("projectTrusted");
        assert_eq!(parse(&value).unwrap_err(), Diagnostic::InvalidReport);
        let mut value = report(); value["configWarnings"] = serde_json::json!([{"message":"SECRET"}]);
        assert_eq!(parse(&value).unwrap_err(), Diagnostic::ConfigurationWarning);
        assert_eq!(parse_json(b"SECRET_NOT_JSON").unwrap_err(), Diagnostic::InvalidReport);
        assert_eq!(parse_json(&vec![b' '; MAX_STDOUT + 1]).unwrap_err(), Diagnostic::OutputLimit);
        assert_eq!(parse_for_cwd(&serde_json::to_vec(&report()).unwrap(), &fixture_path("/other")).unwrap_err(), Diagnostic::CwdMismatch);
    }

    #[test]
    fn unknown_sources_and_unsafe_paths_never_get_guessed() {
        for source in [serde_json::json!({"type":"user","path":"relative/SKILL.md"}),
            serde_json::json!({"type":"user","path":fixture_path("/bad\npath")}),
            serde_json::json!({"type":"user","path":4}), Value::Null]
        {
            let mut value = report(); value["skills"][0]["source"] = source;
            let parsed = parse(&value).unwrap();
            assert!(parsed.has_unknown_sources());
            assert!(parsed.skills[0].source.path.is_none());
        }
        for kind in ["project", "configToml", "bundled", "server"] {
            let source = parse_source(&serde_json::json!({"type":kind,"path":fixture_path("/synthetic/SKILL.md")}));
            assert_ne!(source.kind, SourceKind::Unknown);
        }
        assert_eq!(parse_source(&serde_json::json!({"type":"builtin"})).path, None);
        let plugin = parse_source(&serde_json::json!({"type":"plugin","plugin_name":"native-provider","path":fixture_path("/synthetic/SKILL.md")}));
        assert_eq!(plugin.plugin_name.as_deref(), Some("native-provider"));
        assert_eq!(parse_source(&serde_json::json!({"type":"plugin","path":fixture_path("/synthetic/SKILL.md")})).kind, SourceKind::Unknown);
    }

    #[test]
    fn source_metadata_survives_unknown_kind_and_invalid_path() {
        for (label, path, plugin, kind) in [
            ("future", Some("/synthetic/future/SKILL.md"), Some("future-provider"), SourceKind::Unknown),
            ("plugin", Some("/synthetic/plugin/SKILL.md"), Some("provider"), SourceKind::Plugin),
            ("plugin", Some("relative/SKILL.md"), Some("provider"), SourceKind::Unknown),
            ("builtin", None, None, SourceKind::Builtin),
            ("managed", None, None, SourceKind::Managed),
            ("future", None, None, SourceKind::Unknown),
        ] {
            let path = path.map(fixture_path);
            let mut value = report();
            value["skills"][0]["source"] = serde_json::json!({"type":label,"path":path,"plugin_name":plugin});
            // No plugin field is valid for builtin/managed (null is not a name).
            if plugin.is_none() { value["skills"][0]["source"].as_object_mut().unwrap().remove("plugin_name"); }
            value["skills"][0]["disabled"] = Value::Bool(true);
            value["skills"][0]["collidesWith"] = Value::from("login");
            value["skills"][0]["invocableAs"] = Value::from("user:login");
            let catalog = parse(&value).unwrap();
            let skill = &catalog.skills[0];
            assert_eq!(skill.source.source_type, label);
            assert_eq!(skill.source.kind, kind);
            assert_eq!(skill.source.path, path.filter(|path| path.is_absolute()));
            assert_eq!(skill.source.plugin_name.as_deref(), plugin);
            assert_eq!(skill.name, "exact-Native_Name");
            assert_eq!(skill.description, "synthetic\nonly");
            assert!(skill.user_invocable && skill.disabled);
            assert_eq!(skill.collides_with.as_deref(), Some("login"));
            assert_eq!(skill.invocable_as.as_deref(), Some("user:login"));
        }
        for label in ["", "bad\nlabel", &"x".repeat(129)] {
            let source = parse_source(&serde_json::json!({"type":label,"path":fixture_path("/synthetic/hint")}));
            assert_eq!(source.source_type, "unknown");
            assert_eq!(source.kind, SourceKind::Unknown);
            assert_eq!(source.path, Some(fixture_path("/synthetic/hint")));
        }
    }

    #[cfg(unix)]
    fn executable(path: &Path, bytes: &[u8]) {
        use std::os::unix::fs::PermissionsExt;
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    // TEST-ONLY capability bypass: a self-created shell fixture, never a real
    // installed agent. Product constructors accept only verified native magic.
    #[cfg(unix)]
    fn fake(root: &Path, body: &str) -> NativeExecutable {
        let path = root.join("fake-owned-fixture");
        let script = format!("#!/bin/sh\n[ \"$#\" = 2 ] && [ \"$1\" = inspect ] && [ \"$2\" = --json ] || exit 91\n{body}\n");
        executable(&path, script.as_bytes());
        let (file, stamp) = open_regular(&path).unwrap();
        NativeExecutable { path, file, stamp }
    }

    #[cfg(unix)]
    #[test]
    fn first_active_path_shadows_later_native_and_no_match_does_not_scan_elsewhere() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let first = root.join("first"); let second = root.join("second");
        fs::create_dir(&first).unwrap(); fs::create_dir(&second).unwrap();
        executable(&first.join("grok"), b"#!/bin/sh\nexit 99\n");
        executable(&second.join("grok-native"), b"\x7fELFsynthetic-not-executed");
        std::os::unix::fs::symlink(second.join("grok-native"), second.join("grok")).unwrap();
        let path = std::env::join_paths([&first, &second]).unwrap();
        assert!(matches!(resolve_native(Some(&path), &root), Err(Diagnostic::NativeIdentityUnknown)));
        assert!(matches!(resolve_native(None, &root), Err(Diagnostic::CliUnavailable)));
        assert!(matches!(resolve_native(Some(OsStr::new("missing")), &root), Err(Diagnostic::CliUnavailable)));
        // Package labels and native magic alone cannot authorize auto-start.
        assert!(matches!(resolve_native(Some(second.as_os_str()), &root), Err(Diagnostic::NativeIdentityUnknown)));
        let expected = format!("{:x}", Sha256::digest(b"\x7fELFsynthetic-not-executed"));
        let found = resolve_native_with_digest(Some(second.as_os_str()), &root, &expected).ok().unwrap();
        assert_eq!(found.path, second.join("grok-native"));
        executable(&second.join("grok-native"), b"#!/bin/sh\nexit 98\n");
        assert!(matches!(resolve_native(Some(second.as_os_str()), &root), Err(Diagnostic::NativeIdentityUnknown)));
    }

    #[cfg(unix)]
    #[test]
    fn official_package_sibling_identity_is_bounded_and_never_executes_wrapper() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap(); let bin = root.join("bin");
        fs::create_dir(&bin).unwrap();
        executable(&bin.join("grok"), b"#!/usr/bin/env node\nSECRET\n");
        executable(&bin.join("grok-native"), b"\x7fELFsynthetic-not-executed");
        fs::write(root.join("package.json"), br#"{"name":"@xai-official/grok","bin":{"grok":"bin/grok"}}"#).unwrap();
        let expected = format!("{:x}", Sha256::digest(b"\x7fELFsynthetic-not-executed"));
        assert!(resolve_native(Some(bin.as_os_str()), &root).is_err());
        assert!(resolve_native_with_digest(Some(bin.as_os_str()), &root, &expected).is_ok());
        fs::write(root.join("package.json"), br#"{"name":"other","bin":{"grok":"bin/grok"}}"#).unwrap();
        assert!(resolve_native(Some(bin.as_os_str()), &root).is_err());
        fs::write(root.join("package.json"), vec![b' '; MAX_PACKAGE_JSON + 1]).unwrap();
        assert!(resolve_native(Some(bin.as_os_str()), &root).is_err());
        fs::remove_file(root.join("package.json")).unwrap();
        std::os::unix::fs::symlink(root.join("absent"), root.join("package.json")).unwrap();
        assert!(resolve_native(Some(bin.as_os_str()), &root).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn fake_success_has_fixed_arguments_null_stdin_frozen_roots_and_projection() {
        let temp = tempfile::tempdir().unwrap(); let root = temp.path().canonicalize().unwrap();
        let native = fake(&root, r#"
            [ "$HOME" = "$PWD/home" ] && [ "$GROK_HOME" = "$PWD/custom" ] || exit 92
            if read line; then exit 93; fi
            printf 'SECRET_STDERR' >&2
            printf '{"grokVersion":"1.0.50","cwd":"%s","projectRoot":null,"projectTrusted":false,"skills":[]}' "$PWD"
        "#);
        let output = run_inspect(&native, &root, &root.join("home"), &root.join("custom"), Duration::from_secs(1), MAX_STDOUT).unwrap();
        let catalog = parse_json(&output).unwrap();
        assert_eq!(catalog.cwd, root); assert!(catalog.skills.is_empty());
        assert!(!root.join("home").exists() && !root.join("custom").exists());
    }

    #[cfg(unix)]
    #[test]
    fn fake_nonzero_invalid_json_overflow_and_timeout_return_only_fixed_diagnostics() {
        for (body, limit, expected) in [("printf SECRET; exit 7", 1024, Diagnostic::ProcessFailed),
            ("printf SECRET_NOT_JSON", 1024, Diagnostic::InvalidReport),
            ("while :; do printf SECRET_OVERFLOW; done", 32, Diagnostic::OutputLimit),
            ("/bin/sleep 30", 1024, Diagnostic::TimedOut)]
        {
            let temp = tempfile::tempdir().unwrap(); let root = temp.path().canonicalize().unwrap();
            let native = fake(&root, body); let start = Instant::now();
            let timeout = if expected == Diagnostic::TimedOut { Duration::from_millis(100) }
                else { Duration::from_secs(1) };
            let result = run_inspect(&native, &root, &root, &root, timeout, limit)
                .and_then(|bytes| parse_json(&bytes));
            let error = result.unwrap_err(); assert_eq!(error, expected);
            assert!(!error.message().contains("SECRET"));
            assert!(start.elapsed() < Duration::from_secs(3));
        }
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_on_success_kills_owned_descendants_and_reaps_leader() {
        let temp = tempfile::tempdir().unwrap(); let root = temp.path().canonicalize().unwrap();
        let native = fake(&root, r#"
            (/bin/sleep 1 & : > descendant.ready; wait; printf 'SURVIVED' > escaped) >/dev/null 2>&1 &
            # Establish a fixed subtree for the one-shot group cleanup contract.
            # This fixture does not assert that cleanup drains concurrent forks.
            while [ ! -f descendant.ready ]; do :; done
            printf '%s' "$$" > leader.pid
            printf '{"grokVersion":"1.0.50","cwd":"%s","projectRoot":null,"projectTrusted":false,"skills":[]}' "$PWD"
        "#);
        assert!(run_inspect(&native, &root, &root, &root, Duration::from_secs(1), MAX_STDOUT).is_ok());
        let pid: libc::pid_t = fs::read_to_string(root.join("leader.pid")).unwrap().parse().unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        std::thread::sleep(Duration::from_millis(1200));
        assert!(!root.join("escaped").exists());
    }

    #[cfg(unix)]
    #[test]
    fn raii_drop_cleans_up_early_exit_and_replacement_prevents_spawn() {
        let temp = tempfile::tempdir().unwrap(); let root = temp.path().canonicalize().unwrap();
        let native = fake(&root, "/bin/sleep 30");
        let pid = { let child = spawn_owned(&native, &root, &root, &root).unwrap(); child.child.as_ref().unwrap().id() as libc::pid_t };
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        let replacement = root.join("replacement"); executable(&replacement, b"#!/bin/sh\nexit 0\n");
        fs::rename(replacement, &native.path).unwrap();
        assert!(matches!(spawn_owned(&native, &root, &root, &root), Err(Diagnostic::NativeIdentityUnknown)));
    }

    #[cfg(unix)]
    fn wait_for_owned_exit(child: &mut OwnedChild) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !child.exited().unwrap() {
            assert!(Instant::now() < deadline, "owned fixture did not exit");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[cfg(unix)]
    fn assert_leader_reaped(pid: libc::pid_t) {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT) }, -1);
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ECHILD));
    }

    #[cfg(unix)]
    fn saturated_probe(_: libc::pid_t, _: libc::pid_t) -> Result<bool, Diagnostic> {
        checked_member_count(256, 256, Some(0)).map(|_| false)
    }

    #[cfg(unix)]
    fn failed_pid_info_probe(_: libc::pid_t, _: libc::pid_t) -> Result<bool, Diagnostic> {
        checked_pid_info(0, 128, Some(libc::EIO))
    }

    #[cfg(unix)]
    #[test]
    fn member_probe_errors_are_unknown_not_empty() {
        assert_eq!(checked_member_count(1, 256, Some(0)), Ok(1));
        assert_eq!(checked_member_count(0, 256, Some(libc::ESRCH)), Ok(0));
        for count in [-1, 256, 257] {
            assert_eq!(checked_member_count(count, 256, Some(0)), Err(Diagnostic::ProcessFailed));
        }
        assert_eq!(checked_member_count(0, 256, Some(libc::EIO)), Err(Diagnostic::ProcessFailed));
        assert_eq!(checked_pid_info(128, 128, None), Ok(true));
        assert_eq!(checked_pid_info(0, 128, Some(libc::ESRCH)), Ok(false));
        assert_eq!(checked_pid_info(0, 128, Some(libc::EPERM)), Err(Diagnostic::ProcessFailed));
        assert_eq!(checked_pid_info(64, 128, None), Err(Diagnostic::ProcessFailed));
    }

    #[cfg(unix)]
    #[test]
    fn probe_failures_still_kill_owned_descendants_before_finish_or_drop_reaps() {
        let probes: [fn(libc::pid_t, libc::pid_t) -> Result<bool, Diagnostic>; 2] =
            [saturated_probe, failed_pid_info_probe];
        for probe in probes {
            for use_finish in [true, false] {
                let temp = tempfile::tempdir().unwrap(); let root = temp.path().canonicalize().unwrap();
                let native = fake(&root, r#"
                    (/bin/sleep 1 & : > descendant.ready; wait; printf 'SURVIVED' > escaped) >/dev/null 2>&1 &
                    # Establish a fixed subtree for the one-shot group cleanup contract.
                    # This fixture does not assert that cleanup drains concurrent forks.
                    while [ ! -f descendant.ready ]; do :; done
                    printf 'fixture'
                "#);
                let mut child = spawn_owned(&native, &root, &root, &root).unwrap();
                wait_for_owned_exit(&mut child);
                let pid = child.pgid;
                #[cfg(target_os = "macos")]
                assert!(group_has_live_members(pid, pid).unwrap(), "fixture must have a live descendant before cleanup");
                child.test_hooks.probe = Some(probe);
                if use_finish {
                    assert_eq!(child.finish().unwrap_err(), Diagnostic::ProcessFailed);
                } else {
                    drop(child);
                }
                assert_leader_reaped(pid);
                #[cfg(target_os = "macos")]
                {
                    let deadline = Instant::now() + Duration::from_secs(1);
                    while group_has_live_members(pid, pid).unwrap() {
                        assert!(Instant::now() < deadline, "owned descendant survived cleanup");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
                std::thread::sleep(Duration::from_millis(1200));
                assert!(!root.join("escaped").exists());
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn only_known_empty_group_skips_signal_and_a_denied_attempt_is_never_retried() {
        use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
        type Probe = fn(libc::pid_t, libc::pid_t) -> Result<bool, Diagnostic>;
        for (probe, denied, expected_calls, expected_error) in [
            ((|_, _| Ok(false)) as Probe, false, 0, false),
            ((|_, _| Ok(true)) as Probe, false, 1, false),
            ((|_, _| Ok(true)) as Probe, true, 1, true),
            (saturated_probe as Probe, false, 1, true),
            (failed_pid_info_probe as Probe, true, 1, true),
        ] {
            let temp = tempfile::tempdir().unwrap(); let root = temp.path().canonicalize().unwrap();
            let native = fake(&root, "exit 0");
            let mut child = spawn_owned(&native, &root, &root, &root).unwrap();
            wait_for_owned_exit(&mut child);
            let pid = child.pgid;
            let calls = Arc::new(AtomicUsize::new(0)); let observed = calls.clone();
            child.test_hooks.probe = Some(probe);
            child.test_hooks.signal = Some(Box::new(move |_| {
                observed.fetch_add(1, Ordering::SeqCst);
                if denied { Err(libc::EPERM) } else { Ok(()) }
            }));
            assert_eq!(child.kill_group().is_err(), expected_error);
            drop(child); // Must not retry either a denied or successful signal.
            assert_eq!(calls.load(Ordering::SeqCst), expected_calls);
            assert_leader_reaped(pid);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_zombie_only_group_needs_no_signal() {
        use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
        let temp = tempfile::tempdir().unwrap(); let root = temp.path().canonicalize().unwrap();
        let native = fake(&root, "exit 0");
        let mut child = spawn_owned(&native, &root, &root, &root).unwrap();
        wait_for_owned_exit(&mut child);
        let pid = child.pgid;
        let calls = Arc::new(AtomicUsize::new(0)); let observed = calls.clone();
        child.test_hooks.signal = Some(Box::new(move |_| {
            observed.fetch_add(1, Ordering::SeqCst); Err(libc::EPERM)
        }));
        // Use actual libproc metadata, keeping the exited leader unreaped.
        assert!(child.finish().unwrap().success());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_leader_reaped(pid);
    }

    #[cfg(unix)]
    #[test]
    fn externally_reaped_leader_blocks_signals_on_overflow_timeout_and_nonblocking_failure() {
        use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
        for expected in [Diagnostic::OutputLimit, Diagnostic::TimedOut, Diagnostic::ProcessFailed] {
            let temp = tempfile::tempdir().unwrap(); let root = temp.path().canonicalize().unwrap();
            let native = fake(&root, "printf 'fixture-output'; exit 0");
            let mut child = spawn_owned(&native, &root, &root, &root).unwrap();
            wait_for_owned_exit(&mut child);
            let pid = child.pgid;
            // Simulate an external reaper, leaving Rust Child and its stdout
            // handle present. This fake has no descendants; never signal a
            // potentially reused PGID even if the regression returns.
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
            child.leader_exited = false; // Also cover failure before any poll.
            let calls = Arc::new(AtomicUsize::new(0)); let observed = calls.clone();
            child.test_hooks.signal = Some(Box::new(move |_| {
                observed.fetch_add(1, Ordering::SeqCst); Ok(())
            }));
            child.test_hooks.fail_nonblocking = expected == Diagnostic::ProcessFailed;
            let deadline = Instant::now() + if expected == Diagnostic::TimedOut {
                Duration::ZERO
            } else { Duration::from_secs(2) };
            assert_eq!(collect_inspect(child, deadline, 1).unwrap_err(), expected);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_leader_reaped(pid);
        }
    }
}
