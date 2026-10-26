//! Headless, descriptor-relative reader for host extensions. It does not open a
//! library, agent configuration, network connection or a desktop window.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{io::{Read, Write}, path::PathBuf};

const FLAG: &str = "--pleat-native-read-adaptation-v1";
const MAX_REQUEST: u64 = 16 * 1024;
const MAX_ADAPTATION: u64 = 64 * 1024;
const MAX_SKILL: u64 = 256 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    schema_version: u32,
    source_root: PathBuf,
    source_identity: String,
    #[serde(default)]
    local_data_identity: Option<String>,
    #[serde(default)]
    expected_skill_digest: Option<String>,
    #[serde(default)]
    include_skill_text: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Response {
    schema_version: u32,
    source_root: PathBuf,
    source_identity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_data_identity: Option<String>,
    text: String,
    digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    skill_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    skill_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    skill_text: Option<String>,
}

/// In-process access to the same bounded FD reader; callers cannot choose a
/// filename. Source-only proof deliberately never opens local-data.
pub(crate) struct SourceDocument {
    pub(crate) text: String,
    digest: String,
    identity: String,
}

pub(crate) fn source_document(root: &std::path::Path, identity: &str) -> Result<SourceDocument> {
    owned_directory(root, identity)?;
    let response = read(&Request {
        schema_version: 1, source_root: root.to_path_buf(), source_identity: identity.into(),
        local_data_identity: None, expected_skill_digest: None, include_skill_text: true,
    })?;
    owned_directory(root, identity)?;
    Ok(SourceDocument {
        text: response.skill_text.ok_or_else(|| anyhow::anyhow!("missing source proof"))?,
        digest: response.skill_digest.ok_or_else(|| anyhow::anyhow!("missing source proof"))?,
        identity: response.skill_identity.ok_or_else(|| anyhow::anyhow!("missing source proof"))?,
    })
}

pub(crate) fn source_local_data_identity(root: &std::path::Path, identity: &str) -> Result<String> {
    owned_directory(root, identity)?;
    let directory = crate::package_fs::Directory::open_anchored(root)?;
    ensure!(directory.identity()? == identity, "source identity mismatch");
    let local = directory.child(std::path::Path::new("local-data"))?;
    let local_identity = local.identity()?;
    owned_directory(&root.join("local-data"), &local_identity)?;
    directory.verify_location(root)?;
    local.verify_location(&root.join("local-data"))?;
    Ok(local_identity)
}

pub(crate) fn adaptation_for_verified_skill(
    root: &std::path::Path, identity: &str, local_identity: &str, skill: &SourceDocument,
) -> Result<String> {
    source_and_adaptation_for_verified_skill(root, identity, local_identity, skill)
        .map(|(_, adaptation)| adaptation)
}

/// Return BOTH complete texts from one descriptor-bound transaction, keeping the
/// first proof's digest/inode gate before ADP opens. This binds the current read;
/// it makes no claim about a native tool's historical file descriptor.
pub(crate) fn source_and_adaptation_for_verified_skill(
    root: &std::path::Path, identity: &str, local_identity: &str, skill: &SourceDocument,
) -> Result<(String, String)> {
    owned_directory(root, identity)?;
    owned_directory(&root.join("local-data"), local_identity)?;
    let response = read_bound(&Request {
        schema_version: 1, source_root: root.to_path_buf(), source_identity: identity.into(),
        local_data_identity: Some(local_identity.into()),
        expected_skill_digest: Some(skill.digest.clone()), include_skill_text: true,
    }, Some(&skill.identity))?;
    ensure!(response.skill_identity.as_deref() == Some(skill.identity.as_str()), "skill identity changed");
    owned_directory(root, identity)?;
    owned_directory(&root.join("local-data"), local_identity)?;
    let source = response.skill_text.ok_or_else(|| anyhow::anyhow!("missing source proof"))?;
    Ok((source, response.text))
}

fn owned_directory(path: &std::path::Path, identity: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let directory = crate::package_fs::Directory::open_anchored(path)?;
        let metadata = std::fs::symlink_metadata(path)?;
        ensure!(metadata.is_dir() && metadata.uid() == unsafe { libc::geteuid() }
            && format!("{}:{}", metadata.dev(), metadata.ino()) == identity
            && directory.identity()? == identity, "unowned or changed directory");
        directory.verify_location(path)?;
        Ok(())
    }
    #[cfg(not(unix))]
    { let _ = (path, identity); anyhow::bail!("descriptor-relative reader unavailable") }
}

/// Called before normal CLI parsing and before the desktop runtime starts.
pub(crate) fn run_if_requested() {
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new(FLAG)) { return; }
    let result = (|| -> Result<()> {
        ensure!(std::env::args_os().count() == 2, "invalid reader arguments");
        let mut bytes = Vec::new();
        std::io::stdin().lock().take(MAX_REQUEST + 1).read_to_end(&mut bytes)?;
        ensure!(bytes.len() as u64 <= MAX_REQUEST, "reader input exceeds bound");
        let request = serde_json::from_slice::<Request>(&bytes)?;
        let response = read(&request)?;
        let mut stdout = std::io::stdout().lock();
        serde_json::to_writer(&mut stdout, &response)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
        Ok(())
    })();
    if result.is_err() {
        // Never emit paths, input data, adaptation text or underlying OS errors.
        let _ = std::io::stderr().write_all(b"Managed adaptation read was rejected.\n");
    }
    std::process::exit(if result.is_ok() { 0 } else { 1 });
}

#[cfg(unix)]
fn revision(metadata: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!("{}:{}:{}:{}:{}:{}:{}:{}", metadata.dev(), metadata.ino(), metadata.len(),
        metadata.mtime(), metadata.mtime_nsec(), metadata.ctime(), metadata.ctime_nsec(), metadata.nlink())
}

#[cfg(unix)]
struct SkillProof {
    file: std::fs::File,
    revision: String,
    digest: String,
    identity: String,
    text: Option<String>,
}

#[cfg(unix)]
fn bind_skill(root: &crate::package_fs::Directory, request: &Request) -> Result<Option<SkillProof>> {
    use std::{os::unix::fs::MetadataExt, path::Path};
    use sha2::{Digest, Sha256};
    if !request.include_skill_text && request.expected_skill_digest.is_none() { return Ok(None); }
    if let Some(expected) = &request.expected_skill_digest {
        ensure!(expected.len() == 64 && expected.bytes().all(|b| b.is_ascii_digit()
            || (b'a'..=b'f').contains(&b)), "invalid expected skill digest");
    }
    // Only this fixed child of the already-bound source FD is readable. No
    // caller-supplied filename and no source document is written or initialized.
    let mut file = root.read(Path::new("SKILL.md"))?;
    let before = file.metadata()?;
    ensure!(before.is_file() && before.nlink() == 1 && before.len() <= MAX_SKILL, "invalid skill file");
    let observed = revision(&before);
    let mut bytes = Vec::new();
    (&mut file).take(MAX_SKILL + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_SKILL && revision(&file.metadata()?) == observed,
        "skill changed while reading");
    let digest = format!("{:x}", Sha256::digest(&bytes));
    ensure!(request.expected_skill_digest.as_ref().is_none_or(|expected| expected == &digest),
        "native skill text does not match source");
    let text = String::from_utf8(bytes)?;
    ensure!(!text.contains('\0'), "invalid skill text");
    Ok(Some(SkillProof { file, revision: observed, digest,
        identity: format!("{}:{}", before.dev(), before.ino()),
        text: request.include_skill_text.then_some(text) }))
}

fn read(request: &Request) -> Result<Response> {
    read_bound(request, None)
}

#[cfg(unix)]
fn read_bound(request: &Request, expected_skill_identity: Option<&str>) -> Result<Response> {
    use std::{os::unix::fs::MetadataExt, path::Path};
    use sha2::{Digest, Sha256};
    use crate::package_fs::Directory;
    ensure!(request.schema_version == 1 && request.source_root.is_absolute()
        && request.source_root.to_str().is_some_and(|p| !p.chars().any(char::is_control))
        && request.source_root.canonicalize()? == request.source_root, "invalid source root");
    ensure!(request.local_data_identity.is_some() || request.include_skill_text,
        "local data identity is required for adaptation reads");
    let root = Directory::open_anchored(&request.source_root)?;
    ensure!(root.identity()? == request.source_identity, "source identity mismatch");
    let skill = bind_skill(&root, request)?;
    ensure!(expected_skill_identity.is_none_or(|identity|
        skill.as_ref().is_some_and(|proof| proof.identity == identity)), "skill identity changed");
    let bytes = if let Some(expected_local_identity) = &request.local_data_identity {
    #[cfg(test)]
    BEFORE_LOCAL_OPEN.with(|hook| if let Some(callback) = hook.borrow_mut().take() { callback(); });
    let local = root.child(Path::new("local-data"))?;
    ensure!(local.identity()? == *expected_local_identity, "local data identity mismatch");
    root.verify_location(&request.source_root)?;
    let local_path = request.source_root.join("local-data");
    local.verify_location(&local_path)?;
    #[cfg(test)]
    AFTER_LOCAL_BIND.with(|hook| if let Some(callback) = hook.borrow_mut().take() { callback(); });
    // Both opens below are openat on the SAME directory FD, never a reconstructed
    // path through a replaceable parent. Directory::read rejects symlinks/FIFOs.
    let name = Path::new("ADAPTATION.md");
    let mut file = local.read(name)?;
    let before = file.metadata()?;
    ensure!(before.is_file() && before.nlink() == 1 && before.len() <= MAX_ADAPTATION, "invalid adaptation file");
    let observed = revision(&before);
    let mut bytes = Vec::new();
    (&mut file).take(MAX_ADAPTATION + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_ADAPTATION && revision(&file.metadata()?) == observed,
        "adaptation changed while reading");
    #[cfg(test)]
    AFTER_FILE_READ.with(|hook| if let Some(callback) = hook.borrow_mut().take() { callback(); });
    ensure!(revision(&local.read(name)?.metadata()?) == observed, "adaptation entry changed");
    root.verify_location(&request.source_root)?;
    local.verify_location(&local_path)?;
    ensure!(root.identity()? == request.source_identity && local.identity()? == *expected_local_identity,
        "source binding changed");
    bytes
    } else {
        // An explicit source-document read without a local-data identity never
        // opens local-data and never returns adaptation content, even if present.
        Vec::new()
    };
    if let Some(proof) = &skill {
        ensure!(revision(&proof.file.metadata()?) == proof.revision
            && revision(&root.read(Path::new("SKILL.md"))?.metadata()?) == proof.revision,
            "skill entry changed during adaptation read");
        root.verify_location(&request.source_root)?;
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let text = String::from_utf8(bytes)?;
    ensure!(!text.contains('\0'), "invalid adaptation text");
    Ok(Response { schema_version: 1, source_root: request.source_root.clone(),
        source_identity: request.source_identity.clone(), local_data_identity: request.local_data_identity.clone(), text, digest,
        skill_digest: skill.as_ref().map(|proof| proof.digest.clone()),
        skill_identity: skill.as_ref().map(|proof| proof.identity.clone()),
        skill_text: skill.and_then(|proof| proof.text) })
}

#[cfg(not(unix))]
fn read_bound(_: &Request, _: Option<&str>) -> Result<Response> {
    anyhow::bail!("descriptor-relative adaptation reader is unavailable on this platform")
}

#[cfg(all(test, unix))]
thread_local! {
    static BEFORE_LOCAL_OPEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static AFTER_LOCAL_BIND: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static AFTER_FILE_READ: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    fn fixture() -> (tempfile::TempDir, Request) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir(root.join("local-data")).unwrap();
        fs::write(root.join("local-data/ADAPTATION.md"), "OWNED-ADAPTATION").unwrap();
        let request = Request { schema_version: 1,
            source_identity: crate::library::filesystem_identity(&root).unwrap(),
            local_data_identity: Some(crate::library::filesystem_identity(&root.join("local-data")).unwrap()), source_root: root,
            expected_skill_digest: None, include_skill_text: false };
        (temp, request)
    }
    #[test]
    fn hook_wrapper_source_proof_does_not_open_local_data() {
        let (_temp, request) = fixture();
        fs::write(request.source_root.join("SKILL.md"), "Full source\n").unwrap();
        BEFORE_LOCAL_OPEN.with(|hook| *hook.borrow_mut() = Some(Box::new(|| panic!("ADP opened before source proof"))));
        let skill = source_document(&request.source_root, &request.source_identity).unwrap();
        assert_eq!(skill.text, "Full source\n");
        BEFORE_LOCAL_OPEN.with(|hook| assert!(hook.borrow_mut().take().is_some()));
    }
    #[test]
    fn pair_wrapper_returns_both_transaction_texts_and_keeps_legacy_projection() {
        let (_temp, request) = fixture();
        let source = "Full source\r\n\r\n";
        let adaptation = "完整ADP\n\n";
        fs::write(request.source_root.join("SKILL.md"), source).unwrap();
        fs::write(request.source_root.join("local-data/ADAPTATION.md"), adaptation).unwrap();
        let mut first_proof = source_document(&request.source_root, &request.source_identity).unwrap();
        // Only digest/inode bind the second transaction. An old cached String
        // must never become that transaction's returned source text.
        first_proof.text = "STALE-FIRST-PROOF-TEXT".into();
        let local_identity = request.local_data_identity.as_deref().unwrap();
        assert_eq!(source_and_adaptation_for_verified_skill(&request.source_root,
            &request.source_identity, local_identity, &first_proof).unwrap(),
            (source.to_owned(), adaptation.to_owned()));
        assert_eq!(adaptation_for_verified_skill(&request.source_root,
            &request.source_identity, local_identity, &first_proof).unwrap(), adaptation);
    }
    #[test]
    fn pair_wrapper_rejects_same_byte_inode_change_after_proof_with_zero_adaptation_opens() {
        let (_temp, request) = fixture();
        let source = "Full source\n";
        fs::write(request.source_root.join("SKILL.md"), source).unwrap();
        let first_proof = source_document(&request.source_root, &request.source_identity).unwrap();
        let _held = fs::File::open(request.source_root.join("SKILL.md")).unwrap();
        fs::write(request.source_root.join("next"), source).unwrap();
        fs::rename(request.source_root.join("next"), request.source_root.join("SKILL.md")).unwrap();
        let opens = std::rc::Rc::new(std::cell::Cell::new(0));
        let observed = opens.clone();
        BEFORE_LOCAL_OPEN.with(|hook| *hook.borrow_mut() = Some(Box::new(move || observed.set(observed.get() + 1))));
        assert!(source_and_adaptation_for_verified_skill(&request.source_root, &request.source_identity,
            request.local_data_identity.as_deref().unwrap(), &first_proof).is_err());
        assert_eq!(opens.get(), 0);
        BEFORE_LOCAL_OPEN.with(|hook| assert!(hook.borrow_mut().take().is_some()));
    }
    #[test]
    fn hook_wrapper_rejects_stale_digest_and_inode_before_any_adaptation_open() {
        let (_temp, request) = fixture();
        fs::write(request.source_root.join("SKILL.md"), "Full source\n").unwrap();
        let skill = source_document(&request.source_root, &request.source_identity).unwrap();
        // Keep the original inode alive across both replacements in this test.
        let _held = fs::File::open(request.source_root.join("SKILL.md")).unwrap();
        for replacement in ["Changed source\n", "Full source\n"] {
            fs::write(request.source_root.join("next"), replacement).unwrap();
            fs::rename(request.source_root.join("next"), request.source_root.join("SKILL.md")).unwrap();
            BEFORE_LOCAL_OPEN.with(|hook| *hook.borrow_mut() = Some(Box::new(|| panic!("ADP opened for stale source"))));
            assert!(adaptation_for_verified_skill(&request.source_root, &request.source_identity,
                request.local_data_identity.as_deref().unwrap(), &skill).is_err());
            BEFORE_LOCAL_OPEN.with(|hook| assert!(hook.borrow_mut().take().is_some()));
        }
    }
    #[test]
    fn hook_wrapper_inherits_fd_parent_swap_and_file_revision_protection() {
        let (_temp, request) = fixture();
        fs::write(request.source_root.join("SKILL.md"), "Full source\n").unwrap();
        let skill = source_document(&request.source_root, &request.source_identity).unwrap();
        let root = request.source_root.clone();
        fs::create_dir(root.join("foreign")).unwrap();
        fs::write(root.join("foreign/ADAPTATION.md"), "FOREIGN").unwrap();
        let moved = root.clone();
        AFTER_LOCAL_BIND.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            fs::rename(moved.join("local-data"), moved.join("held-original")).unwrap();
            std::os::unix::fs::symlink(moved.join("foreign"), moved.join("local-data")).unwrap();
        })));
        let restored = root.clone();
        AFTER_FILE_READ.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            fs::rename(restored.join("local-data"), restored.join("held-alias")).unwrap();
            fs::rename(restored.join("held-original"), restored.join("local-data")).unwrap();
        })));
        assert_eq!(adaptation_for_verified_skill(&request.source_root, &request.source_identity,
            request.local_data_identity.as_deref().unwrap(), &skill).unwrap(), "OWNED-ADAPTATION");
        AFTER_FILE_READ.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            fs::write(root.join("local-data/next"), "replacement").unwrap();
            fs::rename(root.join("local-data/next"), root.join("local-data/ADAPTATION.md")).unwrap();
        })));
        assert!(adaptation_for_verified_skill(&request.source_root, &request.source_identity,
            request.local_data_identity.as_deref().unwrap(), &skill).is_err());
    }
    #[test]
    fn bounded_fd_read_and_atomic_update_keep_the_bound_directory() {
        let (_temp, request) = fixture();
        assert_eq!(read(&request).unwrap().text, "OWNED-ADAPTATION");
        fs::write(request.source_root.join("local-data/next"), "OWNED-V2").unwrap();
        fs::rename(request.source_root.join("local-data/next"), request.source_root.join("local-data/ADAPTATION.md")).unwrap();
        assert_eq!(read(&request).unwrap().text, "OWNED-V2");
    }
    #[test]
    fn legacy_request_does_not_require_or_expose_a_skill_document() {
        let (_temp, request) = fixture();
        let value = serde_json::to_value(read(&request).unwrap()).unwrap();
        assert!(value.get("skillText").is_none() && value.get("skillDigest").is_none()
            && value.get("skillIdentity").is_none());
    }
    #[test]
    fn native_whole_document_digest_and_explicit_text_are_bound_to_the_source() {
        use sha2::{Digest, Sha256};
        let (_temp, mut request) = fixture();
        let text = "---\nname: owned\ndescription: Owned probe\n---\nComplete skill text\n";
        fs::write(request.source_root.join("SKILL.md"), text).unwrap();
        request.expected_skill_digest = Some(format!("{:x}", Sha256::digest(text.as_bytes())));
        request.include_skill_text = true;
        let response = read(&request).unwrap();
        assert_eq!(response.skill_text.as_deref(), Some(text));
        assert_eq!(response.skill_digest, request.expected_skill_digest);
        assert_eq!(response.skill_identity, Some(crate::library::filesystem_identity(&request.source_root.join("SKILL.md")).unwrap()));
        request.expected_skill_digest = Some(format!("{:x}", Sha256::digest(b"Skill instructions were already served whole earlier.")));
        assert!(read(&request).is_err());
        request.expected_skill_digest = Some("not-a-digest".into());
        assert!(read(&request).is_err());
    }
    #[test]
    fn explicit_source_only_read_never_reads_adaptation_or_falls_back_after_bad_identity() {
        use sha2::{Digest, Sha256};
        let (_temp, mut request) = fixture();
        fs::write(request.source_root.join("SKILL.md"), "Owned source-only skill").unwrap();
        request.include_skill_text = true;
        request.local_data_identity = None;
        let response = read(&request).unwrap();
        assert_eq!(response.skill_text.as_deref(), Some("Owned source-only skill"));
        assert!(response.text.is_empty() && response.local_data_identity.is_none());
        assert_eq!(response.digest, format!("{:x}", Sha256::digest(b"")));
        request.local_data_identity = Some("0:0".into());
        assert!(read(&request).is_err());
        request.local_data_identity = None;
        fs::rename(request.source_root.join("local-data"), request.source_root.join("retained-local-data")).unwrap();
        assert_eq!(read(&request).unwrap().skill_text.as_deref(), Some("Owned source-only skill"));
        request.include_skill_text = false;
        assert!(read(&request).is_err());
    }
    #[test]
    fn skill_entry_replacement_during_the_same_adaptation_transaction_is_rejected() {
        let (_temp, mut request) = fixture();
        fs::write(request.source_root.join("SKILL.md"), "Owned complete skill").unwrap();
        request.include_skill_text = true;
        let root = request.source_root.clone();
        AFTER_FILE_READ.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            fs::write(root.join("replacement-skill"), "Foreign replacement skill").unwrap();
            fs::rename(root.join("replacement-skill"), root.join("SKILL.md")).unwrap();
        })));
        assert!(read(&request).is_err());
    }
    #[test]
    fn skill_aliases_hardlinks_invalid_utf8_and_size_overflow_are_rejected() {
        let (_temp, mut request) = fixture();
        request.include_skill_text = true;
        let skill = request.source_root.join("SKILL.md");
        let foreign = request.source_root.join("foreign-skill");
        fs::write(&foreign, "Owned alias fixture").unwrap();
        std::os::unix::fs::symlink(&foreign, &skill).unwrap();
        assert!(read(&request).is_err());
        fs::remove_file(&skill).unwrap();
        fs::hard_link(&foreign, &skill).unwrap();
        assert!(read(&request).is_err());
        fs::write(request.source_root.join("next-skill"), [0xff]).unwrap();
        fs::rename(request.source_root.join("next-skill"), &skill).unwrap();
        assert!(read(&request).is_err());
        fs::write(&skill, "Owned\0invalid").unwrap();
        assert!(read(&request).is_err());
        fs::write(&skill, vec![b'x'; MAX_SKILL as usize + 1]).unwrap();
        assert!(read(&request).is_err());
    }
    #[test]
    fn foreign_parent_before_open_is_rejected_without_following() {
        let (_temp, request) = fixture();
        let root = request.source_root.clone();
        fs::create_dir(root.join("foreign")).unwrap();
        fs::write(root.join("foreign/ADAPTATION.md"), "FOREIGN-ADAPTATION").unwrap();
        BEFORE_LOCAL_OPEN.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            fs::rename(root.join("local-data"), root.join("held-original")).unwrap();
            std::os::unix::fs::symlink(root.join("foreign"), root.join("local-data")).unwrap();
        })));
        assert!(read(&request).is_err());
    }
    #[test]
    fn swapped_and_restored_parent_cannot_redirect_the_open_file_descriptor() {
        let (_temp, request) = fixture();
        let root = request.source_root.clone();
        fs::create_dir(root.join("foreign")).unwrap();
        fs::write(root.join("foreign/ADAPTATION.md"), "FOREIGN-ADAPTATION").unwrap();
        let moved = root.clone();
        AFTER_LOCAL_BIND.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            fs::rename(moved.join("local-data"), moved.join("held-original")).unwrap();
            std::os::unix::fs::symlink(moved.join("foreign"), moved.join("local-data")).unwrap();
        })));
        AFTER_FILE_READ.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            fs::rename(root.join("local-data"), root.join("held-alias")).unwrap();
            fs::rename(root.join("held-original"), root.join("local-data")).unwrap();
        })));
        assert_eq!(read(&request).unwrap().text, "OWNED-ADAPTATION");
    }
    #[test]
    fn file_replacement_during_read_is_rejected() {
        let (_temp, request) = fixture();
        let root = request.source_root.clone();
        AFTER_FILE_READ.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            fs::write(root.join("local-data/next"), "Changed after read").unwrap();
            fs::rename(root.join("local-data/next"), root.join("local-data/ADAPTATION.md")).unwrap();
        })));
        assert!(read(&request).is_err());
    }
    #[test]
    fn links_wrong_identities_and_oversized_data_are_rejected() {
        let (_temp, mut request) = fixture();
        request.local_data_identity = Some("0:0".into());
        assert!(read(&request).is_err());
        request.local_data_identity = Some(crate::library::filesystem_identity(&request.source_root.join("local-data")).unwrap());
        fs::hard_link(request.source_root.join("local-data/ADAPTATION.md"), request.source_root.join("retained-hardlink")).unwrap();
        assert!(read(&request).is_err());
        fs::write(request.source_root.join("local-data/new-large"), vec![b'x'; MAX_ADAPTATION as usize + 1]).unwrap();
        fs::rename(request.source_root.join("local-data/new-large"), request.source_root.join("local-data/ADAPTATION.md")).unwrap();
        assert!(read(&request).is_err());
    }
}
