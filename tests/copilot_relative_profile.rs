//! Controller-only relative COPILOT_HOME lifecycle. Author/last_modified_by: admin.
//! The controller owns native.lock, native authentication, and catalog observations.
//! Required literal environment: HOME=<case>/home, cwd=<case>/project,
//! COPILOT_HOME=~/profile, PLEAT_COPILOT_PROFILE_CASE=/private/var/tmp/pcp-managed-<16 hex>.
//! Run once per PLEAT_COPILOT_PROFILE_PHASE=install, update, uninstall, in that order.
//! Receipts also reserve failed attempts; use a fresh controller case after failure.
#![cfg(unix)]

use kitter::{agents, project, InstallTarget};
use serde_json::{json, Value};
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

const NAME: &str = "pleat-relative-profile-managed";
const USER: &[u8] = b"OWNED_RELATIVE_USER_SENTINEL\r\n";
const LOCAL: &[u8] = b"OWNED_RELATIVE_LOCAL_SENTINEL\n";
const SKILL_A: &[u8] = b"---\nname: pleat-relative-profile-managed\ndescription: own revisionA\n---\n\nbodyA\n";
const SKILL_B: &[u8] = b"---\nname: pleat-relative-profile-managed\ndescription: own revisionB\n---\n\nbodyB\n";
const ADP_A: &[u8] = b"# Owned relative profile adaptation\n\nadaptationA\n";
const ADP_B: &[u8] = b"# Owned relative profile adaptation\n\nadaptationB\n";
type Check<T> = Result<T, &'static str>;

unsafe extern "C" {
    fn geteuid() -> u32;
}

fn require(ok: bool, code: &'static str) -> Check<()> {
    if ok { Ok(()) } else { Err(code) }
}

fn absent(path: &Path) -> Check<()> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err("PC_EXPECTED_ABSENT"),
    }
}

fn private_dir(path: &Path, uid: u32) -> Check<()> {
    let m = fs::symlink_metadata(path).map_err(|_| "PC_DIRECTORY_METADATA")?;
    require(m.is_dir() && !m.file_type().is_symlink() && m.uid() == uid
        && m.mode() & 0o077 == 0 && m.mode() & 0o700 == 0o700,
        "PC_DIRECTORY_NOT_PRIVATE")?;
    require(fs::canonicalize(path).map_err(|_| "PC_DIRECTORY_CANONICAL")? == path,
        "PC_DIRECTORY_ALIAS")
}

fn make_dir(path: &Path, uid: u32) -> Check<()> {
    DirBuilder::new().mode(0o700).create(path).map_err(|_| "PC_CREATE_DIRECTORY")?;
    private_dir(path, uid)
}

fn owned_file(path: &Path, uid: u32) -> Check<fs::Metadata> {
    let m = fs::symlink_metadata(path).map_err(|_| "PC_FILE_METADATA")?;
    require(m.is_file() && !m.file_type().is_symlink() && m.uid() == uid
        && m.mode() & 0o077 == 0 && m.nlink() == 1 && m.len() <= 65_536,
        "PC_FILE_NOT_PRIVATE")?;
    Ok(m)
}

fn new_file(path: &Path, bytes: &[u8]) -> Check<File> {
    let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600)
        .open(path).map_err(|_| "PC_CREATE_NEW")?;
    f.write_all(bytes).map_err(|_| "PC_WRITE_NEW")?;
    f.sync_all().map_err(|_| "PC_SYNC_NEW")?;
    Ok(f)
}

fn bytes_are(path: &Path, uid: u32, expected: &[u8]) -> Check<()> {
    owned_file(path, uid)?;
    require(fs::read(path).map_err(|_| "PC_READ_BYTES")? == expected, "PC_BYTES_CHANGED")
}

fn identity(m: &fs::Metadata) -> Value {
    json!({"dev":m.dev(), "ino":m.ino(), "mtime":m.mtime(), "mtime_nsec":m.mtime_nsec()})
}

fn file_summary(path: &Path, uid: u32) -> Check<Value> {
    let m = owned_file(path, uid)?;
    let bytes = fs::read(path).map_err(|_| "PC_SUMMARY_READ")?;
    // Fixed fixture bytes only. FNV-1a is a comparison digest, not an authentication claim.
    let digest = bytes.iter().fold(0xcbf29ce484222325_u64,
        |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3));
    let mut summary = identity(&m);
    summary["bytes"] = json!({"len":bytes.len(), "fnv1a64":format!("{digest:016x}")});
    Ok(summary)
}

struct Fixture {
    case: PathBuf,
    home: PathBuf,
    root: PathBuf,
    sources: PathBuf,
    source: PathBuf,
    instructions: PathBuf,
    uid: u32,
}

impl Fixture {
    fn preflight() -> Check<Self> {
        let case_raw = std::env::var("PLEAT_COPILOT_PROFILE_CASE").map_err(|_| "PC_CASE_ENV")?;
        let suffix = case_raw.strip_prefix("/private/var/tmp/pcp-managed-").ok_or("PC_CASE_FORMAT")?;
        require(suffix.len() == 16 && suffix.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "PC_CASE_FORMAT")?;
        let case = PathBuf::from(case_raw);
        let home = case.join("home");
        require(std::env::var_os("HOME").as_deref() == Some(home.as_os_str()), "PC_HOME_ENV")?;
        require(std::env::var_os("COPILOT_HOME").as_deref() == Some(std::ffi::OsStr::new("~/profile")),
            "PC_PROFILE_ENV")?;
        require(std::env::current_dir().map_err(|_| "PC_CWD_READ")? == case.join("project"), "PC_CWD")?;
        // This is the product's authenticated resolver, never a fixture fallback.
        // In particular, failure occurs before any source directory or file write.
        let root = agents::global_target_root(&home, InstallTarget::Copilot)
            .map_err(|_| "PC_NATIVE_ROOT_UNAVAILABLE")?;
        require(root == case.join("project/~/profile/skills"), "PC_NATIVE_ROOT_MISMATCH")?;
        let uid = unsafe { geteuid() };
        for path in [&case, &home, &case.join("project"), &case.join("project/~"),
            &case.join("project/~/profile")] {
            private_dir(path, uid)?;
        }
        match fs::symlink_metadata(&root) {
            Ok(_) => private_dir(&root, uid)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(_) => return Err("PC_ROOT_METADATA"),
        }
        let sources = case.join("sources");
        let source = sources.join(NAME);
        let instructions = case.join("project/~/profile/copilot-instructions.md");
        Ok(Self { case, home, root, sources, source, instructions, uid })
    }

    fn paths(&self) -> Value {
        json!({"case":self.case, "home":self.home, "root":self.root,
            "sources":self.sources, "source":self.source,
            "skill":self.source.join("SKILL.md"), "adaptation":self.source.join("local-data/ADAPTATION.md"),
            "local_data":self.source.join("local-data"), "sentinel":self.source.join("local-data/sentinel"),
            "link":self.root.join(NAME), "instructions":self.instructions})
    }

    fn source_summary(&self, skill: &[u8], adaptation: &[u8]) -> Check<Value> {
        for dir in [&self.sources, &self.source, &self.source.join("local-data")] {
            private_dir(dir, self.uid)?;
        }
        bytes_are(&self.source.join("SKILL.md"), self.uid, skill)?;
        bytes_are(&self.source.join("local-data/ADAPTATION.md"), self.uid, adaptation)?;
        bytes_are(&self.source.join("local-data/sentinel"), self.uid, LOCAL)?;
        Ok(json!({"source_dir":identity(&fs::symlink_metadata(&self.source).map_err(|_| "PC_SOURCE_STAT")?),
            "local_data_dir":identity(&fs::symlink_metadata(self.source.join("local-data")).map_err(|_| "PC_LOCAL_STAT")?),
            "skill":file_summary(&self.source.join("SKILL.md"), self.uid)?,
            "adaptation":file_summary(&self.source.join("local-data/ADAPTATION.md"), self.uid)?,
            "sentinel":file_summary(&self.source.join("local-data/sentinel"), self.uid)?}))
    }

    fn link_summary(&self) -> Check<Value> {
        private_dir(&self.root, self.uid)?;
        let link = self.root.join(NAME);
        let m = fs::symlink_metadata(&link).map_err(|_| "PC_LINK_STAT")?;
        require(m.file_type().is_symlink() && m.uid() == self.uid, "PC_DEFAULT_LINK_REQUIRED")?;
        require(fs::read_link(&link).map_err(|_| "PC_LINK_READ")? == self.source
            && fs::canonicalize(&link).map_err(|_| "PC_LINK_CANONICAL")? == self.source,
            "PC_LINK_TARGET")?;
        Ok(identity(&m))
    }

    fn installed_summary(&self, skill: &[u8], adaptation: &[u8]) -> Check<Value> {
        Ok(json!({"source":self.source_summary(skill, adaptation)?, "link":self.link_summary()?,
            "instructions":file_summary(&self.instructions, self.uid)?}))
    }

    fn read_json(&self, path: &Path) -> Check<Value> {
        owned_file(path, self.uid)?;
        serde_json::from_slice(&fs::read(path).map_err(|_| "PC_JSON_READ")?)
            .map_err(|_| "PC_JSON_PARSE")
    }

    fn previous(&self, phase: &str) -> Check<Value> {
        let receipt = self.read_json(&self.case.join(format!("relative-lifecycle-{phase}.json")))?;
        require(receipt["author"] == "admin" && receipt["last_modified_by"] == "admin"
            && receipt["status"] == "PASS_PUBLIC_API_FIXTURE" && receipt["phase"] == phase
            && receipt["paths"] == self.paths(), "PC_PREVIOUS_RECEIPT")?;
        let state = self.read_json(&self.case.join("relative-profile-state.json"))?;
        require(state["author"] == "admin" && state["last_modified_by"] == "admin"
            && state["paths"] == self.paths() && state[phase] == receipt["snapshot"], "PC_PREVIOUS_STATE")?;
        Ok(state)
    }

    fn receipt(&self, phase: &str) -> Check<File> {
        let pending = json!({"author":"admin", "last_modified_by":"admin", "phase":phase,
            "status":"RESERVED_NOT_COMPLETED", "paths":self.paths(), "native_consumption_verified":false});
        new_file(&self.case.join(format!("relative-lifecycle-{phase}.json")),
            &serde_json::to_vec_pretty(&pending).map_err(|_| "PC_JSON_ENCODE")?)
    }

    fn finish(&self, mut receipt: File, phase: &str, snapshot: Value, preserved: Value) -> Check<()> {
        let report = json!({"author":"admin", "last_modified_by":"admin", "phase":phase,
            "status":"PASS_PUBLIC_API_FIXTURE", "paths":self.paths(), "snapshot":snapshot,
            "expected":match phase {
                "install" => json!({"description":"own revisionA", "body":"bodyA", "adaptation":"adaptationA"}),
                "update" => json!({"description":"own revisionB", "body":"bodyB", "adaptation":"adaptationB"}),
                _ => json!({"installation_absent":true, "retained_description":"own revisionB", "retained_body":"bodyB", "retained_adaptation":"adaptationB"}),
            }, "preserved":preserved, "native_consumption_verified":false});
        receipt.seek(SeekFrom::Start(0)).map_err(|_| "PC_RECEIPT_SEEK")?;
        receipt.set_len(0).map_err(|_| "PC_RECEIPT_TRUNCATE")?;
        receipt.write_all(&serde_json::to_vec_pretty(&report).map_err(|_| "PC_JSON_ENCODE")?)
            .map_err(|_| "PC_RECEIPT_WRITE")?;
        receipt.sync_all().map_err(|_| "PC_RECEIPT_SYNC")
    }
}

fn same_inode(before: &Value, after: &Value) -> Check<()> {
    require(before["dev"] == after["dev"] && before["ino"] == after["ino"], "PC_INODE_CHANGED")
}

fn run() -> Check<()> {
    let phase = std::env::var("PLEAT_COPILOT_PROFILE_PHASE").map_err(|_| "PC_PHASE_ENV")?;
    require(matches!(phase.as_str(), "install" | "update" | "uninstall"), "PC_PHASE_VALUE")?;
    let f = Fixture::preflight()?;
    let state_path = f.case.join("relative-profile-state.json");
    absent(&f.case.join(format!("relative-lifecycle-{phase}.json")))?;
    match phase.as_str() {
        "install" => {
            absent(&state_path)?;
            absent(&f.root.join(NAME))?;
            match fs::symlink_metadata(&f.sources) {
                Ok(_) => private_dir(&f.sources, f.uid)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
                Err(_) => return Err("PC_SOURCES_METADATA"),
            }
            absent(&f.source)?;
            let create_instructions = match fs::symlink_metadata(&f.instructions) {
                Ok(_) => { bytes_are(&f.instructions, f.uid, USER)?; false },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
                Err(_) => return Err("PC_INSTRUCTIONS_METADATA"),
            };
            let receipt = f.receipt(&phase)?;
            // Keep the profile's installation directory private even with a permissive umask.
            if absent(&f.root).is_ok() { make_dir(&f.root, f.uid)?; }
            if absent(&f.sources).is_ok() { make_dir(&f.sources, f.uid)?; }
            make_dir(&f.source, f.uid)?;
            make_dir(&f.source.join("local-data"), f.uid)?;
            new_file(&f.source.join("SKILL.md"), SKILL_A)?;
            new_file(&f.source.join("local-data/ADAPTATION.md"), ADP_A)?;
            new_file(&f.source.join("local-data/sentinel"), LOCAL)?;
            if create_instructions { new_file(&f.instructions, USER)?; }
            let before = f.source_summary(SKILL_A, ADP_A)?;
            project::install_from_path(&f.home, &f.source, NAME, &[InstallTarget::Copilot])
                .map_err(|_| "PC_PUBLIC_INSTALL")?;
            let snapshot = f.installed_summary(SKILL_A, ADP_A)?;
            require(snapshot["source"] == before, "PC_INSTALL_SOURCE_CHANGED")?;
            let instructions = fs::read(&f.instructions).map_err(|_| "PC_INSTRUCTIONS_READ")?;
            require(instructions.starts_with(USER) && instructions != USER, "PC_ADAPTATION_ENTRY_MISSING")?;
            let preserved = json!({"source_bytes_and_identity":true, "local_data_bytes_and_identity":true,
                "user_instruction_prefix":true, "default_directory_link":true});
            let state = json!({"author":"admin", "last_modified_by":"admin", "paths":f.paths(),
                "install":snapshot, "preserved":preserved});
            new_file(&state_path, &serde_json::to_vec_pretty(&state).map_err(|_| "PC_JSON_ENCODE")?)?;
            f.finish(receipt, &phase, snapshot, preserved)
        },
        "update" => {
            let mut state = f.previous("install")?;
            require(state.get("update").is_none(), "PC_UPDATE_ALREADY_RECORDED")?;
            let before = f.installed_summary(SKILL_A, ADP_A)?;
            require(before == state["install"], "PC_INSTALL_STATE_CHANGED")?;
            let receipt = f.receipt(&phase)?;
            // In-place writes to our two files; no source update or explicit writer API.
            fs::write(f.source.join("SKILL.md"), SKILL_B).map_err(|_| "PC_SKILL_UPDATE")?;
            fs::write(f.source.join("local-data/ADAPTATION.md"), ADP_B).map_err(|_| "PC_ADAPTATION_UPDATE")?;
            let after = f.installed_summary(SKILL_B, ADP_B)?;
            for key in ["skill", "adaptation"] { same_inode(&before["source"][key], &after["source"][key])?; }
            for key in ["source_dir", "local_data_dir", "sentinel"] {
                require(before["source"][key] == after["source"][key], "PC_LOCAL_OR_DIRECTORY_CHANGED")?;
            }
            require(before["link"] == after["link"] && before["instructions"] == after["instructions"],
                "PC_LINK_OR_INSTRUCTIONS_CHANGED")?;
            let preserved = json!({"source_file_inodes":true, "source_directory_identity":true,
                "local_data_bytes_and_identity":true, "link_identity":true, "instructions_bytes_and_identity":true});
            state["update"] = after.clone();
            state["preserved"] = preserved.clone();
            let state_before = owned_file(&state_path, f.uid)?;
            fs::write(&state_path, serde_json::to_vec_pretty(&state).map_err(|_| "PC_JSON_ENCODE")?)
                .map_err(|_| "PC_STATE_UPDATE")?;
            same_inode(&identity(&state_before), &identity(&owned_file(&state_path, f.uid)?))?;
            require(f.read_json(&state_path)? == state, "PC_STATE_READBACK")?;
            f.finish(receipt, &phase, after, preserved)
        },
        "uninstall" => {
            let state = f.previous("update")?;
            let before = f.installed_summary(SKILL_B, ADP_B)?;
            require(before == state["update"], "PC_UPDATE_STATE_CHANGED")?;
            let receipt = f.receipt(&phase)?;
            project::uninstall(&f.home, &f.sources, NAME, &[InstallTarget::Copilot])
                .map_err(|_| "PC_PUBLIC_UNINSTALL")?;
            absent(&f.root.join(NAME))?;
            bytes_are(&f.instructions, f.uid, USER)?;
            let source = f.source_summary(SKILL_B, ADP_B)?;
            require(source == before["source"], "PC_UNINSTALL_SOURCE_CHANGED")?;
            require(f.read_json(&state_path)? == state, "PC_UNINSTALL_STATE_CHANGED")?;
            let snapshot = json!({"source":source, "instructions":file_summary(&f.instructions, f.uid)?,
                "installation_absent":true});
            let preserved = json!({"source_B_bytes_and_inodes":true, "adaptation_B_bytes_and_inode":true,
                "source_directory_identity":true, "local_data_bytes_and_identity":true,
                "user_instruction_bytes_restored":true, "adaptation_entry_removed":true});
            f.finish(receipt, &phase, snapshot, preserved)
        },
        _ => Err("PC_PHASE_VALUE"),
    }
}

#[test]
#[ignore = "controller-only: requires the authenticated live native and its existing native.lock"]
fn owned_relative_profile_public_lifecycle() {
    if let Err(code) = run() { panic!("{code}"); }
}
