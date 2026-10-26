//! Read-only Hermes profile and project routing, pinned to 5fc490e2b341.
use anyhow::{Context, Result, ensure};
use serde_yaml::Value;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub(crate) struct HermesSnapshot {
    pub home: PathBuf,
    pub project_root: Option<PathBuf>,
    pub project_enabled: bool,
    pub project_trusted: bool,
    /// Ordered project, profile, create_dir, external_dirs roots.
    pub roots: Vec<(PathBuf, u8)>,
    pub skills: Value,
}

/// Explicit HERMES_HOME names a process/profile. Otherwise follow the bare
/// CLI's sticky profile; no process-local ContextVar or CLI flag is invented.
pub(crate) fn hermes_home(home: &Path) -> Result<PathBuf> {
    if let Some(raw) =
        std::env::var_os("HERMES_HOME").filter(|v| !v.to_string_lossy().trim().is_empty())
    {
        let profile = expand_path(
            raw.to_string_lossy().trim(),
            home,
            &std::env::current_dir()?,
        )?;
        validate_named_profile(&profile)?;
        if profile
            .parent()
            .and_then(Path::file_name)
            .is_none_or(|p| p != "profiles")
        {
            let sticky = read_optional(&profile.join("active_profile"), 128)?;
            ensure!(
                sticky
                    .as_deref()
                    .map(|v| v.trim_start_matches('\u{feff}').trim())
                    .is_none_or(|v| v.is_empty() || v == "default"),
                "Hermes 显式 HERMES_HOME 与 sticky profile 存在歧义；当前宿主 profile 未知，请固定实际 profile 路径"
            );
        }
        return Ok(profile);
    }
    let suffix = std::env::var("HERMES_DATA_DIR_SUFFIX").unwrap_or_default();
    ensure!(!suffix.contains(['/', '\\']), "Hermes 数据目录后缀无效");
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("AppData/Local"));
    #[cfg(not(windows))]
    let base = home.to_path_buf();
    let root = base.join(format!(
        "{}{suffix}",
        if cfg!(windows) { "hermes" } else { ".hermes" }
    ));
    let marker = read_optional(&root.join("active_profile"), 128)?;
    let profile = marker
        .as_deref()
        .unwrap_or("")
        .trim_start_matches('\u{feff}')
        .trim();
    if profile.is_empty() || profile == "default" {
        return Ok(root);
    }
    ensure!(
        profile.len() <= 64
            && profile
                .bytes()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            && profile
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-')),
        "Hermes profile 名称无效"
    );
    let selected = root.join("profiles").join(profile);
    validate_named_profile(&selected)?;
    Ok(selected)
}

fn validate_named_profile(profile: &Path) -> Result<()> {
    if profile
        .parent()
        .and_then(Path::file_name)
        .is_none_or(|p| p != "profiles")
    {
        return Ok(());
    }
    ensure!(profile.is_dir(), "Hermes 当前 profile 不可用，状态未知");
    let parent = profile.parent().context("Hermes profile 路径无效")?;
    ensure!(
        !parent
            .join(".deleted")
            .join(profile.file_name().unwrap())
            .exists(),
        "Hermes 当前 profile 已删除，状态未知"
    );
    // Metadata only, including credential markers: never open their contents.
    ensure!(
        [
            "config.yaml",
            ".env",
            "SOUL.md",
            "profile.yaml",
            "auth.json",
            "state.db"
        ]
        .iter()
        .any(|name| profile
            .join(name)
            .symlink_metadata()
            .is_ok_and(|m| m.is_file() || m.file_type().is_symlink())),
        "Hermes 当前 profile 缺少原生身份标记，状态未知"
    );
    Ok(())
}

fn existing_dir(path: &Path) -> Result<bool> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound && path.symlink_metadata().is_err() =>
        {
            Ok(false)
        }
        Err(_) => anyhow::bail!("Hermes 技能根不可读，状态未知"),
    }
}

fn read_optional(path: &Path, limit: u64) -> Result<Option<String>> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound && path.symlink_metadata().is_err() =>
        {
            return Ok(None);
        }
        Err(_) => anyhow::bail!("Hermes 配置或 profile 元数据不可读，状态未知"),
    };
    ensure!(file.metadata()?.is_file(), "Hermes 元数据不是普通文件，状态未知");
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("Hermes 元数据读取失败，状态未知"))?;
    ensure!(
        bytes.len() as u64 <= limit,
        "Hermes 元数据超出读取上限，状态未知"
    );
    Ok(Some(String::from_utf8(bytes).map_err(|_| {
        anyhow::anyhow!("Hermes 元数据编码无效，状态未知")
    })?))
}

/// Native expandvars + expanduser; relative trust is cwd-relative, whereas
/// external_dirs/create_dir are profile-relative. Never echo config values.
pub(crate) fn expand_path(raw: &str, home: &Path, base: &Path) -> Result<PathBuf> {
    let mut value = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '$' {
            value.push(ch);
            continue;
        }
        let mut name = String::new();
        if chars.peek() == Some(&'{') {
            chars.next();
            let mut closed = false;
            for c in chars.by_ref() {
                if c == '}' {
                    closed = true;
                    break;
                }
                name.push(c);
            }
            ensure!(
                closed && !name.is_empty(),
                "Hermes 路径变量无法解析，状态未知"
            );
        } else {
            while chars
                .peek()
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
            {
                name.push(chars.next().unwrap());
            }
        }
        ensure!(!name.is_empty(), "Hermes 路径变量无法解析，状态未知");
        let replacement = std::env::var_os(name).context("Hermes 路径变量缺失，状态未知")?;
        value.push_str(&replacement.to_string_lossy());
    }
    let path = if value == "~" {
        home.to_path_buf()
    } else if let Some(part) = value.strip_prefix("~/") {
        home.join(part)
    } else {
        PathBuf::from(value)
    };
    ensure!(!path.as_os_str().is_empty(), "Hermes 路径为空，状态未知");
    Ok(crate::project_discovery::path_identity(
        &if path.is_absolute() {
            path
        } else {
            base.join(path)
        },
    ))
}

pub(crate) fn hermes_project_root(cwd: &Path, home: &Path) -> Result<Option<PathBuf>> {
    let mut current = cwd
        .canonicalize()
        .context("Hermes 当前工作目录不可读，状态未知")?;
    let home = crate::project_discovery::path_identity(home);
    for _ in 0..64 {
        match fs::metadata(current.join(".git")) {
            Ok(_) => return Ok((current != home).then_some(current)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => anyhow::bail!("Hermes Git 根不可读，状态未知"),
        }
        if !current.pop() {
            return Ok(None);
        }
    }
    Ok(None)
}

fn string_list(value: Option<&Value>) -> Result<Vec<String>> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(vec![s.trim().to_owned()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect()),
        Some(Value::Sequence(values)) => values
            .iter()
            .map(|v| {
                v.as_str()
                    .map(|s| s.trim().to_owned())
                    .context("Hermes 技能路径列表格式不支持，状态未知")
            })
            .filter(|v| v.as_ref().map_or(true, |s| !s.is_empty()))
            .collect(),
        _ => anyhow::bail!("Hermes 技能路径配置格式不支持，状态未知"),
    }
}

pub(crate) fn hermes_snapshot(home: &Path, cwd: &Path) -> Result<HermesSnapshot> {
    let profile = hermes_home(home)?;
    let content = read_optional(&profile.join("config.yaml"), 2 * 1024 * 1024)?;
    let config: Value = match content {
        Some(content) => serde_yaml::from_str(content.trim_start_matches('\u{feff}'))
            .map_err(|_| anyhow::anyhow!("Hermes config.yaml 无法解析，状态未知"))?,
        None => Value::Null,
    };
    ensure!(
        config.is_null() || config.is_mapping(),
        "Hermes 配置结构不支持，状态未知"
    );
    let skills = config.get("skills").cloned().unwrap_or(Value::Null);
    ensure!(
        skills.is_null() || skills.is_mapping(),
        "Hermes skills 配置结构不支持，状态未知"
    );
    let project_enabled = skills.get("project_discovery").and_then(Value::as_bool) != Some(false);
    let project_root = if crate::project_discovery::path_identity(cwd)
        == crate::project_discovery::path_identity(home)
    {
        None
    } else {
        hermes_project_root(cwd, home)?
    };
    let trusted = string_list(skills.get("trusted_project_dirs"))?
        .into_iter()
        .map(|p| expand_path(&p, home, cwd))
        .collect::<Result<Vec<_>>>()?;
    let project_trusted = project_root
        .as_ref()
        .is_some_and(|root| trusted.contains(root));
    let local = crate::project_discovery::path_identity(&profile.join("skills"));
    let mut roots = Vec::new();
    if project_enabled && project_trusted {
        for sub in [".hermes/skills", ".agents/skills"] {
            let path =
                crate::project_discovery::path_identity(&project_root.as_ref().unwrap().join(sub));
            if path != local && existing_dir(&path)? {
                roots.push((path, 0));
            }
        }
    }
    roots.push((local, 1));
    if let Some(raw) = skills
        .get("create_dir")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let path = expand_path(raw, home, &profile)?;
        if existing_dir(&path)? && !roots.iter().any(|(p, _)| p == &path) {
            roots.push((path, 2));
        }
    }
    for raw in string_list(skills.get("external_dirs"))? {
        let path = expand_path(&raw, home, &profile)?;
        if existing_dir(&path)? && !roots.iter().any(|(p, _)| p == &path) {
            roots.push((path, 3));
        }
    }
    Ok(HermesSnapshot {
        home: profile,
        project_root,
        project_enabled,
        project_trusted,
        roots,
        skills,
    })
}

/// Re-read at preview AND immediately before mutation. Never grant trust here.
/// Also used by the owning controller's direct project installation entry.
pub fn validate_hermes_project(project: &Path) -> Result<PathBuf> {
    let logical = crate::agents::installation_root(project, crate::InstallTarget::Hermes)?;
    validate_hermes_skill_root(project, &logical)
}

/// Resolve the directory itself, including a root alias. For a not-yet-created
/// root, resolve its existing parent prefix without following a skill entry.
pub(crate) fn hermes_directory_identity(path: &Path) -> Result<PathBuf> {
    match path.canonicalize() {
        Ok(physical) => {
            ensure!(physical.is_dir(), "Hermes 技能根不是目录，状态未知");
            Ok(physical)
        }
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound && path.symlink_metadata().is_err() =>
        {
            crate::cross_project::physical_entry(path)
        }
        Err(_) => anyhow::bail!("Hermes 技能根无法解析，状态未知"),
    }
}

/// Validate the actual planned private OR shared root. Git identity remains
/// separate from the physical skill directory; no root is inferred backwards.
pub(crate) fn validate_hermes_skill_root(project: &Path, selected: &Path) -> Result<PathBuf> {
    let home = dirs::home_dir().context("无法取得 Hermes 用户目录")?;
    let snapshot = hermes_snapshot(&home, project)?;
    ensure!(
        snapshot.project_enabled,
        "Hermes skills.project_discovery 已关闭"
    );
    ensure!(
        snapshot.project_trusted,
        "Hermes 当前 Git 根未列入 skills.trusted_project_dirs；请在原生宿主授予信任后刷新"
    );
    let git_root = snapshot
        .project_root
        .context("Hermes 当前工作目录没有原生 Git 项目根")?;
    let physical = hermes_directory_identity(selected)?;
    ensure!(
        [".hermes/skills", ".agents/skills"]
            .iter()
            .any(|sub| hermes_directory_identity(&git_root.join(sub))
                .is_ok_and(|root| root == physical)),
        "Hermes 当前 Git 根或目标目录已变化，请刷新后重新预览"
    );
    let global = hermes_directory_identity(&snapshot.home.join("skills"))?;
    // Component containment, not string prefixes: skills-other stays distinct.
    // Also protect roots that would include the profile skills namespace.
    ensure!(
        !physical.starts_with(&global) && !global.starts_with(&physical),
        "Hermes 项目目标与全局 profile 技能范围重叠，已保护"
    );
    Ok(physical)
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn fifo_metadata_is_rejected_without_a_blocking_read() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.yaml");
        use std::os::unix::ffi::OsStrExt;
        let raw = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);
        assert!(super::read_optional(&path, 1024).is_err());
    }
}
