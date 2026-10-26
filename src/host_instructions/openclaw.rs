//! OpenClaw supplies a native layout; profile publication uses the shared store.
use super::{hermes, opencode};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{fs, path::{Path, PathBuf}};

pub(super) const PLUGIN_ID: &str = "pleat-openclaw-adaptation";
// Exact JSON emitted by the first desktop producer (serde_json/preserve_order).
// New files use sorted keys on every build; existing known encodings stay intact.
const LEGACY_MANIFEST: &str = r#"{"id":"pleat-openclaw-adaptation","name":"Pleat managed skills","version":"0.1.0","description":"Managed skill body and local adaptation through native plugin tools","contracts":{"tools":["pleat_read_skill"]},"configSchema":{"type":"object","additionalProperties":false,"required":["pleatManagedProfileVersion","pleatManagedAllowAdded"],"properties":{"pleatManagedProfileVersion":{"const":1},"pleatManagedAllowAdded":{"type":"boolean"}}}}"#;
const LEGACY_PACKAGE: &str = r#"{"name":"pleat-openclaw-adaptation","version":"0.1.0","type":"module","openclaw":{"extensions":["./index.js"]}}"#;

fn canonical_json(mut value: serde_json::Value) -> Result<String> {
    value.sort_all_objects();
    Ok(serde_json::to_string(&value)?)
}

fn retain_known_json(path: &Path, canonical: String, legacy: &str) -> Result<String> {
    if let Some((bytes, _)) = super::snapshot(path, 16 * 1024)? {
        if bytes == canonical.as_bytes() || bytes == legacy.as_bytes() {
            return String::from_utf8(bytes).context("OpenClaw 登记编码无效");
        }
    }
    // The shared store will reject every other existing body, including edits
    // that merely reformat JSON. This is not a generic semantic-JSON adoption.
    Ok(canonical)
}

fn record_encodings(value: &serde_json::Value) -> Result<(Vec<u8>, Vec<u8>)> {
    let entry = &value["skills"][0];
    let scalar = |key: &str| serde_json::to_string(&entry[key]);
    let canonical = (canonical_json(value.clone())? + "\n").into_bytes();
    let legacy = format!(concat!("{{\"schemaVersion\":1,\"manager\":\"pleat-native-openclaw-v1\",\"skills\":[{{",
        "\"installationId\":{},\"installationSkillPath\":{},\"canonicalSourceRoot\":{},",
        "\"sourceIdentity\":{},\"scope\":{},\"workspaceRoot\":{}}}]}}\n"),
        scalar("installationId")?, scalar("installationSkillPath")?, scalar("canonicalSourceRoot")?,
        scalar("sourceIdentity")?, scalar("scope")?, scalar("workspaceRoot")?).into_bytes();
    Ok((canonical, legacy))
}

#[derive(Clone, Debug)]
pub(super) struct Plan {
    pub(super) path: PathBuf,
    store: hermes::Plan,
    home: PathBuf,
    state: PathBuf,
    config_path: PathBuf,
}

fn record_path(state: &Path, installation: &Path) -> PathBuf {
    let digest = format!("{:x}", Sha256::digest(installation.as_os_str().as_encoded_bytes()));
    state.join("extensions").join(PLUGIN_ID).join("registrations").join(format!("{}.json", &digest[..24]))
}

fn descriptor(installation: &Path, source: &Path, identity: &str, project: Option<&Path>) -> serde_json::Value {
    let id = format!("pleat-{:x}", Sha256::digest(installation.as_os_str().as_encoded_bytes()));
    serde_json::json!({"schemaVersion":1,"manager":"pleat-native-openclaw-v1","skills":[{
        "installationId":&id[..30], "installationSkillPath":installation.join("SKILL.md"),
        "canonicalSourceRoot":source,"sourceIdentity":identity,
        "scope":if project.is_some() {"workspace"} else {"global"}, "workspaceRoot":project
    }]})
}

fn body(record: &Path, installation: &Path, source: &Path, project: Option<&Path>) -> Result<Vec<u8>> {
    let identity = crate::library::filesystem_identity(source)?;
    let existing = super::snapshot(record, 16 * 1024)?;
    if let Some((bytes, _)) = existing {
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            if let Some(locator) = value.pointer("/skills/0/canonicalSourceRoot").and_then(|v| v.as_str()).map(PathBuf::from) {
                if locator.is_absolute() && locator.canonicalize().ok().as_deref() == Some(source)
                    && value == descriptor(installation, &locator, &identity, project) {
                    let (canonical, legacy) = record_encodings(&value)?;
                    if bytes == canonical || bytes == legacy { return Ok(bytes); }
                }
            }
        }
        // The shared store protects a differing record; it is never repaired
        // by adopting a newly retargeted link or arbitrary third-party metadata.
    }
    Ok(record_encodings(&descriptor(installation, source, &identity, project))?.0)
}

pub(super) fn plan(project: Option<&Path>, installation: &Path, source: &Path, install: bool) -> Result<Plan> {
    let home = dirs::home_dir().context("无法取得 OpenClaw 用户目录")?;
    let (state, config_path) = crate::agents::openclaw_paths(&home)?;
    let installation = if installation.is_absolute() { installation.to_owned() } else { std::env::current_dir()?.join(installation) };
    let source = source.canonicalize().context("OpenClaw 技能源不可验证，已保留")?;
    ensure!(source.is_dir() && source.join("SKILL.md").is_file(), "OpenClaw 技能源缺少 SKILL.md");
    let is_global = installation.parent() == Some(state.join("skills").as_path())
        || project.is_some_and(|p| crate::project_discovery::path_identity(p) == crate::project_discovery::path_identity(&home));
    let workspace = if is_global { None } else {
        Some(project.map(Path::to_path_buf).or_else(|| installation.parent()?.parent().map(Path::to_path_buf))
            .context("缺少 OpenClaw 工作区")?.canonicalize().context("OpenClaw 工作区不可验证")?)
    };
    let root = state.join("extensions").join(PLUGIN_ID);
    let record = record_path(&state, &installation);
    let physical_root = crate::agents::hermes::hermes_directory_identity(&root)?;
    let physical_record = physical_root.join("registrations").join(record.file_name().context("缺少 OpenClaw 登记文件名")?);
    let record_body = body(&physical_record, &installation, &source, workspace.as_deref())?;
    let reader = std::env::current_exe()?.canonicalize()?;
    let mut reader_body = canonical_json(serde_json::json!({"schemaVersion":1,
        "manager":"pleat-native-openclaw-reader-v1","readerExecutable":reader}))?;
    reader_body.push('\n');
    // CLI and desktop are two legitimate producers. Retain a valid existing
    // owned reader location instead of treating the caller's executable path
    // as an adapter-content change during an otherwise identical operation.
    if let Some((bytes, _)) = super::snapshot(&physical_root.join("reader.json"), 16 * 1024)? {
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            if let Some(path) = value.get("readerExecutable").and_then(|v| v.as_str()).map(PathBuf::from) {
                let expected = serde_json::json!({"schemaVersion":1,"manager":"pleat-native-openclaw-reader-v1","readerExecutable":path});
                let canonical = canonical_json(expected.clone())? + "\n";
                let legacy = format!("{{\"schemaVersion\":1,\"manager\":\"pleat-native-openclaw-reader-v1\",\"readerExecutable\":{}}}\n", serde_json::to_string(&path)?);
                if path.is_absolute() && path.is_file() && path.canonicalize().ok().as_deref() == Some(path.as_path())
                    && value == expected && (bytes == canonical.as_bytes() || bytes == legacy.as_bytes()) {
                    reader_body = String::from_utf8(bytes).context("OpenClaw 适配读取器记录编码无效")?;
                }
            }
        }
    }
    let manifest = retain_known_json(&physical_root.join("openclaw.plugin.json"),
        canonical_json(serde_json::from_str(LEGACY_MANIFEST)?)?, LEGACY_MANIFEST)?;
    let package = retain_known_json(&physical_root.join("package.json"),
        canonical_json(serde_json::from_str(LEGACY_PACKAGE)?)?, LEGACY_PACKAGE)?;
    let files = [
        ("index.js", include_str!("../../assets/host-adapters/openclaw-plugin-entry.js")),
        ("adapter.js", include_str!("../../assets/host-adapters/openclaw-adaptation.js")),
        ("reader.json", reader_body.as_str()), ("openclaw.plugin.json", manifest.as_str()),
        ("package.json", package.as_str()),
    ];
    let config = opencode::ConfigPlan::new_openclaw(&config_path, PLUGIN_ID, install)?;
    let store = hermes::Plan::profile(root, record, record_body, &source, &files, config)?;
    Ok(Plan { path: store.path.clone(), store, home, state, config_path })
}

pub(super) fn existing(installation: &Path) -> Result<Option<Plan>> {
    let home = dirs::home_dir().context("无法取得 OpenClaw 用户目录")?;
    let (state, _) = crate::agents::openclaw_paths(&home)?;
    let logical_record = record_path(&state, installation);
    match fs::symlink_metadata(&logical_record) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => anyhow::bail!("OpenClaw 适配登记不可读，已保留"), Ok(_) => {},
    }
    let source = installation.canonicalize().context("OpenClaw 安装来源不可验证，已保留")?;
    let root = crate::agents::hermes::hermes_directory_identity(&state.join("extensions").join(PLUGIN_ID))?;
    let record = root.join("registrations").join(logical_record.file_name().context("缺少 OpenClaw 登记名称")?);
    let (bytes, _) = super::snapshot(&record, 16 * 1024)?.context("OpenClaw 受管登记不可读，已保留")?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("OpenClaw 受管登记格式无效，已保留"))?;
    let project = match value.pointer("/skills/0/scope").and_then(|v| v.as_str()) {
        Some("global") => home,
        Some("workspace") => value.pointer("/skills/0/workspaceRoot").and_then(|v| v.as_str())
            .map(PathBuf::from).filter(|p| p.is_absolute()).context("OpenClaw 受管工作区不可验证，已保留")?,
        _ => anyhow::bail!("OpenClaw 受管登记范围未知，已保留"),
    };
    plan(Some(&project), installation, &source, false).map(Some)
}

impl Plan {
    pub(super) fn paths(&self) -> Vec<PathBuf> { self.store.paths() }
    pub(super) fn merge(&mut self, other: Self) -> Result<()> {
        ensure!(self.state == other.state && self.config_path == other.config_path, "OpenClaw profile 变化，请刷新");
        self.store.merge(other.store)
    }
    pub(super) fn coalesce(&mut self, files: &mut hermes::Pool, configs: &mut opencode::Pool) -> Result<()> {
        self.store.coalesce(files, configs)
    }
    pub(super) fn needs_install(&self) -> bool { self.store.needs_install() }
    pub(super) fn validate(&self) -> Result<()> {
        ensure!(crate::agents::openclaw_paths(&self.home)? == (self.state.clone(), self.config_path.clone()),
            "OpenClaw 当前 profile 已变化，请重新预览");
        self.store.validate()
    }
    pub(super) fn install(&mut self) -> Result<bool> { self.validate()?; self.store.install() }
    pub(super) fn detach(&mut self) -> Result<bool> { self.validate()?; self.store.detach() }
}
