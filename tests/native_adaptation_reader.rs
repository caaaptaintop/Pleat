//! Headless protocol integration. No target Agent or user configuration starts.
#![cfg(unix)]
use std::{fs, io::Write, os::unix::fs::MetadataExt, path::Path, process::{Command, Stdio}};
fn identity(path: &Path) -> String {
    let m = path.metadata().unwrap(); format!("{}:{}", m.dev(), m.ino())
}
#[test]
fn headless_reader_uses_only_the_bound_owned_directory_and_does_not_initialize_library() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let source = root.join("source");
    fs::create_dir_all(source.join("local-data")).unwrap();
    fs::write(source.join("local-data/ADAPTATION.md"), "OWNED-PROTOCOL-BODY\n").unwrap();
    let home = root.join("unused-app-data");
    let request = serde_json::json!({"schemaVersion":1,"sourceRoot":source,
        "sourceIdentity":identity(&source),"localDataIdentity":identity(&source.join("local-data"))});
    let run = |bytes: &[u8]| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kitter")).arg("--pleat-native-read-adaptation-v1")
            .env("KITTER_HOME", &home).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(bytes).unwrap();
        child.wait_with_output().unwrap()
    };
    let output = run(&serde_json::to_vec(&request).unwrap());
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["text"], "OWNED-PROTOCOL-BODY\n");
    assert_eq!(value["sourceIdentity"], request["sourceIdentity"]);
    assert_eq!(value["localDataIdentity"], request["localDataIdentity"]);
    assert!(value.get("skillText").is_none() && value.get("skillDigest").is_none());
    assert!(!home.exists());
    fs::rename(source.join("local-data"), source.join("retained-original")).unwrap();
    fs::create_dir(source.join("foreign")).unwrap();
    fs::write(source.join("foreign/ADAPTATION.md"), "FOREIGN-ADAPTATION\n").unwrap();
    std::os::unix::fs::symlink(source.join("foreign"), source.join("local-data")).unwrap();
    let rejected = run(&serde_json::to_vec(&request).unwrap());
    assert!(!rejected.status.success() && rejected.stdout.is_empty());
    assert_eq!(rejected.stderr, b"Managed adaptation read was rejected.\n");
    assert!(!home.exists());
    let malformed = run(b"PRIVATE-INVALID-FIXTURE-INPUT");
    assert!(!malformed.status.success() && malformed.stdout.is_empty());
    assert_eq!(malformed.stderr, b"Managed adaptation read was rejected.\n");
}

#[test]
fn optional_skill_proof_rejects_a_partial_result_without_exposing_adaptation() {
    use sha2::{Digest, Sha256};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let source = root.join("source");
    fs::create_dir_all(source.join("local-data")).unwrap();
    let text = "---\nname: owned-proof\ndescription: Owned full document\n---\nFull body\n";
    fs::write(source.join("SKILL.md"), text).unwrap();
    fs::write(source.join("local-data/ADAPTATION.md"), "OWNED-OPTIONAL-PROOF-ADAPTATION\n").unwrap();
    let app = root.join("must-not-initialize-library");
    let mut request = serde_json::json!({"schemaVersion":1,"sourceRoot":source,
        "sourceIdentity":identity(&source),"localDataIdentity":identity(&source.join("local-data")),
        "expectedSkillDigest":format!("{:x}",Sha256::digest(text.as_bytes())),"includeSkillText":true});
    let run = |request: &serde_json::Value| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kitter")).arg("--pleat-native-read-adaptation-v1")
            .env_clear().env("PATH", "/usr/bin:/bin").env("HOME", &root).env("LANG", "C.UTF-8")
            .env("KITTER_HOME", &app).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(&serde_json::to_vec(request).unwrap()).unwrap();
        child.wait_with_output().unwrap()
    };
    let accepted = run(&request);
    assert!(accepted.status.success() && accepted.stderr.is_empty());
    let result: serde_json::Value = serde_json::from_slice(&accepted.stdout).unwrap();
    assert_eq!(result["skillText"], text);
    assert_eq!(result["skillDigest"], request["expectedSkillDigest"]);
    assert_eq!(result["text"], "OWNED-OPTIONAL-PROOF-ADAPTATION\n");
    request["expectedSkillDigest"] = format!("{:x}", Sha256::digest(b"Full body")).into();
    let rejected = run(&request);
    assert!(!rejected.status.success() && rejected.stdout.is_empty());
    assert_eq!(rejected.stderr, b"Managed adaptation read was rejected.\n");
    request["expectedSkillDigest"] = format!("{:x}", Sha256::digest(text.as_bytes())).into();
    request.as_object_mut().unwrap().remove("localDataIdentity");
    let source_only = run(&request);
    assert!(source_only.status.success() && source_only.stderr.is_empty());
    let result: serde_json::Value = serde_json::from_slice(&source_only.stdout).unwrap();
    assert_eq!(result["skillText"], text);
    assert_eq!(result["text"], "");
    assert!(result.get("localDataIdentity").is_none());
    assert!(!app.exists());
}
