//! Source-bound Grok PostToolUse replacement. Register before any library/config/GUI
//! initialization. The host owns registration, trust and process lifetime.
//! Wire evidence: grok-native-hook-wire-source-increment-1.json (source 1.0.45);
//! compatibility with the installed 1.0.50 binary still requires a native probe.
use anyhow::{Result, ensure};
use serde::{Deserialize, Deserializer, Serialize};
use std::{ffi::OsStr, io::{Read, Write}, path::{Component, Path, PathBuf}};

const FLAG: &str = "--pleat-native-grok-adaptation-v1";
// Includes both 128-KiB native payloads, their snake mirrors and envelope metadata.
const MAX_EVENT: u64 = 1024 * 1024;
const MAX_NATIVE_PAYLOAD: usize = 128 * 1024;
// Public, stable probe discriminator; never a prompt/provider nonce. Both body
// projections carry this prefix and the same two complete bound documents.
pub(crate) const SOURCE_BOUND_READ_PREFIX: &str = "Pleat source-bound skill read\n";
// Fixed official output.rs:655-674 projects nonempty FileContent.content exactly;
// post_tool_use_delivery.rs:128-130 clips it before escape_reminder_tags.
// Source 1.0.45 only; target 1.0.50 still needs the controller's native probe.
const MAX_COMBINED_CHARS: usize = 64 * 1024;

/// Presence is separate from value: an explicit null mirror of a required field
/// is invalid. Do not use serde aliases (the official event contains both keys).
#[derive(Debug, PartialEq)]
struct Mirror<T>(Option<T>);
impl<T> Default for Mirror<T> {
    fn default() -> Self { Self(None) }
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Mirror<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        T::deserialize(deserializer).map(|value| Self(Some(value)))
    }
}
impl<T: PartialEq> Mirror<T> {
    fn agrees(&self, value: &T) -> bool { self.0.as_ref().is_none_or(|mirror| mirror == value) }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Event {
    hook_event_name: String,
    #[serde(default, rename = "hook_event_name")]
    event_mirror: Mirror<String>,
    session_id: String,
    #[serde(default, rename = "session_id")]
    session_mirror: Mirror<String>,
    // Parsed as metadata only. Never used to resolve paths or open transcripts.
    cwd: String,
    workspace_root: String,
    timestamp: String,
    #[serde(default)]
    transcript_path: Option<String>,
    #[serde(default, rename = "transcript_path")]
    transcript_mirror: Mirror<Option<String>>,
    #[serde(default)]
    client_identifier: Option<String>,
    #[serde(default)]
    prompt_id: Option<String>,
    #[serde(default)]
    permission_mode: Option<String>,
    #[serde(default, rename = "permission_mode")]
    permission_mirror: Mirror<Option<String>>,
    tool_name: String,
    #[serde(default, rename = "tool_name")]
    tool_mirror: Mirror<String>,
    tool_use_id: String,
    #[serde(default, rename = "tool_use_id")]
    use_id_mirror: Mirror<String>,
    tool_input: ReadInput,
    #[serde(default, rename = "tool_input")]
    input_mirror: Mirror<ReadInput>,
    tool_result: ReadResult,
    #[serde(default, rename = "tool_response")]
    result_mirror: Mirror<ReadResult>,
    tool_input_truncated: bool,
    tool_result_truncated: bool,
    is_backgrounded: bool,
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default, rename = "duration_ms")]
    duration_mirror: Mirror<Option<u64>>,
    #[serde(default)]
    subagent_type: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    target_file: String,
    #[serde(default)]
    offset: Option<u64>,
    #[serde(default)]
    limit: Option<u64>,
    #[serde(default)]
    pages: Option<String>,
    #[serde(default)]
    format: Option<String>,
}

// An exact success shape, not an untagged enum or a flattened map: mixing any
// logical error, future variant, success/is_error flag or wrapper is rejected.
#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ReadResult {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "FileContent")]
    file: FileContent,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct FileContent {
    content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_concise: Option<String>,
    absolute_path: String,
    // The official serializer always includes offset (null is meaningful).
    #[serde(deserialize_with = "required_option")]
    offset: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit: Option<u64>,
    raw_output: String,
    total_lines: u64,
    // Known optional output metadata, never interpreted as a file or variant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    extracted_images: Vec<serde_json::Value>,
}

fn required_option<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Option<u64>, D::Error> {
    Option::<u64>::deserialize(d)
}

impl Event {
    fn validate(&self) -> Result<()> {
        ensure!(self.hook_event_name == "post_tool_use"
            && self.event_mirror.agrees(&"PostToolUse".to_owned())
            && self.tool_name == "read_file"
            && !self.tool_input_truncated && !self.tool_result_truncated && !self.is_backgrounded,
            "event is not a foreground successful read");
        ensure!(self.session_mirror.agrees(&self.session_id)
            && self.transcript_mirror.agrees(&self.transcript_path)
            && self.permission_mirror.agrees(&self.permission_mode)
            && self.tool_mirror.agrees(&self.tool_name)
            && self.use_id_mirror.agrees(&self.tool_use_id)
            && self.input_mirror.agrees(&self.tool_input)
            && self.result_mirror.agrees(&self.tool_result)
            && self.duration_mirror.agrees(&self.duration_ms), "wire mirrors disagree");
        ensure!(self.tool_result.kind == "ReadFile" && !self.tool_result.file.content.is_empty(),
            "unsupported tool result");
        // These known envelope fields have no authority over the file selection.
        let _ = (&self.cwd, &self.workspace_root, &self.timestamp, &self.client_identifier,
            &self.prompt_id, &self.subagent_type);
        Ok(())
    }
}

fn safe_absolute(path: &Path) -> bool {
    path.is_absolute() && path.to_str().is_some_and(|text| !text.chars().any(char::is_control)
        && text.len() <= 16 * 1024
        && !text.split('/').any(|part| part == "." || part == ".."))
        && path.components().all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
}

fn valid_identity(identity: &str) -> bool {
    identity.split_once(':').is_some_and(|(dev, ino)| [dev, ino].into_iter().all(|part|
        !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()) && part.parse::<u64>().is_ok()))
}

/// A single installation directory and the registered SOURCE DIRECTORY dev:ino.
/// Resolve its current target each invocation; no old canonical library path is
/// persisted, so an inode-preserving library move + symlink retarget still works.
struct Installation {
    path: PathBuf,
    root: PathBuf,
    source_identity: String,
    entry_identity: String,
    raw_target: Option<PathBuf>,
}

impl Installation {
    #[cfg(unix)]
    fn bind(path: &Path, source_identity: &str) -> Result<Self> {
        use std::os::unix::fs::MetadataExt;
        ensure!(safe_absolute(path) && valid_identity(source_identity), "invalid installation arguments");
        let entry = std::fs::symlink_metadata(path)?;
        ensure!(entry.uid() == unsafe { libc::geteuid() }
            && (entry.is_dir() || entry.file_type().is_symlink()), "invalid installation entry");
        let root = path.canonicalize()?;
        ensure!(safe_absolute(&root), "invalid source path");
        let directory = crate::package_fs::Directory::open_anchored(&root)?;
        ensure!(directory.identity()? == source_identity, "foreign source identity");
        let raw_target = if entry.file_type().is_symlink() { Some(std::fs::read_link(path)?) } else { None };
        let bound = Self { path: path.into(), root, source_identity: source_identity.into(),
            entry_identity: format!("{}:{}", entry.dev(), entry.ino()), raw_target };
        bound.verify()?;
        Ok(bound)
    }

    #[cfg(not(unix))]
    fn bind(_: &Path, _: &str) -> Result<Self> { anyhow::bail!("unsupported platform") }

    #[cfg(unix)]
    fn verify(&self) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let entry = std::fs::symlink_metadata(&self.path)?;
        let target = if entry.file_type().is_symlink() { Some(std::fs::read_link(&self.path)?) } else { None };
        ensure!(entry.uid() == unsafe { libc::geteuid() }
            && format!("{}:{}", entry.dev(), entry.ino()) == self.entry_identity
            && target == self.raw_target && self.path.canonicalize()? == self.root,
            "installation changed");
        let directory = crate::package_fs::Directory::open_anchored(&self.root)?;
        ensure!(directory.identity()? == self.source_identity, "source changed");
        directory.verify_location(&self.root)?;
        Ok(())
    }

    #[cfg(not(unix))]
    fn verify(&self) -> Result<()> { anyhow::bail!("unsupported platform") }

    fn accepts_skill_path(&self, text: &str) -> Result<()> {
        let path = Path::new(text);
        let canonical_skill = self.root.join("SKILL.md");
        ensure!(safe_absolute(path) && (path == self.path.join("SKILL.md") || path == canonical_skill)
            && path.canonicalize()? == canonical_skill, "unrecognized skill path");
        Ok(())
    }
}

/// Native whole reads preserve the exact source, including its final newline.
/// Their total_lines metadata need not describe a window. Byte equality itself
/// proves completeness. Raw windows instead use Rust lines().join("\n"): CRLF becomes LF, and
/// exactly the final line terminator is omitted. Compare the entire normalized
/// document, NOT a substring or trim_end (which would lose final blank lines).
fn proves_complete(file: &FileContent, source: &str) -> bool {
    if !source.is_empty() && file.offset.is_none() && file.limit.is_none() && file.raw_output == source {
        return true;
    }
    let lines: Vec<_> = source.lines().collect();
    !source.is_empty() && file.offset.is_none() && file.total_lines == lines.len() as u64
        && file.limit.is_none_or(|limit| limit >= lines.len() as u64)
        && file.raw_output == lines.join("\n")
}

fn replacement(skill_path: &Path, source: &str, adaptation: &str) -> Result<ReadResult> {
    // Reject the marker text even with a CRLF/no final terminator, so neither
    // normalization nor the section separator can create a second probe prefix.
    ensure!(!source.contains(SOURCE_BOUND_READ_PREFIX.trim_end_matches('\n')), "ambiguous source prefix");
    ensure!(!adaptation.is_empty() && !adaptation.contains('\0')
        && adaptation.len() <= 64 * 1024 && adaptation.chars().count() <= 10_000,
        "adaptation exceeds complete context bound");
    let payload = format!("{SOURCE_BOUND_READ_PREFIX}SKILL.md\n{source}\nlocal-data/ADAPTATION.md\n{adaptation}");
    ensure!(payload.chars().count() <= MAX_COMBINED_CHARS, "combined replacement exceeds bound");
    let lines: Vec<_> = source.lines().collect();
    Ok(ReadResult {
        kind: "ReadFile".into(),
        file: FileContent {
            content: payload.clone(), content_concise: Some(payload),
            absolute_path: skill_path.to_str().ok_or_else(|| anyhow::anyhow!("invalid source path"))?.into(),
            offset: None, limit: None, raw_output: lines.join("\n"), total_lines: lines.len() as u64,
            extracted_images: Vec::new(),
        },
    })
}

fn load(installation: &Path, identity: &str, bytes: &[u8]) -> Result<ReadResult> {
    use crate::native_adaptation_reader as reader;
    ensure!(bytes.len() as u64 <= MAX_EVENT, "event exceeds bound");
    let event: Event = serde_json::from_slice(bytes)?;
    event.validate()?;
    // Typed decoding above rejects duplicate named keys. Also compare the actual
    // JSON mirrors, including optional-key presence (missing != explicit null).
    let wire: serde_json::Value = serde_json::from_slice(bytes)?;
    for (camel, snake) in [
        ("sessionId", "session_id"), ("transcriptPath", "transcript_path"),
        ("permissionMode", "permission_mode"), ("toolName", "tool_name"),
        ("toolUseId", "tool_use_id"), ("toolInput", "tool_input"),
        ("toolResult", "tool_response"), ("durationMs", "duration_ms"),
    ] {
        if let Some(mirror) = wire.get(snake) {
            ensure!(wire.get(camel) == Some(mirror), "wire mirrors disagree");
        }
    }
    ensure!(["toolInput", "toolResult"].into_iter().all(|key|
        serde_json::to_vec(&wire[key]).is_ok_and(|json| json.len() <= MAX_NATIVE_PAYLOAD)),
        "native payload exceeds bound");
    let bound = Installation::bind(installation, identity)?;
    // The successful result is authoritative. Input is ONLY a consistency gate;
    // relative paths and other aliases remain unsupported until native evidence.
    bound.accepts_skill_path(&event.tool_result.file.absolute_path)?;
    bound.accepts_skill_path(&event.tool_input.target_file)?;
    let skill = reader::source_document(&bound.root, identity)?;
    ensure!(proves_complete(&event.tool_result.file, &skill.text), "partial source read");
    ensure!(!skill.text.contains(SOURCE_BOUND_READ_PREFIX.trim_end_matches('\n')), "ambiguous source prefix");
    // No ADP or local-data open occurs before the complete source proof.
    let local_identity = reader::source_local_data_identity(&bound.root, identity)?;
    #[cfg(all(test, unix))]
    BEFORE_ADAPTATION.with(|hook| if let Some(callback) = hook.borrow_mut().take() { callback(); });
    bound.verify()?;
    // Authorization for this current read is the trusted command registration's
    // installation/source ID, NOT a claimed native historical inode. Build both
    // model projections from the SAME transaction; never reuse skill.text here.
    let (source, adaptation) = reader::source_and_adaptation_for_verified_skill(
        &bound.root, identity, &local_identity, &skill)?;
    ensure!(proves_complete(&event.tool_result.file, &source), "native/source text mismatch");
    bound.verify()?;
    replacement(&bound.root.join("SKILL.md"), &source, &adaptation)
}

fn output(installation: &Path, identity: &str, bytes: &[u8]) -> serde_json::Value {
    match load(installation, identity, bytes) {
        // There is deliberately no independent ADP channel if the host rejects
        // or ignores this same-ReadFile-discriminant replacement.
        Ok(candidate) => serde_json::json!({"hookSpecificOutput": {"updatedToolOutput": candidate}}),
        Err(_) => serde_json::json!({}),
    }
}

/// Exact argv: FLAG --installation ABSOLUTE_DIRECTORY --source-identity DEV:INO.
/// Every requested invocation exits zero with one JSON object. Errors are silent;
/// no raw input, paths, OS error, credentials or transcript is ever logged.
pub(crate) fn run_if_requested() {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(OsStr::new(FLAG)) { return; }
    let result = (|| -> Result<serde_json::Value> {
        ensure!(args.next().as_deref() == Some(OsStr::new("--installation")), "invalid arguments");
        let path = PathBuf::from(args.next().ok_or_else(|| anyhow::anyhow!("missing installation"))?);
        ensure!(args.next().as_deref() == Some(OsStr::new("--source-identity")), "invalid arguments");
        let identity = args.next().and_then(|arg| arg.into_string().ok())
            .ok_or_else(|| anyhow::anyhow!("missing identity"))?;
        ensure!(args.next().is_none() && safe_absolute(&path) && valid_identity(&identity), "invalid arguments");
        let mut bytes = Vec::new();
        std::io::stdin().lock().take(MAX_EVENT + 1).read_to_end(&mut bytes)?;
        Ok(output(&path, &identity, &bytes))
    })();
    let mut stdout = std::io::stdout().lock();
    let _ = serde_json::to_writer(&mut stdout, &result.unwrap_or_else(|_| serde_json::json!({})));
    let _ = stdout.write_all(b"\n");
    let _ = stdout.flush();
    std::process::exit(0);
}

#[cfg(all(test, unix))]
thread_local! {
    static BEFORE_ADAPTATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::{fs, os::unix::fs::{MetadataExt, symlink}};

    fn identity(path: &Path) -> String {
        let metadata = fs::metadata(path).unwrap();
        format!("{}:{}", metadata.dev(), metadata.ino())
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        installed: PathBuf,
        id: String,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let base = temp.path().canonicalize().unwrap();
            let root = base.join("library/demo");
            fs::create_dir_all(root.join("local-data")).unwrap();
            fs::write(root.join("SKILL.md"), "# Demo\nComplete instructions\n").unwrap();
            fs::write(root.join("local-data/ADAPTATION.md"), "# 本机适配\n完整适配\n").unwrap();
            let installed = base.join("installed-demo");
            symlink(&root, &installed).unwrap();
            let id = identity(&root);
            Self { _temp: temp, root, installed, id }
        }
        fn event(&self) -> Value {
            let text = fs::read_to_string(self.root.join("SKILL.md")).unwrap();
            let lines: Vec<_> = text.lines().collect();
            json!({
                "hookEventName": "post_tool_use", "hook_event_name": "PostToolUse",
                "sessionId": "isolated-public-session", "session_id": "isolated-public-session",
                "cwd": "/unused-cwd", "workspaceRoot": "/unused-workspace",
                "timestamp": "2026-10-21T00:00:00Z",
                "toolName": "read_file", "tool_name": "read_file",
                "toolUseId": "public-call", "tool_use_id": "public-call",
                "toolInput": {"target_file": self.installed.join("SKILL.md")},
                "tool_input": {"target_file": self.installed.join("SKILL.md")},
                "toolResult": {"type": "ReadFile", "FileContent": {
                    "content": "1→model-facing content", "absolute_path": self.root.join("SKILL.md"),
                    "offset": null, "raw_output": lines.join("\n"), "total_lines": lines.len()
                }},
                "toolInputTruncated": false, "toolResultTruncated": false, "isBackgrounded": false
            })
        }
        fn output(&self, event: &Value) -> Value {
            output(&self.installed, &self.id, &serde_json::to_vec(event).unwrap())
        }
        fn assert_loaded(&self, event: &Value) {
            let source = fs::read_to_string(self.root.join("SKILL.md")).unwrap();
            let adaptation = fs::read_to_string(self.root.join("local-data/ADAPTATION.md")).unwrap();
            let payload = format!("{SOURCE_BOUND_READ_PREFIX}SKILL.md\n{source}\nlocal-data/ADAPTATION.md\n{adaptation}");
            let lines: Vec<_> = source.lines().collect();
            assert_eq!(self.output(event), json!({"hookSpecificOutput": {"updatedToolOutput": {
                "type": "ReadFile", "FileContent": {
                    "content": payload, "content_concise": payload,
                    "absolute_path": self.root.join("SKILL.md"), "offset": null,
                    "raw_output": lines.join("\n"), "total_lines": lines.len()
                }
            }}}));
        }
    }

    #[test]
    fn replacement_projections_preserve_both_full_texts_and_true_source_raw_output() {
        let fixture = Fixture::new();
        let source = "# 完整源\r\nInstructions\r\n\r\n";
        let adaptation = "# 本机适配\n完整😀\n";
        fs::write(fixture.root.join("SKILL.md"), source).unwrap();
        fs::write(fixture.root.join("local-data/ADAPTATION.md"), adaptation).unwrap();
        let event = fixture.event();
        fixture.assert_loaded(&event);
        let result = fixture.output(&event);
        let specific = result["hookSpecificOutput"].as_object().unwrap();
        assert_eq!(result.as_object().unwrap().len(), 1);
        assert_eq!(specific.len(), 1);
        assert!(specific.get("additionalContext").is_none());
        let replacement: ReadResult = serde_json::from_value(specific["updatedToolOutput"].clone()).unwrap();
        assert_eq!(replacement.kind, "ReadFile");
        let file = replacement.file;
        let expected = format!("Pleat source-bound skill read\nSKILL.md\n{source}\nlocal-data/ADAPTATION.md\n{adaptation}");
        assert_eq!(file.content, expected);
        assert_eq!(file.content_concise.as_deref(), Some(expected.as_str()));
        assert_eq!(file.raw_output, "# 完整源\nInstructions\n");
        assert_eq!(file.total_lines, 3);
        assert_eq!(file.absolute_path, fixture.root.join("SKILL.md").to_str().unwrap());
        assert_eq!(file.offset, None);
        assert!(specific["updatedToolOutput"].get("success").is_none());
    }

    #[test]
    fn ignored_missing_or_unknown_replacement_has_no_independent_adaptation_route() {
        let fixture = Fixture::new();
        let adaptation = "ADP-ONLY-IN-REPLACEMENT";
        fs::write(fixture.root.join("local-data/ADAPTATION.md"), adaptation).unwrap();
        let result = fixture.output(&fixture.event());
        let specific = result["hookSpecificOutput"].as_object().unwrap();
        assert_eq!(specific.keys().map(String::as_str).collect::<Vec<_>>(), ["updatedToolOutput"]);
        // Structural channel test, not an emulation/proof of native delivery.
        let mut missing = result.clone();
        missing["hookSpecificOutput"].as_object_mut().unwrap().remove("updatedToolOutput");
        assert_eq!(missing, json!({"hookSpecificOutput": {}}));
        assert!(!missing.to_string().contains(adaptation));
        let mut unknown = result;
        unknown["hookSpecificOutput"]["updatedToolOutput"] = json!({"type": "UnknownReplacement"});
        assert!(unknown["hookSpecificOutput"].get("additionalContext").is_none());
        assert!(!unknown.to_string().contains(adaptation));
    }

    #[test]
    fn same_byte_inode_replacement_before_first_proof_uses_current_registered_read() {
        let fixture = Fixture::new();
        let native_event = fixture.event();
        let skill_path = fixture.root.join("SKILL.md");
        let held_old_file = fs::File::open(&skill_path).unwrap();
        let old_inode = held_old_file.metadata().unwrap().ino();
        fs::write(fixture.root.join("next"), fs::read(&skill_path).unwrap()).unwrap();
        fs::rename(fixture.root.join("next"), &skill_path).unwrap();
        assert_ne!(fs::metadata(&skill_path).unwrap().ino(), old_inode);
        // No assertion that the native event read this inode. Its full text
        // matches; trusted registration authorizes the current replacement read.
        fixture.assert_loaded(&native_event);
    }

    #[test]
    fn same_text_foreign_history_restored_before_proof_replaces_with_registered_pair() {
        let fixture = Fixture::new();
        let held_source = fixture.root.with_file_name("held-registered");
        let foreign_source = fixture.root.with_file_name("held-foreign");
        fs::rename(&fixture.root, &held_source).unwrap();
        fs::create_dir_all(fixture.root.join("local-data")).unwrap();
        fs::copy(held_source.join("SKILL.md"), fixture.root.join("SKILL.md")).unwrap();
        fs::write(fixture.root.join("local-data/ADAPTATION.md"), "FOREIGN-ADAPTATION").unwrap();
        let native_event = fixture.event();
        // While the current source ID differs, the command cannot load it.
        assert_eq!(fixture.output(&native_event), json!({}));
        fs::rename(&fixture.root, foreign_source).unwrap();
        fs::rename(held_source, &fixture.root).unwrap();
        // The event carries no historical identity. It now gates a CURRENT,
        // explicitly registered FD read; both delivered texts are from that read.
        fixture.assert_loaded(&native_event);
        assert!(!fixture.output(&native_event).to_string().contains("FOREIGN-ADAPTATION"));
    }

    #[test]
    fn combined_unicode_limit_is_inclusive_and_never_falls_back_to_context_only() {
        let fixture = Fixture::new();
        let adaptation = "完整😀";
        fs::write(fixture.root.join("local-data/ADAPTATION.md"), adaptation).unwrap();
        let overhead = SOURCE_BOUND_READ_PREFIX.chars().count()
            + "SKILL.md\n\nlocal-data/ADAPTATION.md\n".chars().count()
            + adaptation.chars().count();
        // Mostly ASCII so a complete native result fits its separate 128-KiB
        // JSON byte limit while the replacement reaches its Unicode char limit.
        let source = format!("界{}", "x".repeat(MAX_COMBINED_CHARS - overhead - 1));
        fs::write(fixture.root.join("SKILL.md"), &source).unwrap();
        fixture.assert_loaded(&fixture.event());
        let exact = fixture.output(&fixture.event());
        assert_eq!(exact["hookSpecificOutput"]["updatedToolOutput"]["FileContent"]["content"]
            .as_str().unwrap().chars().count(), MAX_COMBINED_CHARS);
        fs::write(fixture.root.join("SKILL.md"), format!("{source}界")).unwrap();
        assert_eq!(fixture.output(&fixture.event()), json!({}));
    }

    #[test]
    fn source_probe_prefix_is_rejected_before_adaptation_reads() {
        let fixture = Fixture::new();
        for source in [SOURCE_BOUND_READ_PREFIX.to_owned(),
            SOURCE_BOUND_READ_PREFIX.replace('\n', "\r\n"),
            SOURCE_BOUND_READ_PREFIX.trim_end_matches('\n').to_owned()] {
            fs::write(fixture.root.join("SKILL.md"), source).unwrap();
            BEFORE_ADAPTATION.with(|hook| *hook.borrow_mut() = Some(Box::new(|| panic!("ADP stage reached for ambiguous prefix"))));
            assert_eq!(fixture.output(&fixture.event()), json!({}));
            BEFORE_ADAPTATION.with(|hook| assert!(hook.borrow_mut().take().is_some()));
        }
    }

    #[test]
    fn official_success_shape_accepts_both_mirrors_and_known_optional_metadata() {
        let fixture = Fixture::new();
        let mut event = fixture.event();
        event["transcriptPath"] = json!("/never-open-this-transcript");
        event["transcript_path"] = event["transcriptPath"].clone();
        event["permissionMode"] = json!("default");
        event["permission_mode"] = event["permissionMode"].clone();
        event["clientIdentifier"] = json!("public-client");
        event["promptId"] = json!("public-prompt");
        event["durationMs"] = json!(12);
        event["duration_ms"] = json!(12);
        event["subagentType"] = json!("public-agent");
        event["toolInput"]["offset"] = Value::Null;
        event["toolInput"]["limit"] = json!(100);
        event["toolInput"]["pages"] = Value::Null;
        event["toolInput"]["format"] = Value::Null;
        event["tool_input"] = event["toolInput"].clone();
        event["toolResult"]["FileContent"]["content_concise"] = json!("concise");
        event["toolResult"]["FileContent"]["limit"] = json!(100);
        event["toolResult"]["FileContent"]["extracted_images"] = json!([]);
        event["tool_response"] = event["toolResult"].clone();
        fixture.assert_loaded(&event);
        // The result may spell the installed path, too; cwd is never consulted.
        event["toolResult"]["FileContent"]["absolute_path"] = json!(fixture.installed.join("SKILL.md"));
        event["tool_response"] = event["toolResult"].clone();
        fixture.assert_loaded(&event);
    }

    #[test]
    fn critical_mirror_mismatch_null_snake_only_and_duplicate_keys_are_rejected() {
        let fixture = Fixture::new();
        for (key, value) in [
            ("hook_event_name", json!("post_tool_use")), ("tool_name", json!("write_file")),
            ("session_id", json!("different")), ("tool_use_id", json!("different")),
            ("tool_input", json!({"target_file": "/other/SKILL.md"})),
            ("tool_response", json!({"type": "ReadFile", "PermissionDenied": "denied"})),
            ("tool_input", Value::Null),
        ] {
            let mut event = fixture.event();
            event[key] = value;
            assert_eq!(fixture.output(&event), json!({}), "{key}");
        }
        let mut event = fixture.event();
        event.as_object_mut().unwrap().remove("toolName");
        assert_eq!(fixture.output(&event), json!({}));
        let mut event = fixture.event();
        event["tool_input"]["offset"] = Value::Null; // Missing vs explicit null.
        assert_eq!(fixture.output(&event), json!({}));
        let bytes = serde_json::to_string(&fixture.event()).unwrap();
        for duplicate in ["\"toolName\":\"read_file\",", "\"tool_input\":null,"] {
            let bytes = format!("{{{duplicate}{}", &bytes[1..]);
            assert_eq!(output(&fixture.installed, &fixture.id, bytes.as_bytes()), json!({}));
        }
    }

    #[test]
    fn refusals_errors_unknown_variants_and_truncation_never_load() {
        let fixture = Fixture::new();
        for result in [
            json!({"type": "ReadFile", "PermissionDenied": "public refusal"}),
            json!({"type": "ReadFile", "FileNotFound": "missing"}),
            json!({"type": "ReadFile", "FileReadError": "error"}),
            json!({"type": "ReadFile", "ImageContent": {}}),
            json!({"type": "ReadFile", "PdfPageImages": []}),
            json!({"type": "FutureRead", "FileContent": fixture.event()["toolResult"]["FileContent"]}),
            json!("[truncated]"), Value::Null,
        ] {
            let mut event = fixture.event();
            event["toolResult"] = result;
            assert_eq!(fixture.output(&event), json!({}));
        }
        for (key, value) in [
            ("toolInputTruncated", json!(true)), ("toolResultTruncated", json!(true)),
            ("isBackgrounded", json!(true)), ("hookEventName", json!("permission_denied")),
            ("hookEventName", json!("pre_tool_use")), ("toolName", json!("write_file")),
            ("toolInput", json!({"dispatch": {"target_file": "/unknown/SKILL.md"}})),
            ("toolResultTruncated", json!("false")), ("futurePolicyField", json!(false)),
        ] {
            let mut event = fixture.event();
            event[key] = value;
            assert_eq!(fixture.output(&event), json!({}), "{key}");
        }
        for variant in ["PermissionDenied", "FutureVariant", "success", "is_error"] {
            let mut event = fixture.event();
            event["toolResult"][variant] = json!(false);
            assert_eq!(fixture.output(&event), json!({}));
        }
        for missing in ["toolInputTruncated", "toolResultTruncated", "isBackgrounded"] {
            let mut event = fixture.event();
            event.as_object_mut().unwrap().remove(missing);
            assert_eq!(fixture.output(&event), json!({}));
        }
    }

    #[test]
    fn complete_source_proof_preserves_final_blank_lines_and_crlf_rules() {
        let fixture = Fixture::new();
        for text in ["one", "one\n", "one\n\n", "one\r\ntwo\r\n\r\n", "\n", "中文\n"] {
            fs::write(fixture.root.join("SKILL.md"), text).unwrap();
            fixture.assert_loaded(&fixture.event());
        }
        let mut event = fixture.event();
        event["toolResult"]["FileContent"]["raw_output"] = json!("partial");
        assert_eq!(fixture.output(&event), json!({}));
        for (key, value) in [("offset", json!(2)), ("limit", json!(0)),
            ("total_lines", json!(100)), ("raw_output", Value::Null)] {
            let mut event = fixture.event();
            event["toolResult"]["FileContent"][key] = value;
            assert_eq!(fixture.output(&event), json!({}));
        }
        let mut event = fixture.event();
        event["toolResult"]["FileContent"].as_object_mut().unwrap().remove("offset");
        assert_eq!(fixture.output(&event), json!({}));
        fs::write(fixture.root.join("SKILL.md"), "one\n\n").unwrap();
        let mut event = fixture.event();
        event["toolResult"]["FileContent"]["raw_output"] = json!("one");
        assert_eq!(fixture.output(&event), json!({}));
    }

    #[test]
    fn exact_whole_native_read_keeps_terminators_and_ignores_window_line_metadata() {
        let fixture = Fixture::new();
        for text in ["whole\n", "whole\n\n", "whole\r\nbody\r\n", "中文\n"] {
            fs::write(fixture.root.join("SKILL.md"), text).unwrap();
            let mut event = fixture.event();
            event["toolResult"]["FileContent"]["raw_output"] = json!(text);
            event["toolResult"]["FileContent"]["total_lines"] = json!(0);
            event["tool_response"] = event["toolResult"].clone();
            fixture.assert_loaded(&event);
            for changed in [text.trim_end().to_owned(), format!("{text}other")] {
                let mut bad = event.clone();
                bad["toolResult"]["FileContent"]["raw_output"] = json!(changed);
                bad["tool_response"] = bad["toolResult"].clone();
                assert_eq!(fixture.output(&bad), json!({}));
            }
            let mut bad = event.clone();
            bad["toolResult"]["FileContent"]["offset"] = json!(2);
            bad["tool_response"] = bad["toolResult"].clone();
            assert_eq!(fixture.output(&bad), json!({}));
        }
    }

    #[test]
    fn same_name_foreign_source_relative_alias_and_other_file_are_rejected() {
        let fixture = Fixture::new();
        let foreign = Fixture::new();
        // Identical names and bytes do not establish source identity.
        let bytes = serde_json::to_vec(&foreign.event()).unwrap();
        assert_eq!(output(&foreign.installed, &fixture.id, &bytes), json!({}));
        assert_eq!(fixture.output(&foreign.event()), json!({}));
        for path in ["SKILL.md".into(), "/unused-cwd/SKILL.md".into(),
            fixture.root.join("other.md").to_string_lossy().into_owned(),
            format!("{}/../demo/SKILL.md", fixture.root.display()),
            format!("{}/SKILL.md\n", fixture.root.display())] {
            for input in [false, true] {
                let mut event = fixture.event();
                if input {
                    event["toolInput"]["target_file"] = json!(path);
                    event["tool_input"] = event["toolInput"].clone();
                } else { event["toolResult"]["FileContent"]["absolute_path"] = json!(path); }
                assert_eq!(fixture.output(&event), json!({}));
            }
        }
        assert!(!valid_identity("1:2:3") && !valid_identity("1:-2"));
    }

    #[test]
    fn source_updates_and_inode_preserving_migration_are_resolved_per_invocation() {
        let mut fixture = Fixture::new();
        fixture.assert_loaded(&fixture.event());
        fs::write(fixture.root.join("next"), "Updated whole skill\n").unwrap();
        fs::rename(fixture.root.join("next"), fixture.root.join("SKILL.md")).unwrap();
        fixture.assert_loaded(&fixture.event());
        // Retain the installation symlink itself while the library moves.
        let installed_inode = fs::symlink_metadata(&fixture.installed).unwrap().ino();
        let library = fixture.root.parent().unwrap().to_path_buf();
        let moved_library = library.with_file_name("migrated-library");
        fs::rename(&library, &moved_library).unwrap();
        symlink(&moved_library, &library).unwrap();
        fixture.root = moved_library.join("demo");
        assert_eq!(fs::symlink_metadata(&fixture.installed).unwrap().ino(), installed_inode);
        assert_eq!(identity(&fixture.root), fixture.id);
        fixture.assert_loaded(&fixture.event());
        // A later host-managed retarget between invocations is also accepted.
        let moved = fixture.root.parent().unwrap().parent().unwrap().join("migrated-demo");
        fs::rename(&fixture.root, &moved).unwrap();
        fs::remove_file(&fixture.installed).unwrap();
        symlink(&moved, &fixture.installed).unwrap();
        fixture.root = moved;
        assert_eq!(identity(&fixture.root), fixture.id);
        fixture.assert_loaded(&fixture.event());
    }

    #[test]
    fn link_source_and_local_data_replacement_between_phases_are_rejected() {
        for change in ["link", "source", "local", "skill"] {
            let fixture = Fixture::new();
            let root = fixture.root.clone();
            let installed = fixture.installed.clone();
            BEFORE_ADAPTATION.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
                match change {
                    "link" => {
                        let replacement = root.parent().unwrap().join("replacement-link");
                        symlink(&root, &replacement).unwrap();
                        fs::rename(replacement, installed).unwrap();
                    }
                    "source" => {
                        fs::rename(&root, root.with_file_name("held-source")).unwrap();
                        fs::create_dir(&root).unwrap();
                    }
                    "local" => {
                        fs::rename(root.join("local-data"), root.join("held-local")).unwrap();
                        fs::create_dir(root.join("local-data")).unwrap();
                    }
                    _ => {
                        let text = fs::read(root.join("SKILL.md")).unwrap();
                        fs::write(root.join("next"), text).unwrap();
                        fs::rename(root.join("next"), root.join("SKILL.md")).unwrap();
                    }
                }
            })));
            assert_eq!(fixture.output(&fixture.event()), json!({}), "{change}");
        }
    }

    #[test]
    fn adaptation_is_complete_nonempty_unicode_bounded_and_never_truncated() {
        let fixture = Fixture::new();
        let path = fixture.root.join("local-data/ADAPTATION.md");
        for text in ["界".repeat(10_000), "😀".repeat(10_000)] {
            fs::write(&path, text).unwrap();
            fixture.assert_loaded(&fixture.event());
        }
        for bytes in [Vec::new(), "界".repeat(10_001).into_bytes(), vec![b'x'; 64 * 1024 + 1],
            b"bad\0adaptation".to_vec(), vec![0xff]] {
            fs::write(&path, bytes).unwrap();
            assert_eq!(fixture.output(&fixture.event()), json!({}));
        }
        fs::remove_file(&path).unwrap();
        assert_eq!(fixture.output(&fixture.event()), json!({}));
        symlink(fixture.root.join("SKILL.md"), &path).unwrap();
        assert_eq!(fixture.output(&fixture.event()), json!({}));
        assert_eq!(output(&fixture.installed, &fixture.id, &vec![b' '; MAX_EVENT as usize + 1]), json!({}));
        assert_eq!(output(&fixture.installed, &fixture.id, b"{}{}"), json!({}));
        let mut event = fixture.event();
        event["toolResult"]["FileContent"]["content"] = json!("x".repeat(MAX_NATIVE_PAYLOAD + 1));
        assert_eq!(fixture.output(&event), json!({}));
    }

    #[test]
    fn isolated_adaptation_permission_failure_stops_without_fallback() {
        use std::os::unix::fs::PermissionsExt;
        // Root can read mode 000; do not claim a permission denial in that case.
        if unsafe { libc::geteuid() } == 0 { return; }
        let fixture = Fixture::new();
        let path = fixture.root.join("local-data/ADAPTATION.md");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        assert_eq!(fixture.output(&fixture.event()), json!({}));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}
