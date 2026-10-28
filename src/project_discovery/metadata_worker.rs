//! Run fixed SQLite metadata projections in a short-lived copy of this binary.
//!
//! Source descriptors retained for POSIX-lock safety belong to this worker,
//! never to the long-lived desktop process. No arbitrary SQL crosses the pipe.
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use super::{codex_projects, vscode, workspaces};

const ARG: &str = "--pleat-internal-project-metadata";
const MAX_REQUEST_BYTES: u64 = 64 * 1024;
const MAX_REPLY_BYTES: usize = 32 * 1024 * 1024;
#[cfg(not(test))]
const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum PathQuery {
    WorkBuddy,
    OpenCode,
    HermesProjects,
    HermesFolders,
    HermesSessions,
    DimAgentSessions,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields, rename_all = "snake_case")]
pub(super) enum Projection {
    Paths { queries: Vec<PathQuery> },
    CodexProjects,
    WorkBuddyProjects { scope: workspaces::WorkBuddyScope },
    OpenCodeProjects,
    EditorHistory { cursor_native: bool },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    path: PathBuf,
    projection: Projection,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields, rename_all = "snake_case")]
pub(super) enum Reply {
    Paths { paths: Vec<String> },
    CodexProjects { rows: Vec<codex_projects::CatalogProject> },
    OpenCodeProjects { rows: Vec<workspaces::ProjectPathRow> },
    EditorHistory { history: vscode::History, projects: Vec<vscode::NativeProject> },
}

impl Request {
    fn validate(&self) -> Result<()> {
        if !self.path.is_absolute() || self.path.as_os_str().len() > 16 * 1024 {
            bail!("项目元数据路径无效");
        }
        if let Projection::Paths { queries } = &self.projection {
            if queries.is_empty() || queries.len() > 5
                || queries.iter().enumerate().any(|(i, q)| queries[..i].contains(q)) {
                bail!("项目元数据查询无效");
            }
        }
        if let Projection::WorkBuddyProjects { scope } = &self.projection {
            scope.validate()?;
        }
        Ok(())
    }
}

/// Called before CLI parsing, configuration reads, or desktop initialization.
/// The internal mode only emits bounded metadata or a nonzero status.
pub(super) fn entry() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(ARG)) { return None; }
    if args.next().is_some() { return Some(2); }
    let run = || -> Result<()> {
        let mut input = Vec::new();
        std::io::stdin().take(MAX_REQUEST_BYTES + 1).read_to_end(&mut input)?;
        if input.len() as u64 > MAX_REQUEST_BYTES { bail!("项目元数据请求过大"); }
        let request: Request = serde_json::from_slice(&input)?;
        request.validate()?;
        let reply = execute(request)?;
        let mut output = BoundedReply(Vec::new());
        serde_json::to_writer(&mut output, &reply)?;
        std::io::stdout().lock().write_all(&output.0)?;
        Ok(())
    };
    // Never print source paths, SQLite errors, or rows on the error channel.
    Some(if run().is_ok() { 0 } else { 2 })
}

struct BoundedReply(Vec<u8>);
impl Write for BoundedReply {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_REPLY_BYTES.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other("metadata reply limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

fn execute(request: Request) -> Result<Reply> {
    request.validate()?;
    match request.projection {
        Projection::Paths { queries } => Ok(Reply::Paths {
            paths: workspaces::sqlite_paths_local(&request.path, &queries)?,
        }),
        Projection::CodexProjects => Ok(Reply::CodexProjects {
            rows: codex_projects::read_local(&request.path)?,
        }),
        Projection::WorkBuddyProjects { scope } => Ok(Reply::Paths {
            paths: workspaces::workbuddy_paths_local(&request.path, &scope)?,
        }),
        Projection::OpenCodeProjects => Ok(Reply::OpenCodeProjects {
            rows: workspaces::opencode_project_rows_local(&request.path)?,
        }),
        Projection::EditorHistory { cursor_native } => {
            let (history, projects) = vscode::read_history_local(&request.path, cursor_native)?;
            Ok(Reply::EditorHistory { history, projects })
        }
    }
}

pub(super) fn read(path: &Path, projection: Projection) -> Result<Reply> {
    let request = Request { path: path.to_path_buf(), projection };
    request.validate()?;
    // Parser/VFS unit tests use their in-process fixture. Product binaries and
    // integration tests always use the subprocess path exercised below.
    #[cfg(test)]
    { execute(request) }
    #[cfg(not(test))]
    {
        let mut command = Command::new(std::env::current_exe()
            .map_err(|_| anyhow!("项目元数据读取程序不可用"))?);
        command.arg(ARG);
        invoke(command, &serde_json::to_vec(&request)?, TIMEOUT)
    }
}

// Always reap the owned child on errors, timeouts, early returns, and unwind.
struct Worker(Child);
impl Drop for Worker {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) { let _ = self.0.kill(); }
        let _ = self.0.wait();
    }
}

fn invoke(mut command: Command, input: &[u8], timeout: Duration) -> Result<Reply> {
    if input.len() as u64 > MAX_REQUEST_BYTES { bail!("项目元数据请求过大"); }
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
        .env("RUST_BACKTRACE", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut worker = Worker(command.spawn().map_err(|_| anyhow!("项目元数据读取程序不可用"))?);
    let mut stdin = worker.0.stdin.take().ok_or_else(|| anyhow!("项目元数据读取程序不可用"))?;
    let stdout = worker.0.stdout.take().ok_or_else(|| anyhow!("项目元数据读取程序不可用"))?;
    let input = input.to_vec();
    let (sender, receiver) = mpsc::sync_channel(1);
    let io = std::thread::spawn(move || {
        let collect = || -> std::io::Result<Vec<u8>> {
            stdin.write_all(&input)?;
            drop(stdin); // EOF is the request boundary.
            let mut output = Vec::new();
            stdout.take(MAX_REPLY_BYTES as u64 + 1).read_to_end(&mut output)?;
            if output.len() > MAX_REPLY_BYTES {
                return Err(std::io::Error::other("metadata reply limit"));
            }
            Ok(output)
        };
        let _ = sender.send(collect());
    });
    let started = Instant::now();
    let result = loop {
        match worker.0.try_wait() {
            Ok(Some(status)) => {
                if !status.success() { break Err(anyhow!("Agent 项目索引暂不可读")); }
                break match receiver.recv_timeout(Duration::from_millis(200)) {
                    Ok(Ok(bytes)) => serde_json::from_slice(&bytes)
                        .map_err(|_| anyhow!("Agent 项目索引格式无效")),
                    _ => Err(anyhow!("Agent 项目索引暂不可读")),
                };
            }
            Err(_) => break Err(anyhow!("Agent 项目索引暂不可读")),
            Ok(None) => {}
        }
        if started.elapsed() >= timeout {
            break Err(anyhow!("Agent 项目索引读取超时"));
        }
        // An oversized pipe reply must not leave a worker blocked on stdout.
        match receiver.try_recv() {
            Ok(Err(_)) => break Err(anyhow!("Agent 项目索引超过读取上限或不可读")),
            Ok(Ok(bytes)) => {
                // EOF can precede process exit. Finish the bounded wait before
                // trusting success, then parse only this retained response.
                let status = loop {
                    match worker.0.try_wait() {
                        Ok(Some(status)) => break Some(status),
                        Err(_) => break None,
                        Ok(None) if started.elapsed() >= timeout => break None,
                        Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                    }
                };
                break if status.is_some_and(|s| s.success()) {
                    serde_json::from_slice(&bytes).map_err(|_| anyhow!("Agent 项目索引格式无效"))
                } else { Err(anyhow!("Agent 项目索引暂不可读")) };
            }
            Err(mpsc::TryRecvError::Disconnected) => break Err(anyhow!("Agent 项目索引暂不可读")),
            Err(mpsc::TryRecvError::Empty) => {}
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    drop(worker); // Kill/wait closes the pipes before joining the I/O thread.
    let _ = io.join();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unbounded_or_arbitrary_queries_before_execution() {
        let temp = tempfile::tempdir().unwrap();
        assert!(serde_json::from_str::<Request>(r#"{"path":"/x","projection":{"kind":"paths","queries":["SELECT secret FROM chat"]}}"#).is_err());
        assert!(serde_json::from_str::<Request>(r#"{"path":"/x","projection":{"kind":"open_code_projects"},"sql":"DELETE FROM project"}"#).is_err());
        assert!(Request { path: PathBuf::from("relative"), projection: Projection::OpenCodeProjects }.validate().is_err());
        assert!(Request { path: temp.path().join("index.db"), projection: Projection::Paths { queries: vec![] } }.validate().is_err());
        assert!(Request { path: temp.path().join("index.db"), projection: Projection::Paths { queries: vec![PathQuery::WorkBuddy; 2] } }.validate().is_err());
        let mut reply = BoundedReply(vec![0; MAX_REPLY_BYTES]);
        assert!(reply.write_all(b"x").is_err());
    }

    #[test]
    fn codex_project_projection_accepts_only_its_fixed_metadata_request() {
        let temp = tempfile::tempdir().unwrap();
        let raw = serde_json::json!({"path":temp.path().join("state_5.sqlite"),"projection":{"kind":"codex_projects"}}).to_string();
        let request: Request = serde_json::from_str(&raw).unwrap();
        request.validate().unwrap();
        assert!(matches!(request.projection, Projection::CodexProjects));
        assert!(serde_json::from_str::<Request>(
            r#"{"path":"/native/state_5.sqlite","projection":{"kind":"codex_projects"},"sql":"SELECT body FROM messages"}"#,
        ).is_err());
        assert!(Request {
            path: PathBuf::from("relative/state_5.sqlite"),
            projection: Projection::CodexProjects,
        }.validate().is_err());
    }

    #[test]
    fn workbuddy_projection_requires_scope_and_cannot_carry_sql() {
        for json in [
            r#"{"path":"/x","projection":{"kind":"work_buddy_projects"}}"#,
            r#"{"path":"/x","projection":{"kind":"work_buddy_projects","scope":{"current_user_id":"u"}}}"#,
            r#"{"path":"/x","projection":{"kind":"work_buddy_projects","scope":{"current_user_id":"u","assistant_cwd":"/a"},"sql":"SELECT cwd FROM sessions"}}"#,
        ] {
            assert!(serde_json::from_str::<Request>(json).is_err());
        }
        let temp = tempfile::tempdir().unwrap();
        let assistant = temp.path().join("private-assistant");
        for (user, assistant) in [("", assistant.as_path()), (" \t", assistant.as_path()), ("u", Path::new("relative"))] {
            let request = Request { path: temp.path().join("index.db"), projection: Projection::WorkBuddyProjects {
                scope: workspaces::WorkBuddyScope { current_user_id: user.into(), assistant_cwd: assistant.into() },
            }};
            assert!(request.validate().is_err());
        }
        let request = Request { path: temp.path().join("index.db"), projection: Projection::WorkBuddyProjects {
            scope: workspaces::WorkBuddyScope { current_user_id: "private-identity".into(), assistant_cwd: assistant },
        }};
        request.validate().unwrap();
        let debug = format!("{request:?}");
        assert!(!debug.contains("private-identity") && !debug.contains("private-assistant"));
        let encoded = serde_json::to_vec(&request).unwrap();
        serde_json::from_slice::<Request>(&encoded).unwrap().validate().unwrap();
    }

    #[test]
    fn timeout_reaps_only_owned_worker_and_next_invocation_recovers() {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "project_discovery::metadata_worker::tests::worker_timeout_child", "--ignored", "--nocapture"]);
        let started = Instant::now();
        assert!(invoke(command, b"{}", Duration::from_millis(70)).err().unwrap().to_string().contains("超时"));
        assert!(started.elapsed() < Duration::from_secs(2));
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.arg("--list");
        // Invalid protocol is rejected after a clean exit; no stale child or
        // disconnected channel survives to poison a subsequent invocation.
        assert!(invoke(command, b"{}", Duration::from_secs(2)).is_err());
    }

    #[test]
    #[ignore = "Owned child for timeout lifecycle test"]
    fn worker_timeout_child() { std::thread::sleep(Duration::from_secs(5)); }
}
