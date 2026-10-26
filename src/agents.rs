use crate::InstallTarget;
use std::path::{Path, PathBuf};
use anyhow::{Result, ensure};

mod installed;
pub(crate) mod hermes;
pub(crate) mod grok;
pub use hermes::validate_hermes_project;
pub(crate) use hermes::{HermesSnapshot, hermes_home, hermes_project_root, hermes_snapshot};
pub use installed::installed_agents;
pub(crate) use installed::openclaw_bundled_skills_dir;

/// Installed Agents first; names are alphabetical regardless of letter case.
pub fn display_sort_key(name: &str, installed: bool) -> (bool, String) {
    (!installed, name.to_ascii_lowercase())
}

/// A known project binding permits inspecting and managing its configuration
/// even when the program is portable, unavailable, or no longer detected.
/// This does not change the separately recorded installation detection.
pub fn project_agent_available(
    kind: crate::effective_skills::AgentKind,
    detected: &[AgentIconInfo],
    projects: &[crate::project_discovery::ProjectEntry],
) -> bool {
    detected.iter().any(|agent| agent.id == kind.id())
        || projects.iter().any(|project| project.agents.contains(&kind))
}

pub fn project_agents(
    detected: &[AgentIconInfo],
    projects: &[crate::project_discovery::ProjectEntry],
) -> Vec<AgentIconInfo> {
    let mut agents = detected.to_vec();
    for info in AGENT_ICON_ORDER {
        if !agents.iter().any(|agent| agent.id == info.id)
            && crate::effective_skills::AgentKind::from_id(info.id)
                .is_some_and(|kind| project_agent_available(kind, detected, projects))
        {
            agents.push(*info);
        }
    }
    agents
}

/// User-level roots must not be derived from project-relative directories.
pub fn global_target_root(home: &Path, target: InstallTarget) -> Result<PathBuf> {
    ensure!(target != InstallTarget::Proma, "Proma 没有全局技能目标");
    Ok(match target {
        InstallTarget::Codex => std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"))
            .join("skills"),
        InstallTarget::ClaudeCode => claude_config_root(home)?.join("skills"),
        InstallTarget::Pi => std::env::var_os("PI_CODING_AGENT_DIR")
            .map(|value| {
                let value = value.to_string_lossy();
                if let Some(relative) = value.strip_prefix("~/") {
                    home.join(relative)
                } else {
                    home.join(value.as_ref())
                }
            })
            .unwrap_or_else(|| home.join(".pi/agent"))
            .join("skills"),
        InstallTarget::Omp => omp_agent_dir(home).join("skills"),
        InstallTarget::OpenCode => std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("opencode/skills"),
        InstallTarget::Antigravity => home.join(".gemini/config/skills"),
        InstallTarget::Copilot => copilot_config_root(home)?.join("skills"),
        InstallTarget::Grok => grok::grok_config_root(home)?.join("skills"),
        InstallTarget::Trae => home.join(".trae-cn/skills"),
        InstallTarget::WorkBuddy => workbuddy_config_root(home)?.join("skills"),
        InstallTarget::OpenClaw => openclaw_paths(home)?.0.join("skills"),
        InstallTarget::Hermes => hermes_home(home)?.join("skills"),
        _ => home.join(target_directory(target)),
    })
}

/// Shared Copilot CLI configuration root. Never include configured values in errors.
/// The environment-reading entry point delegates to the pure resolver below.
pub(crate) fn copilot_config_root(home: &Path) -> Result<PathBuf> {
    copilot_config_selection(home).map(|selection| selection.0)
}

pub(crate) fn copilot_config_selection(home: &Path) -> Result<(PathBuf, bool)> {
    let configured = std::env::var_os("COPILOT_HOME");
    let configured = configured.as_deref().map(|value| {
        value.to_str().ok_or_else(|| anyhow::anyhow!("Copilot 配置目录编码无效"))
    }).transpose()?;
    copilot_config_selection_with_origin(home, configured,
        |raw| crate::effective_skills::copilot_live_config_root(home, raw))
}

fn copilot_config_selection_with_origin(
    home: &Path, configured: Option<&str>,
    origin: impl FnOnce(&str) -> Result<PathBuf>,
) -> Result<(PathBuf, bool)> {
    if let Some(raw) = configured.filter(|raw| !raw.is_empty() && !Path::new(raw).is_absolute()) {
        ensure!(home.is_absolute() && !raw.contains('\0'), "Copilot 配置目录无效");
        let root = origin(raw)?;
        ensure!(root.is_absolute(), "Copilot 原生配置目录无效");
        return Ok((root, false));
    }
    copilot_config_selection_from(home, configured)
}

/// Fixed 1.0.98 TJn treats absent/empty overrides as the default. Native
/// resolveCopilotHome owns expansion of other forms; without a verified native
/// contract, reject relative/tilde forms rather than guessing or falling back.
pub(crate) fn copilot_config_root_from(home: &Path, configured: Option<&str>) -> Result<PathBuf> {
    copilot_config_selection_from(home, configured).map(|selection| selection.0)
}

pub(crate) fn copilot_config_selection_from(home: &Path, configured: Option<&str>) -> Result<(PathBuf, bool)> {
    let path = match configured {
        None | Some("") => home.join(".copilot"),
        Some(raw) => {
            ensure!(!raw.contains('\0'), "Copilot 配置目录无效");
            PathBuf::from(raw)
        }
    };
    ensure!(path.is_absolute(), "Copilot 配置目录仅支持已核验的绝对路径");
    // Native 1.0.98 compares the configured Path before normalizing its output.
    // A trailing separator keeps .agents; an equivalent path with .. excludes it.
    let include_personal_agents = path == home.join(".copilot");
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {},
            std::path::Component::ParentDir => { normalized.pop(); },
            component => normalized.push(component.as_os_str()),
        }
    }
    Ok((normalized, include_personal_agents))
}

pub(crate) fn claude_config_root(home: &Path) -> Result<PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(value) => {
            let path = PathBuf::from(value);
            ensure!(path.is_absolute(), "Claude Code 配置目录必须是非空绝对路径");
            Ok(path)
        }
        None => Ok(home.join(".claude")),
    }
}

/// Native state/profile selection shared by discovery, installation and scans.
/// This reads paths only; it never changes OpenClaw configuration.
pub(crate) fn openclaw_paths(home: &Path) -> Result<(PathBuf, PathBuf)> {
    let effective_home = openclaw_effective_home(home)?;
    let state = std::env::var_os("OPENCLAW_STATE_DIR").filter(|v| !v.is_empty());
    let profile = std::env::var("OPENCLAW_PROFILE").unwrap_or_default();
    let profile = profile.trim();
    let state = if let Some(raw) = state {
        openclaw_user_path(&raw.to_string_lossy(), home)?
    } else if profile.is_empty() || profile.eq_ignore_ascii_case("default") {
        effective_home.join(".openclaw")
    } else {
        ensure!(profile.len() <= 64 && profile.bytes().all(|c|
            c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-')),
            "OpenClaw profile 路径无法安全解析");
        effective_home.join(format!(".openclaw-{profile}"))
    };
    let config = std::env::var_os("OPENCLAW_CONFIG_PATH").filter(|v| !v.is_empty())
        .map(|raw| openclaw_user_path(&raw.to_string_lossy(), home))
        .transpose()?.unwrap_or_else(|| state.join("openclaw.json"));
    Ok((state, config))
}

pub(crate) fn openclaw_user_path(raw: &str, home: &Path) -> Result<PathBuf> {
    openclaw_path_from(raw, &openclaw_effective_home(home)?, &openclaw_cwd()?)
}

fn openclaw_cwd() -> Result<PathBuf> {
    std::env::current_dir().map_err(|_| anyhow::anyhow!("OpenClaw 路径基准目录不可读"))
}

pub(crate) fn openclaw_effective_home(os_home: &Path) -> Result<PathBuf> {
    let configured = std::env::var("OPENCLAW_HOME")
        .map(Some).or_else(|error| match error {
            std::env::VarError::NotPresent => Ok(None),
            _ => Err(anyhow::anyhow!("OpenClaw home 编码无效")),
        })?;
    openclaw_home_from(os_home, configured.as_deref(), &openclaw_cwd()?)
}

fn openclaw_home_from(os_home: &Path, configured: Option<&str>, cwd: &Path) -> Result<PathBuf> {
    let configured = configured.map(str::trim)
        .filter(|raw| !raw.is_empty() && !matches!(*raw, "undefined" | "null"));
    match configured {
        Some(raw) => openclaw_path_from(raw, os_home, cwd),
        None => openclaw_path_from(&os_home.to_string_lossy(), os_home, cwd),
    }
}

fn openclaw_path_from(raw: &str, home: &Path, cwd: &Path) -> Result<PathBuf> {
    let raw = raw.trim();
    let path = if raw == "~" { home.to_path_buf() }
        else if let Some(relative) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) { home.join(relative) }
        else { PathBuf::from(raw) };
    ensure!(!raw.is_empty() && !raw.contains('\0'), "OpenClaw 路径无效");
    let path = if path.is_absolute() { path } else { cwd.join(path) };
    // Native path.resolve normalizes dot segments before root comparisons.
    let mut resolved = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {},
            std::path::Component::ParentDir => { resolved.pop(); },
            _ => resolved.push(part.as_os_str()),
        }
    }
    ensure!(resolved.is_absolute(), "OpenClaw 路径基准必须是绝对路径");
    Ok(resolved)
}

fn openclaw_agent_entries(config: &serde_json::Value) -> Result<Option<Vec<(&str, &serde_json::Value)>>> {
    use serde_json::Value;
    if let Some(entries) = config.pointer("/agents/entries") {
        return Ok(Some(entries.as_object().ok_or_else(|| anyhow::anyhow!("OpenClaw agents.entries 结构无效"))?
            .iter().map(|(id, entry)| (id.as_str(), entry)).collect()));
    }
    if let Some(list) = config.pointer("/agents/list") {
        return Ok(Some(list.as_array().ok_or_else(|| anyhow::anyhow!("OpenClaw agents.list 结构无效"))?.iter()
            .map(|entry| entry.get("id").and_then(Value::as_str).map(|id| (id, entry))
                .ok_or_else(|| anyhow::anyhow!("OpenClaw legacy agent id 无效"))).collect::<Result<_>>()?));
    }
    Ok(None)
}

pub(crate) fn openclaw_agent_entry<'a>(config: &'a serde_json::Value, agent_id: &str) -> Result<Option<&'a serde_json::Value>> {
    Ok(openclaw_agent_entries(config)?.into_iter().flatten()
        .find(|(id, _)| id.trim().eq_ignore_ascii_case(agent_id.trim())).map(|(_, entry)| entry))
}

pub(crate) fn openclaw_agent_dir(config: &serde_json::Value, agent_id: &str, state: &Path, home: &Path) -> Result<PathBuf> {
    openclaw_agent_dir_from_entry(openclaw_agent_entry(config, agent_id)?, agent_id, state, home)
}

fn openclaw_agent_dir_from_entry(entry: Option<&serde_json::Value>, agent_id: &str, state: &Path, home: &Path) -> Result<PathBuf> {
    if let Some(value) = entry.and_then(|entry| entry.get("agentDir")) {
        let raw = value.as_str().ok_or_else(|| anyhow::anyhow!("OpenClaw agentDir 字段格式无效"))?;
        if !raw.trim().is_empty() { return openclaw_user_path(raw, home); }
    }
    Ok(state.join("agents").join(agent_id.to_ascii_lowercase()).join("agent"))
}

/// A native workspace belongs to one agentId, even when two ids use one path.
pub(crate) fn openclaw_workspace_routes(
    config: &serde_json::Value, state: &Path, home: &Path,
) -> Result<Vec<(String, PathBuf)>> {
    use serde_json::Value;
    ensure!(config.is_object(), "OpenClaw 配置根必须是对象");
    let agents = config.get("agents");
    ensure!(agents.is_none_or(Value::is_object), "OpenClaw agents 配置结构无效");
    let defaults = agents.and_then(|v| v.get("defaults"));
    ensure!(defaults.is_none_or(Value::is_object), "OpenClaw agents.defaults 结构无效");
    let workspace = |value: Option<&Value>| -> Result<Option<PathBuf>> {
        value.map(|v| {
            let text = v.as_str().ok_or_else(|| anyhow::anyhow!("OpenClaw workspace 字段格式无效"))?;
            if text.trim().is_empty() { Ok(None) } else { openclaw_user_path(text, home).map(Some) }
        }).transpose().map(Option::flatten)
    };
    let base = workspace(defaults.and_then(|v| v.get("workspace")))?;
    let default_path = || -> Result<PathBuf> {
        std::env::var_os("OPENCLAW_WORKSPACE_DIR").filter(|v| !v.is_empty())
            .map(|raw| openclaw_user_path(&raw.to_string_lossy(), home))
            .unwrap_or_else(|| Ok(state.join("workspace")))
    };
    let Some(entries) = openclaw_agent_entries(config)? else {
        return Ok(vec![("main".into(), base.map(Ok).unwrap_or_else(default_path)?)]);
    };
    let marked = entries.iter().filter(|(_, v)| v.get("default").and_then(Value::as_bool) == Some(true))
        .map(|(id, _)| *id).collect::<Vec<_>>();
    let legacy_owner = if agents.and_then(|v| v.get("ownership")).and_then(Value::as_str) != Some("explicit") && marked.len() == 1 {
        Some(marked[0])
    } else if entries.len() == 1 { entries.first().map(|(id, _)| *id) } else { None };
    let mut seen = std::collections::BTreeSet::new();
    entries.iter().map(|(id, entry)| {
        ensure!(!id.is_empty() && id.len() <= 64 && id.bytes().all(|c|
            c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-')) && entry.is_object(), "OpenClaw agent id 或条目无效");
        // Native normalizeAgentId lowercases valid ids. Keep that identity in source_id.
        let id = id.to_ascii_lowercase();
        ensure!(seen.insert(id.clone()), "OpenClaw agent id 重复");
        openclaw_agent_dir_from_entry(Some(entry), &id, state, home)?;
        let path = if let Some(path) = workspace(entry.get("workspace"))? { path }
            else if legacy_owner.is_some_and(|owner| owner.eq_ignore_ascii_case(&id)) {
                base.clone().map(Ok).unwrap_or_else(default_path)?
            } else if let Some(base) = &base { base.join(&id) }
            else { state.join(format!("workspace-{id}")) };
        Ok((id, path))
    }).collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OpenClawConfigFailure { Missing, Unreadable, Invalid }

impl OpenClawConfigFailure {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Missing => "openclaw_config_missing",
            Self::Unreadable => "openclaw_config_unreadable",
            Self::Invalid => "openclaw_config_invalid",
        }
    }
    pub(crate) fn message(&self) -> &'static str {
        match self {
            Self::Missing => "OpenClaw 配置缺失，技能生效状态无法确认",
            Self::Unreadable => "OpenClaw 配置不可读，技能生效状态无法确认",
            Self::Invalid => "OpenClaw 配置无效，技能生效状态无法确认",
        }
    }
}

/// One observation supplies roots, disabled entries and trust for a whole scan.
/// A failed read is never an empty configuration or an empty native inventory.
pub(crate) struct OpenClawSnapshot {
    pub(crate) config: serde_json::Value,
    pub(crate) state: PathBuf,
    pub(crate) routes: Vec<(String, PathBuf)>,
    pub(crate) effective_home: PathBuf,
}

pub(crate) fn openclaw_snapshot(home: &Path) -> std::result::Result<OpenClawSnapshot, OpenClawConfigFailure> {
    let (state, path) = openclaw_paths(home).map_err(|_| OpenClawConfigFailure::Invalid)?;
    openclaw_snapshot_at(home, &state, &path)
}

pub(crate) fn openclaw_snapshot_at(home: &Path, state: &Path, path: &Path) -> std::result::Result<OpenClawSnapshot, OpenClawConfigFailure> {
    use OpenClawConfigFailure::{Invalid, Missing, Unreadable};
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => return Err(Unreadable),
        Ok(_) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(Missing),
        Err(_) => return Err(Unreadable),
    }
    let config = crate::project_discovery::openclaw_config_at(path).map_err(|error|
        if error.to_string().contains("不可读") { Unreadable } else { Invalid })?;
    let routes = openclaw_workspace_routes(&config, state, home).map_err(|_| Invalid)?;
    validate_openclaw_scan_config(&config).map_err(|_| Invalid)?;
    Ok(OpenClawSnapshot { config, state: state.to_path_buf(), routes,
        effective_home: openclaw_effective_home(home).map_err(|_| Invalid)? })
}

fn validate_openclaw_scan_config(config: &serde_json::Value) -> Result<()> {
    for pointer in ["/skills", "/skills/load", "/skills/entries", "/plugins", "/plugins/entries", "/plugins/load"] {
        ensure!(config.pointer(pointer).is_none_or(|value| value.is_object()), "OpenClaw 配置结构无效");
    }
    for pointer in ["/skills/allowBundled", "/skills/load/allowSymlinkTargets", "/skills/load/extraDirs", "/plugins/allow", "/plugins/deny", "/plugins/load/paths"] {
        ensure!(config.pointer(pointer).is_none_or(|value| value.as_array()
            .is_some_and(|items| items.iter().all(|item| item.is_string()))), "OpenClaw 配置列表结构无效");
    }
    for pointer in ["/skills/entries", "/plugins/entries"] {
        for (_, entry) in config.pointer(pointer).and_then(|value| value.as_object()).into_iter().flatten() {
            ensure!(entry.is_object() && entry.get("enabled").is_none_or(|value| value.is_boolean()), "OpenClaw 启用状态结构无效");
        }
    }
    ensure!(config.pointer("/plugins/enabled").is_none_or(|value| value.is_boolean()), "OpenClaw 插件启用状态结构无效");
    Ok(())
}

/// Match the pinned native loader: trust is a string[] of real target roots,
/// default [], with descendant containment. No boolean/global opt-in exists.
pub(crate) fn openclaw_target_is_trusted(
    root: &Path, source: &Path, config: &serde_json::Value, home: &Path,
) -> bool {
    let Ok(source) = source.canonicalize() else { return false; };
    if root.canonicalize().is_ok_and(|root| source.starts_with(root)) { return true; }
    let targets = config.pointer("/skills/load/allowSymlinkTargets").and_then(|v| v.as_array());
    if targets.is_some_and(|items| items.iter().any(|v| !v.is_string())) { return false; }
    targets
        .into_iter().flatten().filter_map(|v| v.as_str())
        .filter_map(|raw| openclaw_user_path(raw, home).ok()?.canonicalize().ok())
        .any(|trusted| source.starts_with(trusted))
}

/// Validate before both preview and execution, so revoking trust cannot leave a
/// newly installed but natively rejected link. Uninstall never needs trust.
pub(crate) fn validate_openclaw_link(root: &Path, source: &Path) -> Result<()> {
    let source = source.canonicalize()?;
    ensure!(source.is_dir() && source.join("SKILL.md").is_file() && source.join("SKILL.md").canonicalize()?.starts_with(&source),
        "OpenClaw SKILL.md 必须留在技能实体内");
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("无法取得 OpenClaw 用户目录"))?;
    let (state, config_path) = openclaw_paths(&home)?;
    if root == state.join("skills") {
        return Ok(()); // Native managed root admits external directory targets.
    }
    let snapshot = openclaw_snapshot_at(&home, &state, &config_path)
        .map_err(|failure| anyhow::anyhow!("{}", failure.message()))?;
    ensure!(openclaw_target_is_trusted(root, &source, &snapshot.config, &home),
        "OpenClaw 工作区链接目标尚未被 skills.load.allowSymlinkTargets 信任；请由主控核对原生配置与加载边界，现有信任配置未修改");
    Ok(())
}

fn workbuddy_env(name: &str) -> Result<Option<String>> {
    std::env::var_os(name).map(|value| value.into_string()
        .map_err(|_| anyhow::anyhow!("WorkBuddy 目录配置编码无效"))).transpose()
}

// WorkBuddy 5.8.2 cli/product.json dataFolderName, bound by official ASAR
// b7dee8481e5106f6cfcea1b20cf66c07a103be793333ed45b164aede41cdcce0.
// This desktop DB default is independent of the personal-skills instance root.
const WORKBUDDY_DESKTOP_DATA_FOLDER_NAME: &str = ".workbuddy";

fn workbuddy_desktop_trim(value: &str) -> &str {
    // Match the host's ECMAScript String.trim(), including BOM but not U+0085.
    value.trim_matches(|c| matches!(c, '\u{0009}'..='\u{000D}' | ' ' | '\u{00A0}' | '\u{1680}'
        | '\u{2000}'..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}'
        | '\u{3000}' | '\u{FEFF}'))
}

pub(crate) fn workbuddy_desktop_database_root(home: &Path) -> Result<PathBuf> {
    let configured = workbuddy_env("WORKBUDDY_CONFIG_DIR")?;
    if configured.as_deref().is_some_and(|value| !workbuddy_desktop_trim(value).is_empty()) {
        return workbuddy_desktop_database_root_from(home, configured.as_deref(), None);
    }
    let codebuddy = workbuddy_env("CODEBUDDY_CONFIG_DIR")?;
    if codebuddy.as_deref().is_some_and(|value| !workbuddy_desktop_trim(value).is_empty()) {
        return workbuddy_desktop_database_root_from(home, None, codebuddy.as_deref());
    }
    workbuddy_desktop_database_root_from(home, None, None)
}

pub(crate) fn workbuddy_desktop_database_root_from(
    home: &Path, configured: Option<&str>, codebuddy: Option<&str>,
) -> Result<PathBuf> {
    let selected = configured.map(workbuddy_desktop_trim).filter(|value| !value.is_empty())
        .or_else(|| codebuddy.map(workbuddy_desktop_trim).filter(|value| !value.is_empty()));
    if let Some(value) = selected {
        ensure!(Path::new(value).is_absolute() && !value.contains('\0'),
            "WorkBuddy 桌面项目索引配置根无效，状态未知");
        return Ok(PathBuf::from(value));
    }
    // The verified desktop producer does not read the personal instance hint.
    ensure!(home.is_absolute() && !home.as_os_str().as_encoded_bytes().contains(&0),
        "WorkBuddy 桌面项目索引用户目录无效，状态未知");
    Ok(home.join(WORKBUDDY_DESKTOP_DATA_FOLDER_NAME))
}

pub(crate) fn workbuddy_config_root(home: &Path) -> Result<PathBuf> {
    workbuddy_config_root_from(home, workbuddy_env("WORKBUDDY_CONFIG_DIR")?.as_deref(),
        workbuddy_env("WORKBUDDY_INSTANCE_NUMBER")?.as_deref())
}

pub(crate) fn workbuddy_config_root_from(home: &Path, configured: Option<&str>, instance: Option<&str>) -> Result<PathBuf> {
    if let Some(value) = configured.map(str::trim).filter(|value| !value.is_empty()) {
        let path = Path::new(value);
        ensure!(!value.is_empty() && path.is_absolute(), "WorkBuddy 配置目录必须是非空绝对路径");
        return Ok(path.to_path_buf());
    }
    ensure!(home.is_absolute(), "WorkBuddy 用户目录必须是绝对路径");
    let instance = instance.map(str::trim).filter(|value| !value.is_empty());
    if let Some(instance) = instance {
        ensure!(instance.len() <= 64 && instance.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_')),
            "WorkBuddy 实例目录配置无效");
        Ok(home.join(format!(".workbuddy-{instance}")))
    } else { Ok(home.join(".workbuddy")) }
}

pub(crate) fn workbuddy_project_folder(value: Option<&str>) -> Result<&str> {
    let value = value.unwrap_or(".workbuddy");
    ensure!(!value.is_empty() && !value.contains(['/', '\\']) && !value.chars().any(char::is_control)
        && matches!(Path::new(value).components().next(), Some(std::path::Component::Normal(_)))
        && Path::new(value).components().count() == 1, "WorkBuddy 项目目录配置必须是单个目录名");
    Ok(value)
}

pub(crate) fn project_target_directory(target: InstallTarget) -> Result<PathBuf> {
    if target == InstallTarget::WorkBuddy {
        let configured = workbuddy_env("WORKBUDDY_DATA_FOLDER_NAME")?;
        Ok(Path::new(workbuddy_project_folder(configured.as_deref())?).join("skills"))
    } else { Ok(PathBuf::from(target_directory(target))) }
}

/// OMP profiles use separate agent directories; the default profile honors Pi's
/// shared environment variable, but keeps its own default directory.
pub(crate) fn omp_agent_dir(home: &Path) -> PathBuf {
    let profile = std::env::var("OMP_PROFILE")
        .or_else(|_| std::env::var("PI_PROFILE"))
        .unwrap_or_default();
    let profile = profile.trim();
    if !profile.is_empty()
        && profile != "default"
        && profile.len() <= 64
        && profile
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && profile.bytes().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_' | b'.')
        })
    {
        return home.join(".omp/profiles").join(profile).join("agent");
    }
    std::env::var_os("PI_CODING_AGENT_DIR")
        .map(|value| {
            let value = value.to_string_lossy();
            value.strip_prefix("~/").map_or_else(
                || PathBuf::from(value.as_ref()),
                |relative| home.join(relative),
            )
        })
        .unwrap_or_else(|| home.join(".omp/agent"))
}

pub fn installation_root(project: &Path, target: InstallTarget) -> Result<PathBuf> {
    if target != InstallTarget::Proma && dirs::home_dir().is_some_and(|home|
        crate::project_discovery::path_identity(&home) == crate::project_discovery::path_identity(project)) {
        global_target_root(project, target)
    } else if target == InstallTarget::Hermes {
        let home = dirs::home_dir().unwrap_or_default();
        Ok(hermes_project_root(project, &home)?.unwrap_or_else(|| project.to_path_buf()).join(".hermes/skills"))
    } else {
        Ok(project.join(project_target_directory(target)?))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstallTargetInfo {
    pub target: InstallTarget,
    pub name: &'static str,
    pub icon_path: &'static str,
}

/// Every project directory Kitter can create, in the order used by project
/// snapshots and the install UI.  `Universal` is the shared compatibility
/// directory; the remaining entries are provider-specific project roots.
/// Proma is handled separately, only for registered native workspaces. In
/// particular, plain `skills` is the native OpenClaw workspace target.
pub const PROJECT_INSTALL_TARGETS: &[InstallTarget] = &[
    InstallTarget::Universal,
    InstallTarget::Codex,
    InstallTarget::ClaudeCode,
    InstallTarget::Cursor,
    InstallTarget::OpenCode,
    InstallTarget::Pi,
    InstallTarget::Omp,
    InstallTarget::Grok,
    InstallTarget::Antigravity,
    InstallTarget::Droid,
    InstallTarget::Copilot,
    InstallTarget::Trae,
    InstallTarget::WorkBuddy,
    InstallTarget::OpenClaw,
    InstallTarget::Hermes,
];

pub const INDEPENDENT_INSTALL_TARGETS: &[InstallTargetInfo] = &[
    InstallTargetInfo {
        target: InstallTarget::Codex,
        name: "Codex",
        icon_path: "icons/provider-codex.svg",
    },
    InstallTargetInfo {
        target: InstallTarget::ClaudeCode,
        name: "Claude Code",
        icon_path: "icons/provider-claude.svg",
    },
    InstallTargetInfo {
        target: InstallTarget::Cursor,
        name: "Cursor",
        icon_path: "icons/provider-cursor.svg",
    },
    InstallTargetInfo {
        target: InstallTarget::OpenCode,
        name: "OpenCode",
        icon_path: "icons/provider-opencode.svg",
    },
    InstallTargetInfo {
        target: InstallTarget::Pi,
        name: "Pi",
        icon_path: "icons/provider-pi.svg",
    },
    InstallTargetInfo {
        target: InstallTarget::Proma,
        name: "Proma",
        icon_path: "icons/provider-proma-black.png",
    },
    InstallTargetInfo {
        target: InstallTarget::Omp,
        name: "omp",
        icon_path: "icons/provider-omp.svg",
    },
    InstallTargetInfo {
        target: InstallTarget::Grok,
        name: "Grok",
        icon_path: "icons/provider-grok.svg",
    },
    InstallTargetInfo {
        target: InstallTarget::Antigravity,
        name: "Antigravity",
        icon_path: "icons/provider-antigravity.svg",
    },
    InstallTargetInfo {
        target: InstallTarget::Droid,
        name: "Droid",
        icon_path: "icons/provider-droid.svg",
    },
    InstallTargetInfo {
        target: InstallTarget::Copilot,
        name: "GitHub Copilot",
        icon_path: "icons/provider-copilot.svg",
    },
    InstallTargetInfo {
        target: InstallTarget::Trae,
        name: "Trae",
        icon_path: "icons/provider-trae.svg",
    },
    InstallTargetInfo {
        target: InstallTarget::WorkBuddy,
        name: "WorkBuddy",
        icon_path: "icons/provider-workbuddy.svg",
    },
    InstallTargetInfo {
        target: InstallTarget::OpenClaw,
        name: "OpenClaw",
        icon_path: "icons/provider-openclaw.svg",
    },
    InstallTargetInfo {
        target: InstallTarget::Hermes,
        name: "Hermes",
        icon_path: "icons/provider-hermes.svg",
    },
];

/// Presentation metadata for the Agent badges used throughout the desktop UI.
///
/// An icon can describe an Agent that consumes the shared `.agents/skills`
/// directory without being an install target that Kitter manages directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentIconInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub icon_path: &'static str,
    /// Provider-specific project targets this Agent can consume.  A provider
    /// can list more than one because compatibility directories are shared;
    /// for example Cursor also reads `.claude/skills` and `.codex/skills`.
    pub install_targets: &'static [InstallTarget],
    /// Whether this Agent consumes Kitter's shared `.agents/skills` target.
    /// This is independent from `install_targets`: Pi has both a shared target
    /// and its own `.pi/skills` target.
    pub shared_agents: bool,
    pub global_only: bool,
}

impl AgentIconInfo {
    pub fn owns_target(self, target: InstallTarget) -> bool {
        let id = match target {
            InstallTarget::Universal => return false,
            InstallTarget::Codex => "codex",
            InstallTarget::ClaudeCode => "claude-code",
            InstallTarget::Cursor => "cursor",
            InstallTarget::OpenCode => "opencode",
            InstallTarget::Pi => "pi",
            InstallTarget::Proma => "proma",
            InstallTarget::Omp => "omp",
            InstallTarget::Grok => "grok",
            InstallTarget::Antigravity => "antigravity",
            InstallTarget::Droid => "droid",
            InstallTarget::Copilot => "copilot",
            InstallTarget::Trae => "trae",
            InstallTarget::WorkBuddy => "workbuddy",
            InstallTarget::OpenClaw => "openclaw",
            InstallTarget::Hermes => "hermes",
        };
        self.id == id
    }

    pub fn supports_target(self, target: InstallTarget) -> bool {
        self.install_targets.contains(&target)
            || (target == InstallTarget::Universal && self.shared_agents)
    }
}

/// Supported Agent metadata. Display order uses `display_sort_key` rather
/// than this catalog's declaration order.
pub const AGENT_ICON_ORDER: &[AgentIconInfo] = &[
    AgentIconInfo {
        id: "codex",
        name: "Codex",
        icon_path: "icons/provider-codex.svg",
        install_targets: &[InstallTarget::Codex],
        shared_agents: true,
        global_only: false,
    },
    AgentIconInfo {
        id: "claude-code",
        name: "Claude Code",
        icon_path: "icons/provider-claude.svg",
        install_targets: &[InstallTarget::ClaudeCode],
        shared_agents: false,
        global_only: false,
    },
    AgentIconInfo {
        id: "cursor",
        name: "Cursor",
        icon_path: "icons/provider-cursor.svg",
        install_targets: &[
            InstallTarget::Cursor,
            InstallTarget::ClaudeCode,
            InstallTarget::Codex,
        ],
        shared_agents: true,
        global_only: false,
    },
    AgentIconInfo {
        id: "opencode",
        name: "OpenCode",
        icon_path: "icons/provider-opencode.svg",
        install_targets: &[InstallTarget::OpenCode, InstallTarget::ClaudeCode],
        shared_agents: true,
        global_only: false,
    },
    AgentIconInfo {
        id: "pi",
        name: "Pi",
        icon_path: "icons/provider-pi.svg",
        install_targets: &[InstallTarget::Pi],
        shared_agents: true,
        global_only: false,
    },
    AgentIconInfo {
        id: "proma",
        name: "Proma",
        icon_path: "icons/provider-proma-black.png",
        install_targets: &[InstallTarget::Proma],
        shared_agents: false,
        global_only: false,
    },
    AgentIconInfo {
        id: "omp",
        name: "omp",
        icon_path: "icons/provider-omp.svg",
        install_targets: &[
            InstallTarget::Omp,
            InstallTarget::ClaudeCode,
            InstallTarget::Codex,
            InstallTarget::OpenCode,
            InstallTarget::Antigravity,
            InstallTarget::Copilot,
        ],
        shared_agents: true,
        global_only: false,
    },
    AgentIconInfo {
        id: "dimagent",
        name: "DimAgent",
        icon_path: "icons/provider-dimagent.svg",
        install_targets: &[],
        shared_agents: true,
        global_only: false,
    },
    AgentIconInfo {
        id: "grok",
        name: "Grok",
        icon_path: "icons/provider-grok.svg",
        install_targets: &[
            InstallTarget::Grok,
            InstallTarget::ClaudeCode,
            InstallTarget::Cursor,
        ],
        shared_agents: true,
        global_only: false,
    },
    AgentIconInfo {
        id: "openclaw",
        name: "OpenClaw",
        icon_path: "icons/provider-openclaw.svg",
        install_targets: &[InstallTarget::OpenClaw],
        shared_agents: true,
        global_only: false,
    },
    AgentIconInfo {
        id: "hermes",
        name: "Hermes",
        icon_path: "icons/provider-hermes.svg",
        install_targets: &[InstallTarget::Hermes],
        shared_agents: true,
        global_only: false,
    },
    AgentIconInfo {
        id: "droid",
        name: "Droid",
        icon_path: "icons/provider-droid.svg",
        install_targets: &[InstallTarget::Droid, InstallTarget::Antigravity],
        shared_agents: true,
        global_only: false,
    },
    AgentIconInfo {
        id: "amp",
        name: "Amp",
        icon_path: "icons/provider-amp.svg",
        install_targets: &[InstallTarget::ClaudeCode],
        shared_agents: true,
        global_only: false,
    },
    AgentIconInfo {
        id: "antigravity",
        name: "Antigravity",
        icon_path: "icons/provider-antigravity.svg",
        install_targets: &[InstallTarget::Antigravity],
        shared_agents: true,
        global_only: false,
    },
    AgentIconInfo {
        id: "copilot",
        name: "GitHub Copilot",
        icon_path: "icons/provider-copilot.svg",
        install_targets: &[InstallTarget::Copilot, InstallTarget::ClaudeCode],
        shared_agents: true,
        global_only: false,
    },
    AgentIconInfo {
        id: "trae",
        name: "Trae",
        icon_path: "icons/provider-trae.svg",
        install_targets: &[InstallTarget::Trae],
        shared_agents: true,
        global_only: false,
    },
    AgentIconInfo {
        id: "workbuddy",
        name: "WorkBuddy",
        icon_path: "icons/provider-workbuddy.svg",
        install_targets: &[InstallTarget::WorkBuddy],
        shared_agents: false,
        global_only: false,
    },
];

pub fn target_directory(target: InstallTarget) -> &'static str {
    match target {
        InstallTarget::Universal => ".agents/skills",
        InstallTarget::Codex => ".codex/skills",
        InstallTarget::ClaudeCode => ".claude/skills",
        InstallTarget::Cursor => ".cursor/skills",
        InstallTarget::OpenCode => ".opencode/skills",
        InstallTarget::Pi => ".pi/skills",
        InstallTarget::Proma => "skills",
        InstallTarget::Omp => ".omp/skills",
        InstallTarget::Grok => ".grok/skills",
        InstallTarget::Antigravity => ".agent/skills",
        InstallTarget::Droid => ".factory/skills",
        InstallTarget::Copilot => ".github/skills",
        InstallTarget::Trae => ".trae/skills",
        InstallTarget::WorkBuddy => ".workbuddy/skills",
        InstallTarget::OpenClaw => "skills",
        InstallTarget::Hermes => ".hermes/skills",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copilot_root_resolver_defaults_normalizes_and_fails_closed() {
        let home = Path::new("/synthetic/copilot-home");
        for configured in [None, Some(""), Some("/synthetic/copilot-home/.copilot/./nested/.." )] {
            assert_eq!(copilot_config_root_from(home, configured).unwrap(), home.join(".copilot"));
        }
        assert_eq!(copilot_config_root_from(home, Some("/synthetic/other home")).unwrap(),
            Path::new("/synthetic/other home"));
        for raw in ["relative", "~", "~/.copilot", " ", " /synthetic/home ", "/synthetic/\0home"] {
            assert!(copilot_config_root_from(home, Some(raw)).is_err());
        }
        assert!(copilot_config_root_from(Path::new("relative-home"), None).is_err());
    }

    #[test]
    fn copilot_relative_selection_requires_verified_absolute_native_origin() {
        let home = Path::new("/synthetic/home");
        for raw in ["relative-profile", "~/literal-profile"] {
            let result = copilot_config_selection_with_origin(home, Some(raw), |input| {
                assert_eq!(input, raw);
                Ok(PathBuf::from("/verified/native/profile"))
            }).unwrap();
            assert_eq!(result, (PathBuf::from("/verified/native/profile"), false));
            for result in [Err(anyhow::anyhow!("native unavailable")), Ok(PathBuf::from("relative"))] {
                assert!(copilot_config_selection_with_origin(home, Some(raw), |_| result).is_err());
            }
        }
        assert!(copilot_config_selection_with_origin(home, Some("bad\0profile"),
            |_| panic!("invalid value reached native origin")).is_err());
    }

    #[test]
    fn copilot_absolute_and_default_selection_never_queries_live_origin() {
        let home = Path::new("/synthetic/home");
        for raw in [None, Some(""), Some("/synthetic/home/.copilot"), Some("/other/profile")] {
            let actual = copilot_config_selection_with_origin(home, raw,
                |_| panic!("static config reached native origin")).unwrap();
            assert_eq!(actual, copilot_config_selection_from(home, raw).unwrap());
        }
    }

    #[test]
    fn copilot_personal_agents_selection_matches_native_before_output_normalization() {
        let home = Path::new("/synthetic/copilot-home");
        for configured in [None, Some(""), Some("/synthetic/copilot-home/.copilot"),
            Some("/synthetic/copilot-home/.copilot/")] {
            let (root, personal) = copilot_config_selection_from(home, configured).unwrap();
            assert_eq!(root, home.join(".copilot"));
            assert!(personal);
        }
        let (root, personal) = copilot_config_selection_from(home,
            Some("/synthetic/copilot-home/.copilot/../.copilot")).unwrap();
        assert_eq!(root, home.join(".copilot"));
        assert!(!personal);
    }

    #[test]
    fn workbuddy_runtime_config_override_trims_and_blank_values_fall_back() {
        let home = Path::new("/synthetic/workbuddy-home");
        assert_eq!(workbuddy_config_root_from(home, Some(" \t/synthetic/custom root \n"), None).unwrap(),
            Path::new("/synthetic/custom root"));
        for configured in [None, Some(""), Some("  \t\n")] {
            assert_eq!(workbuddy_config_root_from(home, configured, None).unwrap(), home.join(".workbuddy"));
            assert_eq!(workbuddy_config_root_from(home, configured, Some(" 2 ")).unwrap(), home.join(".workbuddy-2"));
        }
        assert!(workbuddy_config_root_from(home, Some(" relative-root "), None).is_err());
    }

    #[test]
    fn workbuddy_desktop_database_root_uses_official_precedence_without_fallback() {
        let home = Path::new("/synthetic/workbuddy-home");
        assert_eq!(workbuddy_desktop_database_root_from(home, Some(" /synthetic/wb "),
            Some("relative-ignored")).unwrap(), Path::new("/synthetic/wb"));
        assert_eq!(workbuddy_desktop_database_root_from(home, Some(" \t\n"),
            Some(" /synthetic/codebuddy root/ ")).unwrap(),
            Path::new("/synthetic/codebuddy root/"));
        for invalid in ["relative", "~/.workbuddy", "/synthetic/\0root"] {
            assert!(workbuddy_desktop_database_root_from(home, Some(invalid),
                Some("/synthetic/valid-lower-priority")).is_err());
            assert!(workbuddy_desktop_database_root_from(home, None, Some(invalid)).is_err());
        }
    }

    #[test]
    fn workbuddy_desktop_database_root_matches_ecmascript_blank_handling() {
        let home = Path::new("/synthetic/workbuddy-home");
        for blank in [None, Some(""), Some(" \t\n\r"), Some("\u{FEFF}\u{00A0}\u{2028}\u{2029}")] {
            assert_eq!(workbuddy_desktop_database_root_from(home, blank, blank).unwrap(),
                home.join(".workbuddy"));
        }
        assert_eq!(workbuddy_desktop_database_root_from(home,
            Some("\u{FEFF}/synthetic/wb\u{FEFF}"), None).unwrap(), Path::new("/synthetic/wb"));
        assert!(workbuddy_desktop_database_root_from(home, Some("\u{0085}"), None).is_err());
        assert_eq!(workbuddy_desktop_database_root_from(home, Some("/synthetic/wb\u{0085}"),
            None).unwrap(), Path::new("/synthetic/wb\u{0085}"));
    }

    #[test]
    fn workbuddy_desktop_database_root_keeps_default_independent_of_personal_instances() {
        let home = Path::new("/synthetic/workbuddy-home");
        for instance in ["2", "stage-2"] {
            let personal = workbuddy_config_root_from(home, None, Some(instance)).unwrap();
            assert_ne!(personal, home.join(".workbuddy"));
            assert_eq!(workbuddy_desktop_database_root_from(home, None, None).unwrap(),
                home.join(".workbuddy"));
        }
        for home in [Path::new("relative-home"), Path::new("/synthetic/\0home")] {
            assert!(workbuddy_desktop_database_root_from(home, None, None).is_err());
            // An explicit absolute root does not depend on the unused HOME.
            assert_eq!(workbuddy_desktop_database_root_from(home, Some("/synthetic/wb"),
                None).unwrap(), Path::new("/synthetic/wb"));
        }
        assert_eq!(workbuddy_config_root_from(home, None, Some("2")).unwrap(),
            home.join(".workbuddy-2"));
    }

    #[test]
    fn agent_icon_order_matches_usage_priority() {
        let ids = AGENT_ICON_ORDER
            .iter()
            .map(|agent| agent.id)
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec![
                "codex",
                "claude-code",
                "cursor",
                "opencode",
                "pi",
                "proma",
                "omp",
                "dimagent",
                "grok",
                "openclaw",
                "hermes",
                "droid",
                "amp",
                "antigravity",
                "copilot",
                "trae",
                "workbuddy",
            ]
        );
    }

    #[test]
    fn all_agents_have_project_scopes() {
        let global_only = AGENT_ICON_ORDER
            .iter()
            .filter(|agent| agent.global_only)
            .map(|agent| agent.id)
            .collect::<Vec<_>>();
        assert!(global_only.is_empty());
        assert!(
            AGENT_ICON_ORDER
                .iter()
                .filter(|agent| agent.global_only)
                .all(|agent| agent.install_targets.is_empty())
        );
    }

    #[test]
    fn shared_agents_also_have_their_provider_specific_targets() {
        for id in [
            "codex",
            "cursor",
            "opencode",
            "pi",
            "grok",
            "droid",
            "antigravity",
            "copilot",
        ] {
            let agent = AGENT_ICON_ORDER
                .iter()
                .find(|agent| agent.id == id)
                .unwrap();
            assert!(agent.shared_agents, "{id} should consume .agents/skills");
            assert!(
                agent
                    .install_targets
                    .iter()
                    .any(|target| *target != InstallTarget::Universal),
                "{id} should have a provider-specific project target"
            );
        }
    }

    #[test]
    fn display_order_keeps_installed_agents_first_and_each_group_alphabetical() {
        let installed = ["proma", "grok", "codex", "antigravity"];
        let mut agents = AGENT_ICON_ORDER.to_vec();
        agents.sort_by_key(|agent| display_sort_key(agent.name, installed.contains(&agent.id)));
        let names = agents.iter().map(|agent| agent.name).collect::<Vec<_>>();
        assert_eq!(&names[..4], ["Antigravity", "Codex", "Grok", "Proma"]);
        assert_eq!(&names[4..], ["Amp", "Claude Code", "Cursor", "DimAgent", "Droid",
            "GitHub Copilot", "Hermes", "omp", "OpenClaw", "OpenCode", "Pi", "Trae"]);
    }

    #[test]
    fn compatibility_does_not_make_a_provider_an_installed_target() {
        let grok = *AGENT_ICON_ORDER.iter().find(|agent| agent.id == "grok").unwrap();
        assert!(grok.supports_target(InstallTarget::ClaudeCode));
        assert!(!grok.owns_target(InstallTarget::ClaudeCode));
        assert!(grok.owns_target(InstallTarget::Grok));
        for target in INDEPENDENT_INSTALL_TARGETS {
            assert_eq!(AGENT_ICON_ORDER.iter().filter(|agent| agent.owns_target(target.target)).count(), 1);
        }
    }

    #[test]
    fn compatibility_badges_cover_shared_provider_specific_roots() {
        let cursor = AGENT_ICON_ORDER
            .iter()
            .find(|agent| agent.id == "cursor")
            .unwrap();
        assert!(cursor.supports_target(InstallTarget::Universal));
        assert!(cursor.supports_target(InstallTarget::Cursor));
        assert!(cursor.supports_target(InstallTarget::ClaudeCode));
        assert!(cursor.supports_target(InstallTarget::Codex));

        let openclaw = AGENT_ICON_ORDER
            .iter()
            .find(|agent| agent.id == "openclaw")
            .unwrap();
        assert!(openclaw.supports_target(InstallTarget::Universal));
        assert!(openclaw.owns_target(InstallTarget::OpenClaw));
    }
}
