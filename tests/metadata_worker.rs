//! Product subprocess regression for metadata descriptor lifetime.
#![cfg(all(target_pointer_width = "64", any(target_os = "macos", target_os = "linux")))]
use std::{io::Write, path::Path, process::{Command, Stdio}};
use rusqlite::Connection;
use serde_json::{Value, json};

fn read_metadata(db: &Path) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kitter"))
        .arg("--pleat-internal-project-metadata")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().unwrap();
    let request = json!({"path": db, "projection": {"kind":"paths", "queries":["work_buddy"]}});
    child.stdin.take().unwrap().write_all(request.to_string().as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn workbuddy_catalog_worker_does_not_require_or_project_sessions() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("workbuddy.db");
    let connection = Connection::open(&db).unwrap();
    connection.execute_batch("CREATE TABLE workspaces(path TEXT PRIMARY KEY,last_opened_at INTEGER NOT NULL);
        INSERT INTO workspaces VALUES('/fixture-catalog',1);
        CREATE TABLE sessions(cwd TEXT,user_id TEXT,title TEXT);
        INSERT INTO sessions VALUES('/fixture-session-only','fixture-current','fixture-private-body');").unwrap();
    drop(connection);
    let before = std::fs::read(&db).unwrap();
    let output = read_metadata(&db);
    assert!(output.status.success() && output.stderr.is_empty());
    let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply, json!({"kind":"paths", "paths":["/fixture-catalog"]}));
    assert_eq!(std::fs::read(&db).unwrap(), before);
    let connection = Connection::open(&db).unwrap();
    connection.execute_batch("DROP TABLE sessions; CREATE VIEW sessions AS SELECT * FROM absent_private_table;").unwrap();
    drop(connection);
    let before = std::fs::read(&db).unwrap();
    let output = read_metadata(&db);
    assert!(output.status.success() && output.stderr.is_empty());
    assert_eq!(serde_json::from_slice::<Value>(&output.stdout).unwrap(), reply);
    assert_eq!(std::fs::read(&db).unwrap(), before);
}

#[test]
fn product_reader_survives_300_native_writer_generations_and_failed_sources() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("native.db");
    let missing = root.path().join("missing.db");
    let before = fd_count();
    let mut initial_source = None;
    for generation in 0..300 {
        let writer = Connection::open(&db).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL;
CREATE TABLE IF NOT EXISTS workspaces(path TEXT PRIMARY KEY,last_opened_at INTEGER NOT NULL);")
            .unwrap();
        writer.execute("INSERT OR REPLACE INTO workspaces VALUES('/owned-project',?1)", [generation]).unwrap();
        let output = read_metadata(&db);
        assert!(output.status.success(), "generation {generation}; metadata worker failed");
        assert!(output.stderr.is_empty());
        let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(reply, json!({"kind":"paths", "paths":["/owned-project"]}));
        // Closing the reader in another process must not drop this writer's
        // process-owned locks, and must not retain its unlinked sidecars.
        writer.execute("UPDATE workspaces SET last_opened_at=?1", [generation + 1]).unwrap();
        drop(writer);
        assert!(!db.with_extension("db-wal").exists());
        assert!(!db.with_extension("db-shm").exists());
        if generation == 0 { initial_source = Some(std::fs::metadata(&db).unwrap().permissions()); }
        if generation % 75 == 0 {
            let failed = read_metadata(&missing);
            assert!(!failed.status.success());
            assert!(failed.stdout.is_empty() && failed.stderr.is_empty());
            assert!(!missing.exists());
        }
    }
    #[cfg(unix)]
    assert!(fd_count() <= before + 1, "parent descriptors must not grow with source inode churn");
    assert_eq!(std::fs::metadata(&db).unwrap().permissions().readonly(), initial_source.unwrap().readonly());
    // No arbitrary SQL or configuration side effects are accepted by the
    // internal mode; bad protocol cannot poison the next valid read.
    let mut child = Command::new(env!("CARGO_BIN_EXE_kitter"))
        .arg("--pleat-internal-project-metadata")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(br#"{"path":"/unread-source","projection":{"kind":"paths","queries":["SELECT * FROM messages"]}}"#).unwrap();
    let rejected = child.wait_with_output().unwrap();
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty() && rejected.stderr.is_empty());
    assert!(read_metadata(&db).status.success());
}

#[cfg(unix)]
fn fd_count() -> usize { std::fs::read_dir("/dev/fd").unwrap().count() }
#[cfg(not(unix))]
fn fd_count() -> usize { 0 }
