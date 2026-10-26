//! Proma workspace installations. Skill entries are real packages; local data
//! remains at its existing source and is never copied into a workspace.
pub mod builtin_sync;
use crate::{
    package_fs::Directory,
    update_protection::{Manifest, Node},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const MARKER: &str = ".kitter-proma.json";
const RECOVERY: &str = ".kitter-proma-recovery";
const LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub project_root_path: Option<PathBuf>,
    // Display metadata never changes existing installation/recovery records.
    #[serde(skip)]
    pub archived: bool,
}

impl PartialEq for Workspace {
    fn eq(&self, other: &Self) -> bool {
        // Finder associations are display metadata, not transaction identity.
        // Recovery records written before this field existed remain valid.
        self.id == other.id && self.slug == other.slug
            && self.name == other.name && self.path == other.path
    }
}

#[derive(Deserialize)]
struct Index {
    version: u64,
    workspaces: Vec<IndexedWorkspace>,
}
#[derive(Deserialize)]
struct IndexedWorkspace {
    id: String,
    slug: String,
    name: String,
    #[serde(default, rename = "projectRootPath")]
    project_root_path: Option<PathBuf>,
    #[serde(default)]
    archived: bool,
}

pub fn home() -> Option<PathBuf> {
    std::env::var_os("KITTER_PROMA_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|p| p.join(".proma")))
}

fn real_directory(path: &Path) -> Result<()> {
    let m = fs::symlink_metadata(path).context("无法检查 Proma 实体目录")?;
    if !m.is_dir() || m.file_type().is_symlink() {
        bail!("Proma 目录必须是实体目录：{}", path.display());
    }
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let parent = Directory::open_anchored(path.parent().context("文件缺少父目录")?)?;
    let mut bytes = Vec::new();
    parent
        .read(Path::new(path.file_name().context("文件缺少名称")?))?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        bail!("Proma 元数据过大");
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("Proma 元数据格式无效：{}", path.display()))
}

pub fn workspaces(root: &Path) -> Result<Vec<Workspace>> {
    if !root.is_absolute() {
        bail!("Proma 配置根必须是绝对路径");
    }
    if !root.exists() {
        return Ok(vec![]);
    }
    real_directory(root)?;
    let index_path = root.join("agent-workspaces.json");
    if !index_path.exists() {
        return Ok(vec![]);
    }
    let index: Index = read_json(&index_path)?;
    if !matches!(index.version, 1 | 2) || index.workspaces.len() > 1000 {
        bail!("不支持的 Proma 工作区索引");
    }
    let root = root.canonicalize()?;
    let parent = root.join("agent-workspaces");
    real_directory(&parent)?;
    let mut ids = BTreeSet::new();
    let mut slugs = BTreeSet::new();
    let mut out = vec![];
    for entry in index.workspaces {
        crate::library::validate_name(&entry.slug)?;
        if entry.id.trim().is_empty()
            || entry.name.trim().is_empty()
            || !ids.insert(entry.id.clone())
            || !slugs.insert(entry.slug.to_lowercase())
        {
            bail!("Proma 工作区身份无效或重复");
        }
        let path = parent.join(&entry.slug);
        // Discovery does not initialize missing workspaces.
        if !path.exists() {
            continue;
        }
        real_directory(&path)?;
        let project_root_path = entry.project_root_path
            .filter(|project| project.is_absolute() && project.is_dir())
            .and_then(|project| project.canonicalize().ok());
        out.push(Workspace {
            id: entry.id,
            slug: entry.slug,
            name: entry.name,
            path,
            project_root_path,
            archived: entry.archived,
        });
    }
    Ok(out)
}

/// Display identity may be a Finder project; all Skill operations retain the
/// registered native workspace path returned here.
pub fn workspaces_for_project(root: &Path, project: &Path) -> Result<Vec<Workspace>> {
    if !project.is_absolute() || !project.is_dir() {
        return Ok(Vec::new());
    }
    let identity = project.canonicalize()?;
    let workspaces = workspaces(root)?;
    if let Some(native) = workspaces.iter().find(|workspace| workspace.path == identity) {
        return Ok(vec![native.clone()]);
    }
    Ok(workspaces.into_iter()
        .filter(|workspace| workspace.project_root_path.as_ref() == Some(&identity))
        .collect())
}

pub fn project_workspaces(project: &Path) -> Result<Vec<Workspace>> {
    match home() {
        Some(root) => workspaces_for_project(&root, project),
        None => Ok(Vec::new()),
    }
}

/// Mutations must select exactly one workspace. Never fan out an install or
/// target-specific uninstall when several workspaces share a Finder project.
pub fn workspace_for_project(root: &Path, project: &Path) -> Result<Workspace> {
    let mut matches = workspaces_for_project(root, project)?;
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => bail!("项目没有关联已登记的 Proma 原生工作区"),
        _ => bail!("项目关联多个 Proma 工作区，请指定一个原生工作区目录"),
    }
}

pub fn installation_workspace(project: &Path) -> Result<Workspace> {
    let root = home().context("无法定位 Proma 配置根")?;
    workspace_for_project(&root, project)
}

pub fn workspace_for_path(root: &Path, project: &Path) -> Result<Workspace> {
    real_directory(project)?;
    let identity = project.canonicalize()?;
    workspaces(root)?
        .into_iter()
        .find(|w| w.path == identity)
        .context("目录不是已注册的 Proma 原生工作区")
}

pub fn is_workspace(project: &Path) -> bool {
    home().is_some_and(|root| workspace_for_path(&root, project).is_ok())
}

fn configured_workspace(project: &Path) -> Result<(PathBuf, Workspace)> {
    let root = home().context("无法定位 Proma 配置根")?;
    let w = workspace_for_path(&root, project)?;
    Ok((root.canonicalize()?, w))
}

fn path_workspace(path: &Path) -> Result<(PathBuf, Workspace)> {
    let parent = path.parent().context("安装位置缺少父目录")?;
    if !matches!(
        parent.file_name().and_then(|x| x.to_str()),
        Some("skills" | "skills-inactive")
    ) {
        bail!("不是 Proma 技能安装位置");
    }
    let project = parent.parent().context("安装位置缺少工作区")?;
    configured_workspace(project)
}

fn signature(path: &Path) -> Result<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = fs::symlink_metadata(path)?;
        Ok(format!("{}:{}", m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        Ok(format!("{:?}", fs::symlink_metadata(path)?.created()?))
    }
}

fn hash<T: Serialize>(value: &T) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

fn excluded(path: &str) -> bool {
    path.split('/').any(|component| {
        let lower = component.to_ascii_lowercase();
        let s = lower.as_str();
        matches!(
            s,
            "local-data"
                | ".git"
                | ".ds_store"
                | "node_modules"
                | ".venv"
                | "venv"
                | "__pycache__"
                | ".cache"
                | ".pytest_cache"
                | MARKER
        ) || s == ".env"
            || (s.starts_with(".env.")
                && !matches!(s, ".env.example" | ".env.sample" | ".env.template"))
    })
}

fn public_manifest(source: &Path) -> Result<Manifest> {
    let mut m = Directory::open_anchored(source)?.installation_snapshot()?;
    m.retain(|p, _| !excluded(p));
    // Structure-only parent packages need explicit materialization; never copy
    // a package link that could import an unrelated resource or private tree.
    if m.values().any(|n| matches!(n, Node::Symlink { .. })) {
        bail!("技能包含链接，需要先明确其资源范围；未复制内容");
    }
    if !matches!(m.get("SKILL.md"), Some(Node::File { .. })) {
        bail!("技能缺少实体 SKILL.md");
    }
    Ok(m)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Marker {
    schema: u32,
    workspace_id: String,
    workspace_slug: String,
    name: String,
    source: PathBuf,
    source_hash: String,
    content_hash: String,
    local_data: Option<PathBuf>,
}

fn marker(path: &Path, w: &Workspace) -> Result<Marker> {
    real_directory(path)?;
    let mut m: Marker = read_json(&path.join(MARKER))?;
    if m.schema != 1
        || m.workspace_id != w.id
        || m.workspace_slug != w.slug
        || path.file_name().and_then(|p| p.to_str()) != Some(&m.name)
        || !m.source.is_absolute()
    {
        bail!("Proma 安装标记与工作区身份不符");
    }
    if let Some(source) = crate::library::storage::migrated_source(&m.source) {
        m.source = source;
    }
    Ok(m)
}

fn content_hash(path: &Path) -> Result<String> {
    let mut m = Directory::open_anchored(path)?.installation_snapshot()?;
    m.remove(MARKER);
    hash(&m)
}

fn verify_local_data(path: &Path, m: &Marker) -> Result<()> {
    let reference = path.join("local-data");
    match &m.local_data {
        Some(expected)
            if fs::symlink_metadata(&reference).is_ok_and(|v| v.file_type().is_symlink())
                && fs::read_link(&reference)? == *expected =>
        {
            Ok(())
        }
        None if fs::symlink_metadata(&reference)
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(())
        }
        _ => bail!("本机数据引用发生变化，保留现场"),
    }
}

fn clean_marker(path: &Path, w: &Workspace) -> Result<Marker> {
    let m = marker(path, w)?;
    if content_hash(path)? != m.content_hash {
        bail!("Proma 工作区副本已被修改，保留修改并拒绝覆盖");
    }
    verify_local_data(path, &m)?;
    Ok(m)
}

fn observed(path: &Path) -> Result<String> {
    let parent = match Directory::open_anchored(path.parent().context("安装位置缺少父目录")?)
    {
        Ok(parent) => parent,
        Err(error) if crate::package_fs::is_not_found(&error) => return Ok("absent".into()),
        Err(error) => return Err(error),
    };
    parent.observation(Path::new(path.file_name().context("安装位置缺少名称")?))
}

fn directory_signature(path: &Path) -> Result<Option<String>> {
    match Directory::open_anchored(path) {
        Ok(directory) => Ok(Some(directory.identity()?)),
        Err(error) if crate::package_fs::is_not_found(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn installation_directory<'a>(w: &Workspace, path: &'a Path) -> Result<&'a str> {
    let name = path.file_name().and_then(|name| name.to_str()).context("缺少技能名称")?;
    crate::library::validate_name(name)?;
    let directory = path.parent().and_then(Path::file_name).and_then(|name| name.to_str())
        .context("缺少安装目录")?;
    if !path.is_absolute() || !matches!(directory, "skills" | "skills-inactive") {
        bail!("不是已注册工作区下的直接技能安装位置");
    }
    if path != w.path.join(directory).join(name) {
        // Allow the platform's /var and /tmp aliases without resolving the
        // entry itself. Arbitrary directory links and traversal still fail.
        Directory::open_anchored(path.parent().context("缺少安装目录")?)?
            .verify_location(&w.path.join(directory))?;
    }
    Ok(directory)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstallPlan {
    pub workspace: Workspace,
    pub name: String,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub action: String,
    pub source_hash: String,
    pub local_data: Option<PathBuf>,
    root: PathBuf,
    workspace_signature: String,
    expected: String,
    #[serde(default)]
    original: Option<PathBuf>,
    #[serde(default)]
    original_parent_signature: Option<String>,
    #[serde(default)]
    destination_parent_signature: Option<String>,
}

impl InstallPlan {
    /// Existing entry retained by migrate/reinstall; destination is always the
    /// publication location. Older plans use destination for both locations.
    pub fn original_path(&self) -> &Path {
        self.original.as_deref().unwrap_or(&self.destination)
    }
}

fn no_pending(w: &Workspace) -> Result<()> {
    let directory = w.path.join(RECOVERY);
    if !directory.exists() {
        return Ok(());
    }
    real_directory(&directory)?;
    for entry in fs::read_dir(directory)? {
        let path = entry?.path().join("transaction.json");
        if !path.exists() {
            continue;
        }
        let t: Transaction = read_json(&path)?;
        if !matches!(t.phase.as_str(), "committed" | "rolled_back") {
            bail!(
                "存在未完成 Proma 操作，请先执行 proma recover；恢复材料：{}",
                path.display()
            );
        }
    }
    Ok(())
}

pub fn plan_install(root: &Path, project: &Path, source: &Path, name: &str) -> Result<InstallPlan> {
    plan_install_at(root, project, source, name, "skills", false)
}

fn plan_install_at(
    root: &Path, project: &Path, source: &Path, name: &str,
    directory: &str, existing_only: bool,
) -> Result<InstallPlan> {
    crate::library::validate_name(name)?;
    if !matches!(directory, "skills" | "skills-inactive")
        || (directory == "skills-inactive" && !existing_only)
    {
        bail!("自动同步只允许更新原安装目录");
    }
    let workspace = workspace_for_path(root, project)?;
    no_pending(&workspace)?;
    let source = source.canonicalize().context("技能不存在")?;
    let source_hash = hash(&public_manifest(&source)?)?;
    let destination = workspace.path.join(directory).join(name);
    let other = if directory == "skills" { "skills-inactive" } else { "skills" };
    let other_path = workspace.path.join(other).join(name);
    let other_exists = match fs::symlink_metadata(&other_path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    let original = if other_exists {
        if existing_only || directory != "skills" || observed(&destination)? != "absent" {
            bail!("技能启停位置已变化或重复；未改变工作区选择");
        }
        Some(other_path)
    } else { None };
    let existing = original.as_deref().unwrap_or(&destination);
    let expected = observed(existing)?;
    let original_parent_signature = directory_signature(existing.parent().context("缺少原安装目录")?)?;
    let destination_parent_signature = directory_signature(destination.parent().context("缺少安装目录")?)?;
    let action = match fs::symlink_metadata(existing) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !existing_only => "create",
        Err(e) => return Err(e.into()),
        Ok(m) if m.file_type().is_symlink() => {
            if existing_only {
                bail!("已有安装变成外部链接，未迁移或覆盖");
            }
            if existing.canonicalize().ok().as_ref() != Some(&source) {
                bail!("同名链接指向其他来源，未覆盖");
            }
            "migrate"
        }
        Ok(_) => {
            let m = clean_marker(existing, &workspace).context("同名目录不能安全更新")?;
            if m.source != source {
                bail!("同名技能来源不同，未覆盖");
            }
            if original.is_some() {
                "reinstall"
            } else if m.source_hash == source_hash {
                "unchanged"
            } else {
                "update"
            }
        }
    }
    .to_string();
    let local_data_path = source.join("local-data");
    let mut local_data = match fs::symlink_metadata(&local_data_path) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => Some(local_data_path),
        Ok(_) => bail!("来源 local-data 必须是实体目录"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    // Existing copies retain their exact local-data link after a controlled
    // root move. Their marker and content hashes are not rewritten by a move.
    if let Ok(m) = marker(existing, &workspace)
        && m.source == source
        && let Some(previous) = m.local_data
        && storage_local_data(&previous, &source)
    {
        local_data = Some(previous);
    }
    #[cfg(test)]
    BEFORE_PLAN_OBSERVATION.with(|hook| {
        if let Some(callback) = hook.borrow_mut().take() { callback(); }
    });
    if observed(existing)? != expected {
        bail!("安装位置在规划期间变化，请重新预览");
    }
    if existing_only && expected == "absent" {
        bail!("已有安装已被移除，未重新创建");
    }
    Ok(InstallPlan {
        root: root.canonicalize()?,
        workspace_signature: signature(&workspace.path)?,
        expected,
        original,
        original_parent_signature,
        destination_parent_signature,
        workspace,
        name: name.into(),
        source,
        destination,
        action,
        source_hash,
        local_data,
    })
}

fn adaptation(content: &str, source: &Path, local_data: bool) -> String {
    if !local_data {
        return content.into();
    }
    let bridge = format!(
        "\n\n## 本机适配与已配置资源\n\n执行前读取 `{}/local-data/ADAPTATION.md`（若存在），并应用其中与任务相关的本机约定。本机适配中的相对路径以该文件所在目录为准。当前技能的本机数据和已安装环境继续位于 `{}/local-data/`；本目录的 `local-data/` 引用同一位置。复用原配置与运行入口，不因技能安装副本重新初始化环境或重新要求用户配置凭据。不得展示凭据正文。\n\n",
        source.display(),
        source.display()
    );
    let text = content.trim_start_matches('\u{feff}');
    if text.starts_with("---\n") || text.starts_with("---\r\n") {
        let mut offset = 0;
        for (i, line) in text.split_inclusive('\n').enumerate() {
            offset += line.len();
            if i > 0 && line.trim() == "---" {
                return format!("{}{}{}", &text[..offset], bridge, &text[offset..]);
            }
        }
    }
    format!("{bridge}{text}")
}

fn storage_local_data(path: &Path, source: &Path) -> bool {
    path.file_name() == Some(std::ffi::OsStr::new("local-data"))
        && path.parent().and_then(crate::library::storage::migrated_source).as_deref() == Some(source)
        && path.canonicalize().ok() == source.join("local-data").canonicalize().ok()
        && path.is_dir()
}

fn copy_package(plan: &InstallPlan, output: &Directory) -> Result<Marker> {
    let incoming = public_manifest(&plan.source)?;
    if hash(&incoming)? != plan.source_hash {
        bail!("技能源已变化，请重新预览");
    }
    let input = Directory::open_anchored(&plan.source)?;
    for (path, node) in &incoming {
        match node {
            Node::Directory { .. } => output.mkdir(Path::new(path))?,
            Node::File { mode, .. } => {
                std::io::copy(
                    &mut input.read(Path::new(path))?,
                    &mut output.create_file(Path::new(path), *mode)?,
                )?;
            }
            Node::Symlink { .. } => bail!("技能包链接未被授权复制"),
        }
    }
    for (path, node) in incoming.iter().rev() {
        if let Node::Directory { mode } = node {
            output.chmod_directory(Path::new(path), *mode)?;
        }
    }
    if hash(&public_manifest(&plan.source)?)? != plan.source_hash
        || output.installation_snapshot()? != incoming
    {
        bail!("复制期间技能源变化，未安装");
    }
    let mut content = String::new();
    output
        .read(Path::new("SKILL.md"))?
        .take(LIMIT + 1)
        .read_to_string(&mut content)?;
    if content.len() as u64 > LIMIT {
        bail!("SKILL.md 过大");
    }
    output.write(
        Path::new("SKILL.md"),
        adaptation(&content, &plan.source, plan.local_data.is_some()).as_bytes(),
    )?;
    if let Some(local_data) = &plan.local_data {
        output.symlink(
            Path::new("local-data"),
            local_data.to_str().context("本机数据路径不是 UTF-8")?,
            true,
        )?;
    }
    let m = Marker {
        schema: 1,
        workspace_id: plan.workspace.id.clone(),
        workspace_slug: plan.workspace.slug.clone(),
        name: plan.name.clone(),
        source: plan.source.clone(),
        source_hash: plan.source_hash.clone(),
        content_hash: {
            let mut m = output.installation_snapshot()?;
            m.remove(MARKER);
            hash(&m)?
        },
        local_data: plan.local_data.clone(),
    };
    output.write(Path::new(MARKER), &serde_json::to_vec_pretty(&m)?)?;
    Ok(m)
}

#[derive(Serialize, Deserialize)]
struct Transaction {
    workspace: Workspace,
    workspace_signature: String,
    target: PathBuf,
    #[serde(default)]
    original: Option<PathBuf>,
    backup: Option<PathBuf>,
    before: String,
    after: String,
    phase: String,
}

fn recheck(plan: &InstallPlan) -> Result<()> {
    crate::library::validate_name(&plan.name)?;
    let inactive = plan.destination == plan.workspace.path.join("skills-inactive").join(&plan.name);
    let original = plan.original_path();
    if (matches!(plan.action.as_str(), "migrate" | "reinstall" | "update" | "unchanged") && plan.expected == "absent")
        || (plan.action == "create" && plan.expected != "absent")
        || (plan.action == "reinstall" && plan.original.is_none())
        || plan.original.as_ref().is_some_and(|path| {
            *path != plan.workspace.path.join("skills-inactive").join(&plan.name)
                || inactive || !matches!(plan.action.as_str(), "migrate" | "reinstall")
        })
        || (plan.destination != plan.workspace.path.join("skills").join(&plan.name)
        && !(inactive && plan.expected != "absent"
            && matches!(plan.action.as_str(), "update" | "unchanged")))
        || plan.source.canonicalize()? != plan.source
        || !matches!(
            plan.action.as_str(),
            "create" | "migrate" | "reinstall" | "update" | "unchanged"
        )
        || plan
            .local_data
            .as_ref()
            .is_some_and(|p| *p != plan.source.join("local-data") && !storage_local_data(p, &plan.source))
    {
        bail!("Proma 安装计划路径或动作无效");
    }
    if let Some(path) = &plan.local_data {
        real_directory(path)?;
    }
    let other = if inactive { "skills" } else { "skills-inactive" };
    if plan.original.is_none() && fs::symlink_metadata(plan.workspace.path.join(other).join(&plan.name)).is_ok() {
        bail!("技能启停位置已变化，请重新预览");
    }
    if workspace_for_path(&plan.root, &plan.workspace.path)? != plan.workspace
        || signature(&plan.workspace.path)? != plan.workspace_signature
        || observed(original)? != plan.expected
        || (plan.original.is_some() && observed(&plan.destination)? != "absent")
        || (plan.original_parent_signature.is_some()
            && directory_signature(original.parent().context("缺少原安装目录")?)? != plan.original_parent_signature)
        || (plan.destination_parent_signature.is_some()
            && directory_signature(plan.destination.parent().context("缺少安装目录")?)? != plan.destination_parent_signature)
        || hash(&public_manifest(&plan.source)?)? != plan.source_hash
    {
        bail!("工作区、来源或安装位置已变化，请重新预览");
    }
    if plan.expected != "absent" {
        if fs::symlink_metadata(original)?
            .file_type()
            .is_symlink()
        {
            if plan.action != "migrate" || original.canonicalize()? != plan.source {
                bail!("安装链接不属于计划来源");
            }
        } else if plan.action == "migrate" || clean_marker(original, &plan.workspace)?.source != plan.source {
            bail!("安装目录不属于计划来源");
        }
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    static BEFORE_PUBLISH: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
    static BEFORE_STAGING: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
    static BEFORE_ROLLBACK_MOVE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
    static BEFORE_RECOVERY_MUTATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
    static BEFORE_FOLLOWER_REFRESH: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
    static BEFORE_PLAN_OBSERVATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
    static BEFORE_TOGGLE_MOVE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
    static AFTER_TOGGLE_MOVE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
    static BEFORE_REMOVE_MOVE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
    static BEFORE_INSPECTION_RECHECK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
}

fn new_operation(
    workspace: &Directory,
    project: &Path,
    prefix: &str,
) -> Result<(PathBuf, Directory)> {
    workspace.mkdir(Path::new(RECOVERY))?;
    let recovery = workspace.child(Path::new(RECOVERY))?;
    #[cfg(test)]
    BEFORE_STAGING.with(|hook| {
        if let Some(callback) = hook.borrow_mut().take() {
            callback();
        }
    });
    workspace.verify_location(project)?;
    recovery.verify_location(&project.join(RECOVERY))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let name = format!("{prefix}-{}-{stamp}", std::process::id());
    recovery.mkdir(Path::new(&name))?;
    let directory = recovery.child(Path::new(&name))?;
    let path = project.join(RECOVERY).join(name);
    directory.verify_location(&path)?;
    Ok((path, directory))
}

pub fn apply_plan(plan: &InstallPlan) -> Result<()> {
    let workspace = Directory::open_anchored(&plan.workspace.path)?;
    let _lock = workspace.lock(Path::new(".kitter-proma.lock"))?;
    recheck(plan)?;
    if workspace.identity()? != plan.workspace_signature {
        bail!("工作区目录身份已变化");
    }
    if plan.action == "unchanged" {
        return Ok(());
    }
    no_pending(&plan.workspace)?;
    let directory = plan.destination.parent().and_then(Path::file_name)
        .context("安装目录缺少名称")?;
    if directory == "skills" {
        workspace.mkdir(Path::new(directory))?;
    }
    let parent = workspace.child(Path::new(directory))?;
    let original_directory = installation_directory(&plan.workspace, plan.original_path())?;
    let input = workspace.child(Path::new(original_directory))?;
    let (operation, output) = new_operation(&workspace, &plan.workspace.path, "install")?;
    let mut t = Transaction {
        workspace: plan.workspace.clone(),
        workspace_signature: plan.workspace_signature.clone(),
        target: plan.destination.clone(),
        original: plan.original.clone(),
        backup: (plan.expected != "absent").then(|| operation.join("previous")),
        before: plan.expected.clone(),
        after: "unpublished".into(),
        phase: "preparing".into(),
    };
    output.write(
        Path::new("transaction.json"),
        &serde_json::to_vec_pretty(&t)?,
    )?;
    output.mkdir(Path::new("staging"))?;
    let staged = output.child(Path::new("staging"))?;
    if let Err(error) = copy_package(plan, &staged) {
        t.phase = "rolled_back".into();
        output.write(
            Path::new("transaction.json"),
            &serde_json::to_vec_pretty(&t)?,
        )?;
        return Err(error);
    }
    t.after = observed(&operation.join("staging"))?;
    t.phase = "prepared".into();
    output.write(
        Path::new("transaction.json"),
        &serde_json::to_vec_pretty(&t)?,
    )?;
    recheck(plan)?;
    workspace.verify_location(&plan.workspace.path)?;
    parent.verify_location(&plan.workspace.path.join(directory))?;
    input.verify_location(&plan.workspace.path.join(original_directory))?;
    output.verify_location(&operation)?;
    #[cfg(test)]
    BEFORE_PUBLISH.with(|hook| {
        if let Some(callback) = hook.borrow_mut().take() {
            callback();
        }
    });
    let result = (|| -> Result<()> {
        workspace.verify_location(&plan.workspace.path)?;
        parent.verify_location(&plan.workspace.path.join(directory))?;
        input.verify_location(&plan.workspace.path.join(original_directory))?;
        output.verify_location(&operation)?;
        if workspace_for_path(&plan.root, &plan.workspace.path)? != plan.workspace {
            bail!("Proma 工作区身份已变化");
        }
        if t.backup.is_some() {
            input.rename_to(Path::new(&plan.name), &output, Path::new("previous"))?;
            if !output
                .observation(Path::new("previous"))
                .is_ok_and(|value| value == t.before)
            {
                // Preserve the actual concurrent object at its original location.
                output.rename_to(Path::new("previous"), &input, Path::new(&plan.name))?;
                t.phase = "rolled_back".into();
                output.write(
                    Path::new("transaction.json"),
                    &serde_json::to_vec_pretty(&t)?,
                )?;
                bail!("被移走的原件已变化，已恢复原位置并停止安装");
            }
        }
        t.phase = "replacing".into();
        output.write(
            Path::new("transaction.json"),
            &serde_json::to_vec_pretty(&t)?,
        )?;
        output.rename_to(Path::new("staging"), &parent, Path::new(&plan.name))?;
        if observed(&plan.destination)? != t.after {
            bail!("发布对象身份或内容变化，保留现场");
        }
        clean_marker(&plan.destination, &plan.workspace)?;
        t.phase = "committed".into();
        output.write(
            Path::new("transaction.json"),
            &serde_json::to_vec_pretty(&t)?,
        )?;
        Ok(())
    })();
    if let Err(error) = result {
        if t.phase != "rolled_back" && restore_with_handles(&t, &output, &parent, &input).is_ok() {
            t.phase = "rolled_back".into();
            output.write(
                Path::new("transaction.json"),
                &serde_json::to_vec_pretty(&t)?,
            )?;
        }
        bail!(
            "Proma 安装未完成：{error}；恢复材料：{}",
            operation.display()
        );
    }
    Ok(())
}

fn restore_with_handles(t: &Transaction, output: &Directory, parent: &Directory, input: &Directory) -> Result<()> {
    let name = Path::new(t.target.file_name().context("目标缺少名称")?);
    let original = t.original.as_deref().unwrap_or(&t.target);
    let original_name = Path::new(original.file_name().context("原安装位置缺少名称")?);
    parent.verify_location(t.target.parent().context("目标缺少父目录")?)?;
    input.verify_location(original.parent().context("原安装位置缺少父目录")?)?;
    let current = match parent.observation(name) {
        Ok(value) => Some(value),
        // An unobservable active occupant is also foreign. Restoring a separate
        // inactive entry needs no access to that occupant.
        Err(_) if t.original.is_some() => None,
        Err(error) => return Err(error),
    };
    if t.original.is_some() {
        let previous = input.observation(original_name)?;
        if previous == t.before && current.as_deref() != Some(t.after.as_str()) {
            return Ok(());
        }
        if previous != "absent" {
            bail!("原安装位置已有其他对象，保留现场");
        }
    } else if current.as_deref() == Some(t.before.as_str()) {
        return Ok(());
    }
    if t.backup.is_some() && output.observation(Path::new("previous"))? != t.before {
        bail!("恢复原件已变化，保留现场");
    }
    if t.original.is_none() && current.as_deref() != Some("absent") && current.as_deref() != Some(t.after.as_str()) {
        bail!("目标不属于本事务，保留现场");
    }
    #[cfg(test)]
    BEFORE_ROLLBACK_MOVE.with(|hook| {
        if let Some(callback) = hook.borrow_mut().take() {
            callback();
        }
    });
    parent.verify_location(t.target.parent().context("目标缺少父目录")?)?;
    input.verify_location(original.parent().context("原安装位置缺少父目录")?)?;
    // A concurrent active occupant never prevents restoring an inactive entry,
    // and is never moved by this transaction.
    let move_current = current.as_deref().is_some_and(|value| value != "absent" && value == t.after);
    if move_current {
        parent.rename_to(name, output, Path::new("failed"))?;
        if !output
            .observation(Path::new("failed"))
            .is_ok_and(|value| value == t.after)
        {
            output.rename_to(Path::new("failed"), parent, name)?;
            bail!("回滚时目标已替换，已恢复原位置");
        }
    }
    if t.backup.is_some() {
        if let Err(error) = output.rename_to(Path::new("previous"), input, original_name) {
            if move_current {
                output.rename_to(Path::new("failed"), parent, name)?;
            }
            return Err(error);
        }
        if !input
            .observation(original_name)
            .is_ok_and(|value| value == t.before)
        {
            input.rename_to(original_name, output, Path::new("previous"))?;
            if move_current {
                output.rename_to(Path::new("failed"), parent, name)?;
            }
            bail!("恢复时原件已替换，保留恢复前状态");
        }
    }
    Ok(())
}

#[cfg(test)]
fn restore_transaction(t: &Transaction, operation: &Path) -> Result<()> {
    restore_with_handles(
        t,
        &Directory::open_anchored(operation)?,
        &Directory::open_anchored(t.target.parent().context("目标缺少父目录")?)?,
        &Directory::open_anchored(t.original.as_deref().unwrap_or(&t.target).parent().context("原安装位置缺少父目录")?)?,
    )
}

pub fn install(project: &Path, source: &Path, name: &str) -> Result<()> {
    let (root, _) = configured_workspace(project)?;
    apply_plan(&plan_install(&root, project, source, name)?)
}

/// Refresh existing active followers after the maintained source has committed.
/// No new installation is created and inactive workspace choices are retained.
pub(crate) fn sync_existing_source(source: &Path) -> Vec<String> {
    home()
        .map(|root| sync_existing_source_in(&root, source))
        .unwrap_or_default()
}

fn sync_existing_source_in(root: &Path, source: &Path) -> Vec<String> {
    let result = (|| -> Result<Vec<String>> {
        let source = source.canonicalize()?;
        let mut warnings = Vec::new();
        for workspace in workspaces(root)? {
            for installation in inspect_in(root, &workspace.path)? {
                if installation.state != "active"
                    || !installation.managed
                    || installation.source.as_ref() != Some(&source)
                {
                    continue;
                }
                #[cfg(test)]
                BEFORE_FOLLOWER_REFRESH.with(|hook| {
                    if let Some(callback) = hook.borrow_mut().take() {
                        callback();
                    }
                });
                if let Err(error) = plan_install(root, &workspace.path, &source, &installation.name)
                    .and_then(|plan| {
                        if !matches!(plan.action.as_str(), "update" | "unchanged") {
                            bail!("已有安装已移除或改变，未重新安装或迁移");
                        }
                        apply_plan(&plan)
                    })
                {
                    warnings.push(format!(
                        "Proma {} / {} 待同步：{error}",
                        workspace.name, installation.name
                    ));
                }
            }
        }
        Ok(warnings)
    })();
    result.unwrap_or_else(|error| vec![format!("Proma 已有安装待核验：{error}")])
}

#[derive(Default)]
pub(crate) struct LocalMirrorSync {
    observed: BTreeMap<PathBuf, String>,
}

#[derive(Default, Debug)]
pub struct LocalSyncReport {
    pub updated: usize,
    pub inactive_updated: usize,
    pub catalog_changed: bool,
    pub warnings: Vec<String>,
}

impl LocalMirrorSync {
    /// Two identical observations debounce edits. Only existing marked copies
    /// of registered local sources are eligible, in their current directory.
    pub(crate) fn poll(&mut self, root: &Path, sources: &BTreeSet<PathBuf>) -> Result<LocalSyncReport> {
        let sources = sources.iter().filter_map(|path| path.canonicalize().ok())
            .collect::<BTreeSet<_>>();
        self.observed.retain(|source, _| sources.contains(source));
        let mut followers: BTreeMap<PathBuf, Vec<(Workspace, String, Marker)>> = BTreeMap::new();
        let mut report = LocalSyncReport::default();
        if sources.is_empty() {
            return Ok(report);
        }
        for workspace in workspaces(root)? {
            for directory in ["skills", "skills-inactive"] {
                let parent = workspace.path.join(directory);
                if fs::symlink_metadata(&parent).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
                    continue;
                }
                real_directory(&parent)?;
                for entry in fs::read_dir(parent)? {
                    let path = entry?.path();
                    // Unmanaged packages, built-ins and external links are never adopted.
                    if let Ok(m) = marker(&path, &workspace)
                        && sources.contains(&m.source)
                    {
                        followers.entry(m.source.clone()).or_default()
                            .push((workspace.clone(), directory.into(), m));
                    }
                }
            }
        }
        for (source, copies) in followers {
            let source_hash = match public_manifest(&source).and_then(|m| hash(&m)) {
                Ok(value) => value,
                Err(error) => {
                    self.observed.remove(&source);
                    report.warnings.push(format!("{}：{error}", source.display()));
                    continue;
                }
            };
            let stable = self.observed.insert(source.clone(), source_hash.clone())
                .as_ref() == Some(&source_hash);
            if !stable {
                continue;
            }
            for (workspace, directory, m) in copies {
                if m.source_hash == source_hash {
                    continue;
                }
                #[cfg(test)]
                BEFORE_FOLLOWER_REFRESH.with(|hook| {
                    if let Some(callback) = hook.borrow_mut().take() { callback(); }
                });
                let result = plan_install_at(root, &workspace.path, &source, &m.name, &directory, true)
                    .and_then(|plan| {
                        if plan.source_hash != source_hash {
                            bail!("技能源仍在修改，等待下一次稳定检查");
                        }
                        apply_plan(&plan)
                    });
                match result {
                    Ok(()) => {
                        report.updated += 1;
                        report.inactive_updated += usize::from(directory == "skills-inactive");
                    }
                    Err(error) => report.warnings.push(format!("{} / {}：{error}", workspace.name, m.name)),
                }
            }
        }
        report.warnings.sort();
        report.warnings.dedup();
        Ok(report)
    }
}

pub fn is_managed(path: &Path, source: &Path) -> bool {
    let Ok((_, w)) = path_workspace(path) else {
        return false;
    };
    marker(path, &w).is_ok_and(|m| source.canonicalize().ok().as_ref() == Some(&m.source))
}

#[derive(Clone, Debug, Serialize)]
pub struct Installation {
    pub name: String,
    pub path: PathBuf,
    pub state: String,
    pub status: String,
    pub source: Option<PathBuf>,
    pub managed: bool,
    pub removable: bool,
    /// Display origin, including configured local variants of app-listed presets.
    /// State-change permission is separately verified by `can_toggle`.
    pub bundled: bool,
    pub integration: Option<String>,
    toggleable: bool,
    #[serde(skip)]
    snapshot: Option<InstallationSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InstallationSnapshot {
    root: PathBuf,
    workspace: Workspace,
    workspace_signature: String,
    parent_signature: String,
    path: PathBuf,
    observation: String,
    raw_target: Option<PathBuf>,
    link_source: Option<PathBuf>,
    binding: String,
}

fn capture_installation(root: &Path, w: &Workspace, path: &Path) -> Result<InstallationSnapshot> {
    let directory = installation_directory(w, path)?;
    let workspace = Directory::open_anchored(&w.path)?;
    let parent = workspace.child(Path::new(directory))?;
    let name = Path::new(path.file_name().context("缺少技能名称")?);
    let observation = parent.observation(name)?;
    let raw_target = if fs::symlink_metadata(path)?.file_type().is_symlink() {
        Some(fs::read_link(path)?)
    } else { None };
    let link_source = if raw_target.is_some() && path.is_dir() {
        Some(path.canonicalize()?)
    } else { None };
    if parent.observation(name)? != observation || workspace_for_path(root, &w.path)? != *w {
        bail!("Proma 工作区或技能在观察期间变化，请刷新后重试");
    }
    workspace.verify_location(&w.path)?;
    parent.verify_location(&w.path.join(directory))?;
    Ok(InstallationSnapshot {
        root: root.canonicalize()?, workspace: w.clone(),
        workspace_signature: workspace.identity()?, parent_signature: parent.identity()?,
        path: path.to_path_buf(), observation, raw_target, link_source, binding: String::new(),
    })
}

/// Check the exact object captured by inspect, without adopting a newer copy.
/// Serialized display records intentionally have no usable execution snapshot.
pub fn validate_previewed_installation(root: &Path, workspace: &Workspace, expected: &Installation) -> Result<()> {
    let snapshot = expected.snapshot.as_ref().context("预览缺少冻结快照，请刷新后重新选择")?;
    if snapshot.workspace != *workspace || snapshot.path != expected.path
        || hash(expected)? != snapshot.binding
    {
        bail!("Proma 安装记录与预览快照不一致，请刷新后重新选择");
    }
    let mut current = capture_installation(root, workspace, &expected.path)?;
    current.binding = snapshot.binding.clone();
    if current != *snapshot {
        bail!("Proma 工作区、安装目录或技能与预览不一致，已保留现场");
    }
    Ok(())
}

impl Installation {
    /// Snapshot capability only; set_enabled_path rechecks the live package.
    pub fn can_toggle(&self) -> bool {
        self.toggleable && self.bundled && !self.managed && self.integration.is_none() && self.snapshot.is_some()
    }
}

fn skill_entry_bytes(path: &Path) -> Option<Vec<u8>> {
    let file = Directory::open_anchored(path.parent()?).ok()?
        .read(Path::new(path.file_name()?)).ok()?;
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= LIMIT).then_some(bytes)
}

fn skill_entry_fingerprint(path: &Path) -> Option<[u8; 32]> {
    skill_entry_bytes(path).map(|bytes| Sha256::digest(&bytes).into())
}

fn integration_entry_templates(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let defaults = root.join("default-skills");
    let mut entries = BTreeMap::new();
    if real_directory(&defaults).is_err() { return entries; }
    for name in ["distill-memory", "read-working-memory", "save-thread", "search-memory", "status"] {
        let template = defaults.join(name);
        if real_directory(&template).is_err() { continue; }
        let Some(bytes) = skill_entry_bytes(&template.join("SKILL.md")) else { continue; };
        // Recognize the configured Proma integration templates, not arbitrary
        // same-name project skills. The workspace entry must also match below.
        if String::from_utf8_lossy(&bytes).contains("Nowledge Mem") {
            entries.insert(name.into(), bytes);
        }
    }
    entries
}

// Display origin only: local commands and added notes may differ, but the
// configured integration's name, purpose and existing prose must remain.
fn integration_identity(bytes: &[u8], name: &str) -> Option<(String, Vec<String>)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" { return None; }
    let header = lines.by_ref().take_while(|line| line.trim() != "---").collect::<Vec<_>>().join("\n");
    let metadata: serde_yaml::Value = serde_yaml::from_str(&header).ok()?;
    if metadata.get("name")?.as_str()? != name { return None; }
    let description = metadata.get("description")?.as_str()?.trim();
    if description.is_empty() { return None; }
    let mut fence = None;
    let prose = lines.filter_map(|line| {
        let line = line.trim();
        let delimiter = *line.as_bytes().first()?;
        let width = line.bytes().take_while(|byte| *byte == delimiter).count();
        if let Some((opening, minimum)) = fence {
            if delimiter == opening && width >= minimum && line[width..].trim().is_empty() {
                fence = None;
            }
            None
        } else if matches!(delimiter, b'`' | b'~') && width >= 3
            && !(delimiter == b'`' && line[width..].contains('`'))
        {
            fence = Some((delimiter, width));
            None
        } else { Some(line.to_string()) }
    }).collect::<Vec<_>>();
    prose.iter().any(|line| line.contains("Nowledge Mem"))
        .then(|| (description.to_string(), prose))
}

fn matches_integration_variant(template: &[u8], entry: &[u8], name: &str) -> bool {
    let Some((expected, prose)) = integration_identity(template, name) else { return false; };
    let Some((actual, local_prose)) = integration_identity(entry, name) else { return false; };
    let mut local = local_prose.iter();
    expected == actual && prose.iter().all(|line| local.by_ref().any(|candidate| candidate == line))
}

fn bundled_entry_fingerprints(root: &Path) -> BTreeMap<String, BTreeSet<[u8; 32]>> {
    #[cfg(test)]
    if let Some(templates) = root.canonicalize().ok().and_then(|root| {
        TEST_BUNDLED_TEMPLATES.with(|entries| entries.borrow().get(&root).cloned())
    }) {
        return bundled_entry_fingerprints_in(&root.join("default-skills"), &templates);
    }
    #[cfg(target_os = "macos")]
    return bundled_entry_fingerprints_in(&root.join("default-skills"), Path::new("/Applications/Proma.app/Contents/Resources/default-skills"));
    #[cfg(not(target_os = "macos"))]
    { let _ = root; BTreeMap::new() }
}

#[cfg(test)]
thread_local! {
    static TEST_BUNDLED_TEMPLATES: std::cell::RefCell<BTreeMap<PathBuf, PathBuf>> = const {
        std::cell::RefCell::new(BTreeMap::new())
    };
}

/// Isolated fixture override; never compiled into a production build.
#[cfg(test)]
pub(crate) fn register_test_bundled_templates(root: &Path, app_templates: &Path) {
    let root = root.canonicalize().expect("test Proma root must exist");
    let templates = app_templates.canonicalize().expect("test bundled templates must exist");
    TEST_BUNDLED_TEMPLATES.with(|entries| entries.borrow_mut().insert(root, templates));
}

fn bundled_entry_fingerprints_in(
    _defaults: &Path, app_templates: &Path,
) -> BTreeMap<String, BTreeSet<[u8; 32]>> {
    let mut entries = BTreeMap::<String, BTreeSet<[u8; 32]>>::new();
    if real_directory(app_templates).is_err() { return entries; }
    // Only current application bytes establish bundled trust. A same-name
    // user cache is not a verified record of a prior application version.
    for entry in fs::read_dir(app_templates).into_iter().flatten().flatten() {
        let path = entry.path();
        if real_directory(&path).is_err() { continue; }
        let Some(fingerprint) = skill_entry_fingerprint(&path.join("SKILL.md")) else { continue; };
        let name = entry.file_name().to_string_lossy().into_owned();
        entries.entry(name).or_default().insert(fingerprint);
    }
    entries
}

pub fn inspect(project: &Path) -> Result<Vec<Installation>> {
    let (root, _) = configured_workspace(project)?;
    inspect_in(&root, project)
}

pub fn inspect_in(root: &Path, project: &Path) -> Result<Vec<Installation>> {
    inspect_with_bundles(root, project, &bundled_entry_fingerprints(root))
}

fn inspect_with_bundles(
    root: &Path, project: &Path, bundled_entries: &BTreeMap<String, BTreeSet<[u8; 32]>>,
) -> Result<Vec<Installation>> {
    let w = workspace_for_path(root, project)?;
    let workspace = Directory::open_anchored(&w.path)?;
    let workspace_signature = workspace.identity()?;
    let recovery_clear = no_pending(&w).is_ok();
    let integration_entries = integration_entry_templates(root);
    let defaults = root.join("default-skills");
    let local_presets = if real_directory(&defaults).is_ok() {
        bundled_entries.keys().filter_map(|name| {
            let template = defaults.join(name);
            real_directory(&template).ok()?;
            Some((name.clone(), skill_entry_fingerprint(&template.join("SKILL.md"))?))
        }).collect::<BTreeMap<_, _>>()
    } else { BTreeMap::new() };
    let mut out = vec![];
    for (directory, state) in [("skills", "active"), ("skills-inactive", "inactive")] {
        let parent = w.path.join(directory);
        if !parent.exists() {
            continue;
        }
        real_directory(&parent)?;
        let input = workspace.child(Path::new(directory))?;
        let parent_signature = input.identity()?;
        for entry in fs::read_dir(&parent)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let before = capture_installation(root, &w, &path).ok();
            if before.as_ref().is_some_and(|snapshot| {
                snapshot.workspace_signature != workspace_signature
                    || snapshot.parent_signature != parent_signature
            }) {
                bail!("Proma 工作区或安装目录在扫描期间变化，请刷新后重试");
            }
            let linked = fs::symlink_metadata(&path)?.file_type().is_symlink();
            let found = marker(&path, &w).ok();
            if !path.join("SKILL.md").is_file() && !linked && found.is_none() {
                continue;
            }
            let status = if linked {
                if path.try_exists().unwrap_or(false) { "external_link" } else { "broken_link" }
            } else if let Some(m) = &found {
                if clean_marker(&path, &w).is_err() {
                    "modified"
                } else if public_manifest(&m.source)
                    .and_then(|v| hash(&v))
                    .is_ok_and(|h| h == m.source_hash)
                {
                    "current"
                } else {
                    "update_pending"
                }
            } else {
                "unmanaged"
            };
            // An invalid managed marker must not fall back to preset rights.
            let unmarked = fs::symlink_metadata(path.join(MARKER))
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound);
            let preset = unmarked && real_directory(&path).is_ok();
            let entry = preset.then(|| skill_entry_bytes(&path.join("SKILL.md"))).flatten();
            let fingerprint = entry.as_ref().map(|bytes| <[u8; 32]>::from(Sha256::digest(bytes)));
            let integration_status = entry.as_deref().and_then(|entry| {
                let template = integration_entries.get(&name)?;
                if entry == template { Some(status) }
                else { matches_integration_variant(template, entry, &name).then_some("integration_variant") }
            });
            let integration = integration_status.map(|_| "Nowledge Mem".to_string());
            let verified_bundle = preset && integration.is_none()
                && bundled_entries.get(&name).is_some_and(|fingerprints| {
                    fingerprint.is_some_and(|fingerprint| fingerprints.contains(&fingerprint))
                });
            // A matching configured template explains the display origin, but
            // only current application bytes authorize state changes.
            let bundled = verified_bundle || (preset && integration.is_none()
                && local_presets.get(&name).is_some_and(|expected| fingerprint.as_ref() == Some(expected)));
            let status = integration_status.unwrap_or_else(|| if bundled && !verified_bundle { "preset_variant" } else { status });
            let removable_link = linked && cfg!(unix) && crate::library::validate_name(&name).is_ok()
                && observed(&path).is_ok();
            let source = found.as_ref().map(|m| m.source.clone()).or_else(|| {
                before.as_ref().and_then(|snapshot| snapshot.link_source.clone())
            });
            #[cfg(test)]
            BEFORE_INSPECTION_RECHECK.with(|hook| {
                if let Some(callback) = hook.borrow_mut().take() { callback(); }
            });
            if capture_installation(root, &w, &path).ok() != before {
                bail!("Proma 技能在扫描期间变化，请刷新后重试");
            }
            let stable = before.is_some();
            let mut installation = Installation {
                name,
                path,
                state: state.into(),
                status: status.into(),
                source,
                managed: found.is_some(),
                removable: stable && recovery_clear && (matches!(status, "current" | "update_pending") || removable_link),
                bundled,
                integration,
                toggleable: stable && cfg!(unix) && recovery_clear && verified_bundle,
                snapshot: None,
            };
            if let Some(mut snapshot) = before {
                snapshot.binding = hash(&installation)?;
                installation.snapshot = Some(snapshot);
            }
            out.push(installation);
        }
        input.verify_location(&parent)?;
    }
    workspace.verify_location(&w.path)?;
    if workspace_for_path(root, &w.path)? != w {
        bail!("Proma 工作区身份在扫描期间变化，请刷新后重试");
    }
    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.state.cmp(&b.state)));
    Ok(out)
}

pub fn set_enabled(project: &Path, name: &str, enabled: bool) -> Result<()> {
    crate::library::validate_name(name)?;
    let (_, w) = configured_workspace(project)?;
    let from = if enabled { "skills-inactive" } else { "skills" };
    set_enabled_path(&w.path.join(from).join(name), enabled)
}

/// Move one installation in its registered native workspace. A Finder project
/// association is never used to select or fan out this operation.
pub fn set_enabled_path(path: &Path, enabled: bool) -> Result<()> {
    let root = home().context("无法定位 Proma 配置根")?;
    set_enabled_path_in(&root, path, enabled)
}

pub fn set_enabled_path_in(root: &Path, path: &Path, enabled: bool) -> Result<()> {
    set_enabled_path_with_expectation(root, path, enabled, None, None)
}

/// Bind a batch operation to the workspace and source shown in its preview.
pub fn set_enabled_previewed_in(root: &Path, workspace: &Workspace, copy: &Installation, source_file: Option<&Path>, enabled: bool) -> Result<()> {
    set_enabled_path_with_expectation(root, &copy.path, enabled,
        None, Some((workspace, copy, source_file)))
}

#[cfg(test)]
thread_local! {
    pub(crate) static BEFORE_TOGGLE_LOCK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn set_enabled_path_with_bundles(
    root: &Path, path: &Path, enabled: bool,
    bundled_entries: &BTreeMap<String, BTreeSet<[u8; 32]>>,
) -> Result<()> {
    set_enabled_path_with_expectation(root, path, enabled, Some(bundled_entries), None)
}

fn set_enabled_path_with_expectation(
    root: &Path, path: &Path, enabled: bool,
    test_bundled_entries: Option<&BTreeMap<String, BTreeSet<[u8; 32]>>>,
    preview: Option<(&Workspace, &Installation, Option<&Path>)>,
) -> Result<()> {
    if !path.is_absolute() {
        bail!("Proma 安装位置必须是绝对路径");
    }
    let name = path.file_name().and_then(|p| p.to_str()).context("缺少技能名称")?;
    crate::library::validate_name(name)?;
    let (from, to) = if enabled {
        ("skills-inactive", "skills")
    } else {
        ("skills", "skills-inactive")
    };
    let parent = path.parent().context("安装位置缺少父目录")?;
    if parent.file_name().and_then(|p| p.to_str()) != Some(from) {
        bail!("启停源目录应为 {from}，请刷新安装状态");
    }
    #[cfg(test)]
    BEFORE_TOGGLE_LOCK.with(|hook| {
        if let Some(callback) = hook.borrow_mut().take() { callback(); }
    });
    let w = workspace_for_path(root, parent.parent().context("安装位置缺少工作区")?)?;
    let workspace = Directory::open_anchored(&w.path)?;
    let _lock = workspace.lock(Path::new(".kitter-proma.lock"))?;
    if let Some((expected_workspace, expected_copy, _)) = preview {
        validate_previewed_installation(root, expected_workspace, expected_copy)?;
        if workspace.identity()? != expected_copy.snapshot.as_ref().context("预览缺少冻结快照")?.workspace_signature {
            bail!("持锁工作区与预览不一致，请刷新后重试");
        }
        let current = workspace_for_path(root, &w.path)?;
        if current.id != expected_workspace.id || current.slug != expected_workspace.slug
            || current.path != expected_workspace.path
        { bail!("工作区身份与预览不一致，请刷新后重新选择"); }
    }
    no_pending(&w)?;
    let source = w.path.join(from).join(name);
    // Open the supplied path as well, rejecting linked or traversing aliases.
    let input = Directory::open_anchored(parent)?;
    input.verify_location(source.parent().context("缺少源目录")?)?;
    real_directory(&source)?;
    let expected = if let Some((_, copy, _)) = preview {
        let snapshot = copy.snapshot.as_ref().context("预览缺少冻结快照")?;
        if input.identity()? != snapshot.parent_signature || !copy.can_toggle() {
            bail!("预览安装目录或启停权限已变化");
        }
        snapshot.observation.clone()
    } else { input.observation(Path::new(name))? };
    // Production callers resolve current templates only while holding the
    // workspace lock. The explicit map is confined to the test-only helper.
    let bundled_entries = test_bundled_entries.cloned()
        .unwrap_or_else(|| bundled_entry_fingerprints(root));
    let installation = inspect_with_bundles(root, &w.path, &bundled_entries)?
        .into_iter().find(|item| item.path == source)
        .context("未找到可启停的 Proma 安装")?;
    if let Some((_, expected_copy, source_file)) = preview {
        if installation.name != expected_copy.name || installation.path != expected_copy.path
            || installation.state != expected_copy.state || installation.source != expected_copy.source
            || installation.managed != expected_copy.managed || installation.bundled != expected_copy.bundled
            || installation.integration != expected_copy.integration
        { bail!("副本来源或安装状态与预览不一致，请刷新后重新选择"); }
        if let Some(expected_source) = source_file {
            let source = installation.source.as_ref().context("预览的受管副本缺少来源")?.join("SKILL.md");
            if source.canonicalize().context("无法核验技能源实际位置，请刷新后检查")? != expected_source {
                bail!("技能源实际位置与预览不一致，请刷新后重新选择");
            }
        }
    }
    if !installation.can_toggle() {
        bail!("只允许启停经核验的内置预设；用户技能请安装或卸载");
    }
    workspace.mkdir(Path::new(to))?;
    let output = workspace.child(Path::new(to))?;
    if workspace_for_path(root, &w.path)? != w
        || input.observation(Path::new(name))? != expected
    {
        bail!("Proma 工作区或安装内容已变化");
    }
    no_pending(&w)?;
    workspace.verify_location(&w.path)?;
    input.verify_location(&w.path.join(from))?;
    output.verify_location(&w.path.join(to))?;
    #[cfg(test)]
    BEFORE_TOGGLE_MOVE.with(|hook| {
        if let Some(callback) = hook.borrow_mut().take() {
            callback();
        }
    });
    if let Some((expected_workspace, expected_copy, _)) = preview {
        validate_previewed_installation(root, expected_workspace, expected_copy)?;
    }
    input.rename_to(Path::new(name), &output, Path::new(name))?;
    #[cfg(test)]
    AFTER_TOGGLE_MOVE.with(|hook| {
        if let Some(callback) = hook.borrow_mut().take() {
            callback();
        }
    });
    if !output
        .observation(Path::new(name))
        .is_ok_and(|value| value == expected)
        || workspace.verify_location(&w.path).is_err()
        || input.verify_location(&w.path.join(from)).is_err()
        || output.verify_location(&w.path.join(to)).is_err()
    {
        output.rename_to(Path::new(name), &input, Path::new(name))
            .context("启停校验失败，无法恢复原位置；保留两个目录中的现场")?;
        bail!("技能在启停期间变化，已恢复原位置");
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct LinkRemovalPlan {
    pub workspace: Workspace,
    pub path: PathBuf,
    pub raw_target: PathBuf,
    root: PathBuf,
    workspace_signature: String,
    parent_signature: String,
    expected: String,
    binding: String,
}

/// Plan removal without resolving the link, including relative/dangling links.
pub fn plan_remove_link(root: &Path, project: &Path, path: &Path) -> Result<LinkRemovalPlan> {
    let workspace = workspace_for_path(root, project)?;
    let directory = installation_directory(&workspace, path)?;
    let normalized = workspace.path.join(directory).join(path.file_name().context("缺少技能名称")?);
    let path = normalized.as_path();
    no_pending(&workspace)?;
    let native = Directory::open_anchored(&workspace.path)?;
    let parent = native.child(Path::new(directory))?;
    if !fs::symlink_metadata(path)?.file_type().is_symlink() {
        bail!("移除链接计划只接受直接软链接");
    }
    let raw_target = fs::read_link(path)?;
    let expected = parent.observation(Path::new(path.file_name().context("缺少技能名称")?))?;
    native.verify_location(&workspace.path)?;
    parent.verify_location(path.parent().context("缺少安装目录")?)?;
    if fs::read_link(path)? != raw_target || observed(path)? != expected {
        bail!("链接在规划期间变化，请重新预览");
    }
    Ok(LinkRemovalPlan {
        binding: hash(&(&workspace, path, &raw_target))?,
        workspace_signature: native.identity()?,
        parent_signature: parent.identity()?,
        root: root.canonicalize()?,
        workspace,
        path: path.to_path_buf(),
        raw_target,
        expected,
    })
}

pub fn apply_remove_link(plan: &LinkRemovalPlan) -> Result<()> {
    if hash(&(&plan.workspace, &plan.path, &plan.raw_target))? != plan.binding {
        bail!("链接移除计划与预览不一致");
    }
    let directory = installation_directory(&plan.workspace, &plan.path)?;
    let workspace = Directory::open_anchored(&plan.workspace.path)?;
    let _lock = workspace.lock(Path::new(".kitter-proma.lock"))?;
    let input = workspace.child(Path::new(directory))?;
    if workspace_for_path(&plan.root, &plan.workspace.path)? != plan.workspace
        || workspace.identity()? != plan.workspace_signature
        || input.identity()? != plan.parent_signature
        || !fs::symlink_metadata(&plan.path)?.file_type().is_symlink()
        || fs::read_link(&plan.path)? != plan.raw_target
        || input.observation(Path::new(plan.path.file_name().context("缺少技能名称")?))? != plan.expected
    {
        bail!("工作区、安装目录或链接与预览不一致");
    }
    no_pending(&plan.workspace)?;
    remove_entry_locked(&plan.root, &plan.workspace, &plan.path, &workspace, &input, &plan.expected)
}

pub fn uninstall_path(path: &Path) -> Result<()> {
    let (root, w) = path_workspace(path)?;
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return apply_remove_link(&plan_remove_link(&root, &w.path, path)?);
    }
    uninstall_at(&root, &w, path, None)
}

/// Remove exactly the managed copy shown by cross-project preview, under the
/// same workspace lock as the existing recoverable removal transaction.
pub fn uninstall_previewed_in(root: &Path, workspace: &Workspace, copy: &Installation, source_file: &Path) -> Result<()> {
    uninstall_at(root, workspace, &copy.path, Some((copy, source_file)))
}

fn uninstall_at(root: &Path, w: &Workspace, path: &Path, preview: Option<(&Installation, &Path)>) -> Result<()> {
    let directory = installation_directory(w, path)?;
    let normalized = w.path.join(directory).join(path.file_name().context("缺少技能名称")?);
    let path = normalized.as_path();
    let workspace = Directory::open_anchored(&w.path)?;
    let _lock = workspace.lock(Path::new(".kitter-proma.lock"))?;
    let current_workspace = workspace_for_path(root, &w.path)?;
    if current_workspace.id != w.id || current_workspace.slug != w.slug || current_workspace.path != w.path {
        bail!("工作区身份与预览不一致，请刷新后检查");
    }
    if let Some((expected_copy, source_file)) = preview {
        validate_previewed_installation(root, w, expected_copy)?;
        if workspace.identity()? != expected_copy.snapshot.as_ref().context("预览缺少冻结快照")?.workspace_signature
            || !expected_copy.managed || !expected_copy.removable
        {
            bail!("持锁工作区或移除权限与预览不一致，已保留");
        }
        let copy = inspect_in(root, &w.path)?.into_iter().find(|c| c.path == path)
            .context("预览安装已不存在，请刷新后检查")?;
        if copy.name != expected_copy.name || copy.state != expected_copy.state || copy.source != expected_copy.source
            || !copy.managed || !copy.removable
        { bail!("副本来源或状态与预览不一致，已保留"); }
        if copy.source.as_ref().context("受管副本缺少来源")?.join("SKILL.md").canonicalize()
            .context("无法核验技能源实际位置")? != source_file
        { bail!("技能源实际位置与预览不一致，已保留"); }
    }
    no_pending(&w)?;
    clean_marker(path, &w)?;
    let input = workspace.child(Path::new(directory))?;
    let expected = if let Some((copy, _)) = preview {
        validate_previewed_installation(root, w, copy)?;
        let snapshot = copy.snapshot.as_ref().context("预览缺少冻结快照")?;
        if input.identity()? != snapshot.parent_signature {
            bail!("安装父目录与预览不一致，已保留");
        }
        snapshot.observation.clone()
    } else { observed(path)? };
    remove_entry_locked(root, w, path, &workspace, &input, &expected)
}

fn remove_entry_locked(root: &Path, w: &Workspace, path: &Path, workspace: &Directory, input: &Directory, expected: &str) -> Result<()> {
    let (operation, output) = new_operation(&workspace, &w.path, "uninstall")?;
    let mut t = Transaction {
        workspace: w.clone(),
        workspace_signature: workspace.identity()?,
        target: path.to_path_buf(),
        original: None,
        backup: Some(operation.join("previous")),
        before: expected.into(),
        after: "absent".into(),
        phase: "prepared".into(),
    };
    output.write(
        Path::new("transaction.json"),
        &serde_json::to_vec_pretty(&t)?,
    )?;
    if workspace_for_path(root, &w.path)? != *w || observed(path)? != expected {
        bail!("Proma 工作区或安装内容已变化");
    }
    workspace.verify_location(&w.path)?;
    input.verify_location(path.parent().context("缺少安装目录")?)?;
    output.verify_location(&operation)?;
    #[cfg(test)]
    BEFORE_REMOVE_MOVE.with(|hook| {
        if let Some(callback) = hook.borrow_mut().take() { callback(); }
    });
    if workspace_for_path(root, &w.path)? != *w {
        bail!("Proma 工作区身份已变化");
    }
    workspace.verify_location(&w.path)?;
    input.verify_location(path.parent().context("缺少安装目录")?)?;
    output.verify_location(&operation)?;
    input.rename_to(
        Path::new(path.file_name().context("缺少技能名称")?),
        &output,
        Path::new("previous"),
    )?;
    if !output
        .observation(Path::new("previous"))
        .is_ok_and(|value| value == expected)
    {
        output.rename_to(
            Path::new("previous"),
            &input,
            Path::new(path.file_name().context("缺少技能名称")?),
        )?;
        t.phase = "rolled_back".into();
        output.write(
            Path::new("transaction.json"),
            &serde_json::to_vec_pretty(&t)?,
        )?;
        bail!("移除对象已变化，已恢复原位置");
    }
    t.phase = "committed".into();
    output.write(
        Path::new("transaction.json"),
        &serde_json::to_vec_pretty(&t)?,
    )?;
    Ok(())
}

pub fn recover(project: &Path) -> Result<()> {
    let (_, w) = configured_workspace(project)?;
    recover_in(&w)
}

fn recover_in(w: &Workspace) -> Result<()> {
    let workspace = Directory::open_anchored(&w.path)?;
    let _lock = workspace.lock(Path::new(".kitter-proma.lock"))?;
    let recovery = match workspace.child(Path::new(RECOVERY)) {
        Ok(recovery) => recovery,
        Err(error) if crate::package_fs::is_not_found(&error) => return Ok(()),
        Err(error) => return Err(error),
    };
    for name in recovery.directory_names()? {
        let operation = w.path.join(RECOVERY).join(&name);
        let output = recovery.child(Path::new(&name))?;
        let mut file = match output.read(Path::new("transaction.json")) {
            Ok(file) => file,
            Err(error) if crate::package_fs::is_not_found(&error) => continue,
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        file.by_ref().take(LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > LIMIT {
            bail!("恢复记录过大");
        }
        let mut t: Transaction =
            serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("恢复记录格式无效"))?;
        if matches!(t.phase.as_str(), "committed" | "rolled_back") {
            continue;
        }
        if t.workspace != *w
            || t.workspace_signature != workspace.identity()?
            || ![w.path.join("skills"), w.path.join("skills-inactive")]
                .iter()
                .any(|p| t.target.parent() == Some(p.as_path()))
            || t.backup
                .as_ref()
                .is_some_and(|p| *p != operation.join("previous"))
        {
            bail!("恢复记录身份或路径不符，保留现场");
        }
        crate::library::validate_name(
            t.target
                .file_name()
                .and_then(|p| p.to_str())
                .context("恢复记录名称无效")?,
        )?;
        if let Some(original) = &t.original {
            let name = t.target.file_name().context("恢复记录名称无效")?;
            if t.target != w.path.join("skills").join(name)
                || *original != w.path.join("skills-inactive").join(name)
                || t.backup.is_none() || t.before == "absent"
            {
                bail!("恢复记录原安装位置无效，保留现场");
            }
        }
        let parent = workspace.child(Path::new(
            t.target
                .parent()
                .and_then(|p| p.file_name())
                .context("恢复记录目录无效")?,
        ))?;
        let original = t.original.as_deref().unwrap_or(&t.target);
        let input = workspace.child(Path::new(installation_directory(w, original)?))?;
        #[cfg(test)]
        BEFORE_RECOVERY_MUTATION.with(|hook| {
            if let Some(callback) = hook.borrow_mut().take() {
                callback();
            }
        });
        workspace.verify_location(&w.path)?;
        recovery.verify_location(&w.path.join(RECOVERY))?;
        output.verify_location(&operation)?;
        restore_with_handles(&t, &output, &parent, &input)?;
        t.phase = "rolled_back".into();
        output.write(
            Path::new("transaction.json"),
            &serde_json::to_vec_pretty(&t)?,
        )?;
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{package_fs, update_protection};
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join("proma");
        let project = root.join("agent-workspaces/pilot");
        let source = temp.path().join("library/example");
        fs::create_dir_all(project.join("skills")).unwrap();
        fs::create_dir_all(source.join("local-data")).unwrap();
        fs::write(
            root.join("agent-workspaces.json"),
            r#"{"version":1,"workspaces":[{"id":"workspace-1","slug":"pilot","name":"Pilot"}]}"#,
        )
        .unwrap();
        fs::write(
            source.join("SKILL.md"),
            "---\nname: example\ndescription: Example\n---\nRun scripts/example.py.\n",
        )
        .unwrap();
        fs::write(
            source.join("local-data/ADAPTATION.md"),
            "Use the already installed local-data runtime.",
        )
        .unwrap();
        fs::write(source.join("local-data/private.env"), "fixture-secret").unwrap();
        (temp, root, project, source)
    }

    #[test]
    fn storage_controlled_move_keeps_marker_and_private_link_then_allows_same_source_update() {
        let (temp, root, project, source) = fixture();
        apply_plan(&plan_install(&root, &project, &source, "example").unwrap()).unwrap();
        let copy = project.join("skills/example");
        let marker_before = fs::read(copy.join(MARKER)).unwrap();
        let private_link = fs::read_link(copy.join("local-data")).unwrap();
        let source_root = source.parent().unwrap().canonicalize().unwrap();
        let old_source = source.canonicalize().unwrap();
        let mut library = crate::SkillLibrary::open_in(temp.path().canonicalize().unwrap().join("Kitter")).unwrap();
        library.config.library_dir = source_root;
        library.save().unwrap();
        let target = temp.path().canonicalize().unwrap().join("moved-library");
        let plan = library.plan_storage_change(crate::library::StorageAction::MoveLibrary, &target).unwrap();
        library.apply_storage_change(&plan).unwrap();
        let moved_source = target.join("example");
        let workspace = workspace_for_path(&root, &project).unwrap();
        assert_eq!(marker(&copy, &workspace).unwrap().source, moved_source);
        assert_eq!(fs::read(copy.join(MARKER)).unwrap(), marker_before);
        assert_eq!(fs::read_link(copy.join("local-data")).unwrap(), private_link);
        assert_eq!(clean_marker(&copy, &workspace).unwrap().source, moved_source);
        let unchanged = plan_install(&root, &project, &old_source, "example").unwrap();
        assert_eq!(unchanged.action, "unchanged");
        assert_eq!(unchanged.local_data, Some(private_link.clone()));
        apply_plan(&unchanged).unwrap();
        assert_eq!(fs::read(copy.join(MARKER)).unwrap(), marker_before);
        fs::write(moved_source.join("SKILL.md"), "---\nname: example\n---\nUpdated public fixture\n").unwrap();
        let update = plan_install(&root, &project, &moved_source, "example").unwrap();
        assert_eq!(update.action, "update");
        assert_eq!(update.local_data, Some(private_link.clone()));
        apply_plan(&update).unwrap();
        assert_eq!(fs::read_link(copy.join("local-data")).unwrap(), private_link);
        assert_eq!(fs::read(copy.join("local-data/private.env")).unwrap(), b"fixture-secret");
        fs::write(copy.join("SKILL.md"), "local copy edit").unwrap();
        assert!(plan_install(&root, &project, &moved_source, "example").is_err());
        let mut other_workspace = workspace.clone();
        other_workspace.id = "different-workspace".into();
        assert!(marker(&copy, &other_workspace).is_err());
    }

    #[test]
    fn storage_unapproved_source_alias_does_not_inherit_proma_marker_rights() {
        let (temp, root, project, source) = fixture();
        apply_plan(&plan_install(&root, &project, &source, "example").unwrap()).unwrap();
        let original_root = source.parent().unwrap().canonicalize().unwrap();
        let target = temp.path().canonicalize().unwrap().join("uncontrolled-library");
        fs::rename(&original_root, &target).unwrap();
        crate::directory_link::create(&target, &original_root).unwrap();
        let workspace = workspace_for_path(&root, &project).unwrap();
        let copy = project.join("skills/example");
        assert_eq!(marker(&copy, &workspace).unwrap().source, original_root.join("example"));
        assert!(plan_install(&root, &project, &target.join("example"), "example").is_err());
        assert!(clean_marker(&copy, &workspace).is_ok());
    }

    fn builtin_fixture() -> (
        tempfile::TempDir, PathBuf, PathBuf, BTreeMap<String, BTreeSet<[u8; 32]>>,
    ) {
        let (temp, root, project, source) = fixture();
        let templates = root.join("fixture-app/default-skills");
        let template = templates.join("example");
        let copy = project.join("skills/example");
        fs::create_dir_all(&template).unwrap();
        fs::create_dir_all(copy.join("scripts")).unwrap();
        fs::create_dir_all(copy.join("local-data/empty")).unwrap();
        fs::copy(source.join("SKILL.md"), template.join("SKILL.md")).unwrap();
        fs::copy(template.join("SKILL.md"), copy.join("SKILL.md")).unwrap();
        fs::write(copy.join("scripts/run.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::write(copy.join(".extra.bin"), [0, 1, 255]).unwrap();
        fs::write(copy.join("local-data/notes.txt"), "keep local notes").unwrap();
        let bundles = bundled_entry_fingerprints_in(&root.join("default-skills"), &templates);
        (temp, root, project, bundles)
    }

    fn replace_previewed_entry(project: &Path, path: &Path, change: &str) {
        match change {
            "package" => {
                let saved = project.join("saved-package");
                fs::rename(path, &saved).unwrap();
                package_fs::copy_tree(&saved, path).unwrap();
                let reference = saved.join("local-data");
                if let Ok(metadata) = fs::symlink_metadata(&reference) {
                    if metadata.file_type().is_symlink() {
                        std::os::unix::fs::symlink(fs::read_link(&reference).unwrap(), path.join("local-data")).unwrap();
                    } else if metadata.is_dir() {
                        package_fs::copy_tree(&reference, &path.join("local-data")).unwrap();
                    }
                }
            }
            "parent" => {
                let saved = project.join("saved-parent");
                fs::rename(path.parent().unwrap(), &saved).unwrap();
                fs::create_dir(path.parent().unwrap()).unwrap();
                fs::rename(saved.join(path.file_name().unwrap()), path).unwrap();
            }
            "workspace" => {
                let saved = project.with_file_name("saved-workspace");
                fs::rename(project, &saved).unwrap();
                fs::create_dir(project).unwrap();
                fs::rename(saved.join("skills"), project.join("skills")).unwrap();
            }
            "content" => {
                fs::write(path.join("new-content"), "same inode, new version").unwrap();
                if path.join(MARKER).exists() {
                    let mut m: Marker = read_json(&path.join(MARKER)).unwrap();
                    m.content_hash = content_hash(path).unwrap();
                    fs::write(path.join(MARKER), serde_json::to_vec(&m).unwrap()).unwrap();
                }
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn previewed_removal_rejects_replacement_and_same_source_updates() {
        for change in ["package", "parent", "workspace", "content", "updated"] {
            let (_temp, root, project, source) = fixture();
            apply_plan(&plan_install(&root, &project, &source, "example").unwrap()).unwrap();
            let workspace = workspace_for_path(&root, &project).unwrap();
            let copy = inspect_in(&root, &project).unwrap().remove(0);
            if change == "updated" {
                fs::write(source.join("new-version"), "v2").unwrap();
                apply_plan(&plan_install(&root, &project, &source, "example").unwrap()).unwrap();
            } else { replace_previewed_entry(&project, &copy.path, change); }
            let before = observed(&copy.path).unwrap();
            let source_file = source.join("SKILL.md").canonicalize().unwrap();
            assert!(uninstall_previewed_in(&root, &workspace, &copy, &source_file).is_err(), "{change}");
            assert_eq!(observed(&copy.path).unwrap(), before, "{change}");
            assert!(source.join("SKILL.md").is_file());
        }
    }

    #[test]
    fn previewed_toggle_rejects_replacement_after_preview_under_the_write_lock() {
        for change in ["package", "parent", "workspace", "content"] {
            let (_temp, root, project, _) = builtin_fixture();
            register_test_bundled_templates(&root, &root.join("fixture-app/default-skills"));
            let workspace = workspace_for_path(&root, &project).unwrap();
            let copy = inspect_in(&root, &project).unwrap().remove(0);
            let native = project.clone();
            let path = copy.path.clone();
            BEFORE_TOGGLE_LOCK.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
                replace_previewed_entry(&native, &path, change);
            })));
            assert!(set_enabled_previewed_in(&root, &workspace, &copy, None, false).is_err(), "{change}");
            assert!(copy.path.join("SKILL.md").is_file());
            assert!(!project.join("skills-inactive").exists());
        }
    }

    #[test]
    fn preview_snapshots_reject_missing_or_mutated_display_records_and_allow_unchanged_toggle() {
        let (_temp, root, project, _) = builtin_fixture();
        register_test_bundled_templates(&root, &root.join("fixture-app/default-skills"));
        let workspace = workspace_for_path(&root, &project).unwrap();
        let copy = inspect_in(&root, &project).unwrap().remove(0);
        validate_previewed_installation(&root, &workspace, &copy).unwrap();
        let display = serde_json::to_value(&copy).unwrap();
        assert!(display.get("snapshot").is_none());
        assert_eq!(display["name"], "example");
        let mut missing = copy.clone();
        missing.snapshot = None;
        assert!(!missing.can_toggle());
        assert!(validate_previewed_installation(&root, &workspace, &missing).is_err());
        assert!(set_enabled_previewed_in(&root, &workspace, &missing, None, false).is_err());
        let mut altered = copy.clone();
        altered.source = Some(project.join("foreign-source"));
        assert!(validate_previewed_installation(&root, &workspace, &altered).is_err());
        set_enabled_previewed_in(&root, &workspace, &copy, None, false).unwrap();
        let fresh = inspect_in(&root, &project).unwrap().remove(0);
        set_enabled_previewed_in(&root, &workspace, &fresh, None, true).unwrap();
    }

    #[test]
    fn previewed_removal_allows_unchanged_objects_and_immediate_removal_can_sample_a_new_copy() {
        let (_temp, root, project, source) = fixture();
        apply_plan(&plan_install(&root, &project, &source, "example").unwrap()).unwrap();
        let workspace = workspace_for_path(&root, &project).unwrap();
        let copy = inspect_in(&root, &project).unwrap().remove(0);
        let source_file = source.join("SKILL.md").canonicalize().unwrap();
        // A source edit alone does not change the previewed installed object.
        fs::write(source.join("upstream"), "v2").unwrap();
        uninstall_previewed_in(&root, &workspace, &copy, &source_file).unwrap();
        assert!(fs::symlink_metadata(&copy.path).is_err());
        apply_plan(&plan_install(&root, &project, &source, "example").unwrap()).unwrap();
        let copy = inspect_in(&root, &project).unwrap().remove(0);
        replace_previewed_entry(&project, &copy.path, "package");
        assert!(uninstall_previewed_in(&root, &workspace, &copy, &source_file).is_err());
        uninstall_at(&root, &workspace, &copy.path, None).unwrap();
        assert!(fs::symlink_metadata(&copy.path).is_err());
        assert!(source.join("SKILL.md").is_file());
    }

    #[test]
    fn link_snapshots_reject_retargeting_before_or_during_removal_planning() {
        for indirect in [false, true] {
            let (_temp, root, project, source) = fixture();
            let other = source.with_file_name("other");
            fs::create_dir(&other).unwrap();
            fs::write(other.join("SKILL.md"), "other source").unwrap();
            let path = project.join("skills/example");
            let changed = if indirect {
                let bridge = project.join("bridge");
                std::os::unix::fs::symlink(&source, &bridge).unwrap();
                std::os::unix::fs::symlink("../bridge", &path).unwrap();
                bridge
            } else {
                std::os::unix::fs::symlink(&source, &path).unwrap();
                path.clone()
            };
            let workspace = workspace_for_path(&root, &project).unwrap();
            let copy = inspect_in(&root, &project).unwrap().remove(0);
            validate_previewed_installation(&root, &workspace, &copy).unwrap();
            fs::rename(&changed, project.join("saved-link")).unwrap();
            std::os::unix::fs::symlink(&other, &changed).unwrap();
            let newer_plan = plan_remove_link(&root, &project, &path).unwrap();
            assert!(validate_previewed_installation(&root, &workspace, &copy).is_err());
            assert_eq!(fs::read_link(&path).unwrap(), newer_plan.raw_target);
            let fresh = inspect_in(&root, &project).unwrap().remove(0);
            validate_previewed_installation(&root, &workspace, &fresh).unwrap();
            assert_eq!(fresh.source, Some(other.canonicalize().unwrap()));
            assert!(source.join("SKILL.md").is_file() && other.join("SKILL.md").is_file());
        }
    }

    #[test]
    fn inspection_rejects_changes_between_the_before_and_after_snapshot() {
        for change in ["package", "parent", "workspace", "content"] {
            let (_temp, root, project, source) = fixture();
            apply_plan(&plan_install(&root, &project, &source, "example").unwrap()).unwrap();
            let path = project.join("skills/example");
            let native = project.clone();
            BEFORE_INSPECTION_RECHECK.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
                replace_previewed_entry(&native, &path, change);
            })));
            assert!(inspect_in(&root, &project).is_err(), "{change}");
            let fresh = inspect_in(&root, &project).unwrap().remove(0);
            validate_previewed_installation(&root, &workspace_for_path(&root, &project).unwrap(), &fresh).unwrap();
        }
    }

    #[test]
    fn regression_local_preset_variants_keep_origin_without_gaining_toggle_rights() {
        for inactive in [false, true] {
            let (_temp, root, project, bundles) = builtin_fixture();
            let active = project.join("skills/example");
            assert!(inspect_with_bundles(&root, &project, &bundles).unwrap()[0].can_toggle());
            let cached = root.join("default-skills/example");
            fs::create_dir_all(&cached).unwrap();
            fs::write(cached.join("SKILL.md"), "locally adapted or older preset").unwrap();
            fs::copy(cached.join("SKILL.md"), active.join("SKILL.md")).unwrap();
            let path = if inactive {
                fs::create_dir(project.join("skills-inactive")).unwrap();
                let path = project.join("skills-inactive/example");
                fs::rename(&active, &path).unwrap(); path
            } else { active };
            let before = observed(&path).unwrap();
            let copy = inspect_with_bundles(&root, &project, &bundles).unwrap().remove(0);
            assert!(copy.bundled, "matching configured preset variants keep their display origin");
            assert_eq!(copy.status, "preset_variant");
            assert!(!copy.can_toggle() && !copy.removable);
            assert!(set_enabled_path_with_bundles(&root, &path, inactive, &bundles).is_err());
            assert_eq!(observed(&path).unwrap(), before);
        }
    }

    #[test]
    fn preset_discovery_follows_app_updates_across_active_and_inactive_workspaces() {
        let (_temp, root, project, _) = builtin_fixture();
        let templates = root.join("fixture-app/default-skills");
        register_test_bundled_templates(&root, &templates);
        let second = root.join("agent-workspaces/second");
        fs::create_dir_all(second.join("skills-inactive")).unwrap();
        fs::write(root.join("agent-workspaces.json"), serde_json::to_vec(&serde_json::json!({
            "version":2,"workspaces":[
                {"id":"workspace-1","slug":"pilot","name":"Pilot"},
                {"id":"workspace-2","slug":"second","name":"Second"}
            ]
        })).unwrap()).unwrap();
        let name = "future-preset";
        let app = templates.join(name);
        let cached = root.join("default-skills").join(name);
        let copies = [project.join("skills").join(name), second.join("skills-inactive").join(name)];
        for directory in [&app, &cached, &copies[0], &copies[1]] {
            fs::create_dir_all(directory).unwrap();
            fs::write(directory.join("SKILL.md"), "new preset version one").unwrap();
        }
        let inspect = || [&project, &second].map(|workspace| {
            inspect_in(&root, workspace).unwrap().into_iter().find(|item| item.name == name).unwrap()
        });
        for copy in inspect() {
            assert!(copy.bundled && copy.can_toggle(), "new app presets require no fixed slug list");
        }
        fs::write(app.join("SKILL.md"), "new preset version two").unwrap();
        for copy in inspect() {
            assert!(copy.bundled && !copy.can_toggle() && !copy.removable);
            assert_eq!(copy.status, "preset_variant", "old configured copies retain display origin");
        }
        for directory in [&cached, &copies[0], &copies[1]] {
            fs::copy(app.join("SKILL.md"), directory.join("SKILL.md")).unwrap();
        }
        for copy in inspect() {
            assert!(copy.bundled && copy.can_toggle(), "updated copies are reverified on each scan");
        }
        let renamed = templates.join("renamed-preset");
        fs::rename(&app, &renamed).unwrap();
        for copy in inspect() {
            assert!(!copy.bundled && !copy.can_toggle() && !copy.removable,
                "a removed app entry never grants stale preset or user-uninstall rights");
            assert!(copy.path.join("SKILL.md").is_file(), "scanning preserves retired copies");
        }
        let renamed_copy = second.join("skills-inactive/renamed-preset");
        fs::create_dir(&renamed_copy).unwrap();
        fs::copy(renamed.join("SKILL.md"), renamed_copy.join("SKILL.md")).unwrap();
        let copy = inspect_in(&root, &second).unwrap().into_iter().find(|item| item.name == "renamed-preset").unwrap();
        assert!(copy.bundled && copy.can_toggle(), "renamed app entries are discovered without a code change");
        fs::rename(&templates, root.join("unavailable-app-templates")).unwrap();
        for copy in inspect_in(&root, &second).unwrap() {
            assert!(!copy.bundled && !copy.can_toggle() && !copy.removable,
                "missing application evidence does not trust stale cache entries");
        }
    }

    #[test]
    fn builtin_toggle_round_trip_preserves_the_entire_directory() {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, root, project, bundles) = builtin_fixture();
        let active = project.join("skills/example");
        let inactive = project.join("skills-inactive/example");
        fs::set_permissions(active.join("scripts/run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        let body = fs::read(active.join("SKILL.md")).unwrap();
        let before = observed(&active).unwrap();
        let scan = inspect_with_bundles(&root, &project, &bundles).unwrap();
        assert!(scan[0].bundled && scan[0].can_toggle());
        assert!(!scan[0].managed && !scan[0].removable);
        for (path, enabled, destination) in [(&active, false, &inactive), (&inactive, true, &active)] {
            set_enabled_path_with_bundles(&root, path, enabled, &bundles).unwrap();
            assert!(!path.exists());
            assert_eq!(observed(destination).unwrap(), before);
            assert_eq!(fs::read(destination.join("SKILL.md")).unwrap(), body);
            assert_eq!(fs::read(destination.join(".extra.bin")).unwrap(), [0, 1, 255]);
            assert_eq!(fs::read_to_string(destination.join("local-data/notes.txt")).unwrap(), "keep local notes");
            assert!(destination.join("local-data/empty").is_dir());
            assert!(!destination.join(MARKER).exists());
            assert!(inspect_with_bundles(&root, &project, &bundles).unwrap()[0].can_toggle());
        }
    }

    #[test]
    fn test_bundled_registration_is_scoped_to_canonical_root_and_thread() {
        let (_temp, root, project, bundles) = builtin_fixture();
        let (_other, other_root, _, _) = builtin_fixture();
        let baseline = bundled_entry_fingerprints(&root);
        let other_baseline = bundled_entry_fingerprints(&other_root);
        register_test_bundled_templates(&root, &root.join("fixture-app/default-skills"));
        assert_eq!(bundled_entry_fingerprints(&root.canonicalize().unwrap()), bundles);
        assert!(inspect_in(&root, &project).unwrap()[0].can_toggle());
        assert_eq!(bundled_entry_fingerprints(&other_root), other_baseline);
        assert_eq!(std::thread::spawn(move || bundled_entry_fingerprints(&root)).join().unwrap(), baseline);
    }

    #[test]
    fn toggle_revalidates_app_templates_and_rejects_unverified_cached_versions() {
        for cached in [false, true] {
            let (_temp, root, project, _) = builtin_fixture();
            let templates = root.join("fixture-app/default-skills");
            register_test_bundled_templates(&root, &templates);
            let active = project.join("skills/example");
            let template = if cached {
                let template = root.join("default-skills/example");
                fs::create_dir_all(&template).unwrap();
                fs::write(template.join("SKILL.md"), "previous bundled version").unwrap();
                fs::copy(template.join("SKILL.md"), active.join("SKILL.md")).unwrap();
                template
            } else { templates.join("example") };
            let before = observed(&active).unwrap();
            if cached {
                assert!(!inspect_in(&root, &project).unwrap()[0].can_toggle());
                assert!(set_enabled_path_in(&root, &active, false).is_err());
                assert_eq!(observed(&active).unwrap(), before);
                assert!(!project.join("skills-inactive").exists());
                continue;
            }
            assert!(inspect_in(&root, &project).unwrap()[0].can_toggle());
            BEFORE_TOGGLE_LOCK.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
                fs::remove_file(template.join("SKILL.md")).unwrap();
            })));
            assert!(set_enabled_path_in(&root, &active, false).is_err());
            assert_eq!(observed(&active).unwrap(), before);
            assert!(!project.join("skills-inactive").exists());
        }
    }

    #[test]
    fn toggle_never_replaces_same_name_targets_in_either_direction() {
        for enabled in [false, true] {
            for dangling in [false, true] {
                let (_temp, root, project, bundles) = builtin_fixture();
                let active = project.join("skills/example");
                let inactive = project.join("skills-inactive/example");
                fs::create_dir_all(inactive.parent().unwrap()).unwrap();
                let (source, destination) = if enabled {
                    fs::rename(&active, &inactive).unwrap();
                    (&inactive, &active)
                } else { (&active, &inactive) };
                if dangling {
                    std::os::unix::fs::symlink(project.join("absent"), destination).unwrap();
                } else {
                    fs::create_dir(destination).unwrap();
                }
                let before = observed(source).unwrap();
                let target_before = observed(destination).unwrap();
                assert!(set_enabled_path_with_bundles(&root, source, enabled, &bundles).is_err());
                assert_eq!(observed(source).unwrap(), before);
                assert_eq!(observed(destination).unwrap(), target_before);
            }
        }
    }

    #[test]
    fn managed_user_copies_never_toggle_even_when_clean_or_pending() {
        let (_temp, root, project, source) = fixture();
        let plan = plan_install(&root, &project, &source, "example").unwrap();
        apply_plan(&plan).unwrap();
        let scan = inspect_in(&root, &project).unwrap();
        assert!(scan[0].managed && scan[0].removable && !scan[0].can_toggle());
        fs::write(source.join("SKILL.md"), "new upstream version").unwrap();
        let scan = inspect_in(&root, &project).unwrap();
        assert_eq!(scan[0].status, "update_pending");
        assert!(scan[0].removable && !scan[0].can_toggle());
        let before = observed(&plan.destination).unwrap();
        let inactive = project.join("skills-inactive/example");
        let fingerprint = skill_entry_fingerprint(&plan.destination.join("SKILL.md")).unwrap();
        let bundles = BTreeMap::from([("example".into(), BTreeSet::from([fingerprint]))]);
        assert!(set_enabled_path_with_bundles(&root, &plan.destination, false, &bundles).is_err());
        assert!(!inactive.exists());
        fs::create_dir(inactive.parent().unwrap()).unwrap();
        fs::rename(&plan.destination, &inactive).unwrap();
        assert!(!inspect_with_bundles(&root, &project, &bundles).unwrap()[0].can_toggle());
        assert!(set_enabled_path_with_bundles(&root, &inactive, true, &bundles).is_err());
        assert_eq!(observed(&inactive).unwrap(), before);
        fs::rename(&inactive, &plan.destination).unwrap();
        assert_eq!(observed(&plan.destination).unwrap(), before);
        let original = fs::read(plan.destination.join("SKILL.md")).unwrap();
        for edit_entry in [true, false] {
            if edit_entry {
                fs::write(plan.destination.join("SKILL.md"), "local edit").unwrap();
            } else {
                fs::write(plan.destination.join("SKILL.md"), &original).unwrap();
                fs::write(plan.destination.join("new.txt"), "local addition").unwrap();
            }
            // Even an exact application template cannot override a dirty marker.
            let fingerprint = skill_entry_fingerprint(&plan.destination.join("SKILL.md")).unwrap();
            let bundles = BTreeMap::from([("example".into(), BTreeSet::from([fingerprint]))]);
            assert!(!inspect_with_bundles(&root, &project, &bundles).unwrap()[0].can_toggle());
            let before = observed(&plan.destination).unwrap();
            assert!(set_enabled_path_with_bundles(&root, &plan.destination, false, &bundles).is_err());
            assert_eq!(observed(&plan.destination).unwrap(), before);
        }
    }

    #[test]
    fn toggle_rechecks_presets_and_rejects_unmanaged_or_linked_entries() {
        for kind in ["mismatch", "unmanaged", "external", "entry_link", "invalid_marker", "marker_link", "integration"] {
            let (_temp, root, project, mut bundles) = builtin_fixture();
            let copy = project.join("skills/example");
            assert!(inspect_with_bundles(&root, &project, &bundles).unwrap()[0].can_toggle());
            match kind {
                "mismatch" => fs::write(copy.join("SKILL.md"), "changed after UI snapshot").unwrap(),
                "unmanaged" => bundles.clear(),
                "external" => {
                    fs::rename(&copy, project.join("external")).unwrap();
                    std::os::unix::fs::symlink(project.join("external"), &copy).unwrap();
                }
                "entry_link" => {
                    fs::rename(copy.join("SKILL.md"), project.join("external.md")).unwrap();
                    std::os::unix::fs::symlink(project.join("external.md"), copy.join("SKILL.md")).unwrap();
                }
                "invalid_marker" => fs::write(copy.join(MARKER), "invalid").unwrap(),
                "marker_link" => std::os::unix::fs::symlink(project.join("missing-marker"), copy.join(MARKER)).unwrap(),
                "integration" => {
                    let body = "Nowledge Mem integration";
                    let entry = project.join("skills/status");
                    fs::rename(&copy, &entry).unwrap();
                    let template = root.join("default-skills/status");
                    fs::create_dir_all(&template).unwrap();
                    fs::write(template.join("SKILL.md"), body).unwrap();
                    fs::write(entry.join("SKILL.md"), body).unwrap();
                    // Integration identity still wins over a matching bundle.
                    bundles.insert("status".into(), BTreeSet::from([skill_entry_fingerprint(&entry.join("SKILL.md")).unwrap()]));
                }
                _ => unreachable!(),
            }
            let path = if kind == "integration" { project.join("skills/status") } else { copy };
            let before = observed(&path).unwrap();
            let scan = inspect_with_bundles(&root, &project, &bundles).unwrap();
            assert!(!scan[0].can_toggle(), "{kind}");
            assert!(set_enabled_path_with_bundles(&root, &path, false, &bundles).is_err(), "{kind}");
            assert_eq!(observed(&path).unwrap(), before, "{kind}");
            assert!(!project.join("skills-inactive").exists());
        }
    }

    #[test]
    fn toggle_requires_registered_native_path_and_expected_source_directory() {
        let (temp, root, project, bundles) = builtin_fixture();
        let active = project.join("skills/example");
        let before = observed(&active).unwrap();
        assert!(set_enabled_path_with_bundles(&root, &active, true, &bundles).is_err());
        assert!(set_enabled_path_with_bundles(&root, Path::new("skills/example"), false, &bundles).is_err());
        let unrelated = root.join("agent-workspaces/unregistered");
        let business = temp.path().join("business");
        for workspace in [&unrelated, &business] {
            fs::create_dir_all(workspace.join("skills/example")).unwrap();
            fs::copy(active.join("SKILL.md"), workspace.join("skills/example/SKILL.md")).unwrap();
            let path = workspace.join("skills/example");
            let original = observed(&path).unwrap();
            assert!(set_enabled_path_with_bundles(&root, &path, false, &bundles).is_err());
            assert_eq!(observed(&path).unwrap(), original);
            assert!(!workspace.join("skills-inactive").exists());
        }
        // A Finder association does not make its Skill directory native.
        fs::write(root.join("agent-workspaces.json"), serde_json::to_vec(&serde_json::json!({
            "version": 2, "workspaces": [{"id": "workspace-1", "slug": "pilot", "name": "Pilot", "projectRootPath": business}]
        })).unwrap()).unwrap();
        assert!(set_enabled_path_with_bundles(&root, &business.join("skills/example"), false, &bundles).is_err());
        fs::create_dir_all(project.join("other/example")).unwrap();
        assert!(set_enabled_path_with_bundles(&root, &project.join("other/example"), false, &bundles).is_err());
        let alias = temp.path().join("native-alias");
        std::os::unix::fs::symlink(&project, &alias).unwrap();
        assert!(set_enabled_path_with_bundles(&root, &alias.join("skills/example"), false, &bundles).is_err());
        assert_eq!(observed(&active).unwrap(), before);
        assert!(!project.join("skills-inactive").exists());
    }

    #[test]
    fn toggle_rejects_pending_recovery_and_a_concurrent_writer() {
        let (_temp, root, project, bundles) = builtin_fixture();
        let active = project.join("skills/example");
        let before = observed(&active).unwrap();
        let workspace = Directory::open_anchored(&project).unwrap();
        let lock = workspace.lock(Path::new(".kitter-proma.lock")).unwrap();
        assert!(set_enabled_path_with_bundles(&root, &active, false, &bundles).is_err());
        drop(lock);
        let pending = project.join(RECOVERY).join("pending");
        fs::create_dir_all(&pending).unwrap();
        let transaction = Transaction {
            workspace: workspace_for_path(&root, &project).unwrap(), workspace_signature: workspace.identity().unwrap(),
            target: active.clone(), original: None, backup: None, before: before.clone(), after: String::new(), phase: "prepared".into(),
        };
        fs::write(pending.join("transaction.json"), serde_json::to_vec(&transaction).unwrap()).unwrap();
        let scan = inspect_with_bundles(&root, &project, &bundles).unwrap();
        assert!(scan[0].bundled && !scan[0].can_toggle());
        assert!(set_enabled_path_with_bundles(&root, &active, false, &bundles).is_err());
        assert_eq!(observed(&active).unwrap(), before);
        assert!(!project.join("skills-inactive").exists());
    }

    #[test]
    fn toggle_restores_a_package_changed_in_the_last_rename_gap() {
        for reoccupy in [false, true] {
            let (_temp, root, project, bundles) = builtin_fixture();
            let active = project.join("skills/example");
            let changed = active.clone();
            let inactive = project.join("skills-inactive/example");
            if reoccupy {
                AFTER_TOGGLE_MOVE.with(|hook| {
                    *hook.borrow_mut() = Some(Box::new(move || {
                        fs::write(inactive.join("extra.txt"), "concurrent edit").unwrap();
                        fs::create_dir(&changed).unwrap();
                        fs::write(changed.join("keep.txt"), "new occupant").unwrap();
                    }));
                });
            } else {
                BEFORE_TOGGLE_MOVE.with(|hook| {
                    *hook.borrow_mut() = Some(Box::new(move || {
                        fs::write(changed.join("extra.txt"), "concurrent edit").unwrap();
                    }));
                });
            }
            assert!(set_enabled_path_with_bundles(&root, &active, false, &bundles).is_err());
            if reoccupy {
                assert_eq!(fs::read_to_string(active.join("keep.txt")).unwrap(), "new occupant");
                assert!(project.join("skills-inactive/example/extra.txt").is_file());
            } else {
                assert_eq!(fs::read_to_string(active.join("extra.txt")).unwrap(), "concurrent edit");
                assert!(active.join("SKILL.md").is_file());
                assert!(!project.join("skills-inactive/example").exists());
            }
        }
    }

    #[test]
    fn removal_availability_matches_mirror_and_recovery_guards() {
        let (_temp, root, project, source) = fixture();
        let plan = plan_install(&root, &project, &source, "example").unwrap();
        apply_plan(&plan).unwrap();
        let original = fs::read(plan.destination.join("SKILL.md")).unwrap();
        assert!(inspect_in(&root, &project).unwrap()[0].removable);

        fs::write(source.join("SKILL.md"), "upstream changed").unwrap();
        let scan = inspect_in(&root, &project).unwrap();
        assert_eq!(scan[0].status, "update_pending");
        assert!(scan[0].removable);

        fs::create_dir_all(project.join("skills-inactive")).unwrap();
        let inactive = project.join("skills-inactive/example");
        fs::rename(&plan.destination, &inactive).unwrap();
        assert!(inspect_in(&root, &project).unwrap()[0].removable);
        fs::write(inactive.join("SKILL.md"), "local edit").unwrap();
        let scan = inspect_in(&root, &project).unwrap();
        assert_eq!(scan[0].status, "modified");
        assert!(!scan[0].removable);

        let external = project.join("skills/external");
        std::os::unix::fs::symlink(&source, &external).unwrap();
        let unmanaged = project.join("skills/unmanaged");
        fs::create_dir_all(&unmanaged).unwrap();
        fs::write(unmanaged.join("SKILL.md"), "unmanaged").unwrap();
        let scan = inspect_in(&root, &project).unwrap();
        assert!(scan.iter().filter(|item| item.name != "external").all(|item| !item.removable));
        let linked = scan.iter().find(|item| item.name == "external").unwrap();
        assert!(linked.removable && !linked.managed && !linked.can_toggle());
        assert_eq!(linked.source.as_ref(), Some(&source.canonicalize().unwrap()));
        assert!(scan.iter().any(|item| item.status == "external_link"));
        assert!(scan.iter().any(|item| item.status == "unmanaged"));

        fs::write(inactive.join("SKILL.md"), original).unwrap();
        assert!(inspect_in(&root, &project).unwrap().iter().find(|item| item.name == "example").unwrap().removable);
        let pending = project.join(RECOVERY).join("pending");
        fs::create_dir_all(&pending).unwrap();
        let transaction = Transaction {
            workspace: plan.workspace.clone(), workspace_signature: plan.workspace_signature,
            target: inactive, original: None, backup: None, before: String::new(), after: String::new(),
            phase: "prepared".into(),
        };
        fs::write(pending.join("transaction.json"), serde_json::to_vec(&transaction).unwrap()).unwrap();
        assert!(inspect_in(&root, &project).unwrap().iter().all(|item| !item.removable));
    }

    #[test]
    fn integration_presets_require_template_and_provider_match_without_removal_rights() {
        let (_temp, root, project, _source) = fixture();
        let template = root.join("default-skills/status");
        let entry = project.join("skills/status");
        fs::create_dir_all(&template).unwrap();
        fs::create_dir_all(&entry).unwrap();
        let body = "---\nname: status\ndescription: Check Nowledge Mem from Proma\n---\nNowledge Mem integration.\n";
        fs::write(template.join("SKILL.md"), body).unwrap();
        fs::write(entry.join("SKILL.md"), body).unwrap();
        let scan = inspect_in(&root, &project).unwrap();
        assert_eq!(scan[0].integration.as_deref(), Some("Nowledge Mem"));
        assert!(!scan[0].bundled);
        assert!(!scan[0].removable);
        fs::write(entry.join("SKILL.md"), "custom status skill").unwrap();
        assert!(inspect_in(&root, &project).unwrap()[0].integration.is_none());
        fs::write(template.join("SKILL.md"), "custom status skill").unwrap();
        assert!(inspect_in(&root, &project).unwrap()[0].integration.is_none());
        fs::write(template.join("SKILL.md"), body).unwrap();
        fs::write(entry.join("SKILL.md"), body).unwrap();
        fs::create_dir_all(project.join("skills-inactive")).unwrap();
        fs::rename(&entry, project.join("skills-inactive/status")).unwrap();
        assert_eq!(inspect_in(&root, &project).unwrap()[0].integration.as_deref(), Some("Nowledge Mem"));
        std::os::unix::fs::symlink(&template, &entry).unwrap();
        assert!(inspect_in(&root, &project).unwrap().iter().find(|item| item.state == "active").unwrap().integration.is_none());
    }

    #[test]
    fn adapted_integrations_keep_origin_without_mutation_rights() {
        for name in ["distill-memory", "read-working-memory", "save-thread", "search-memory", "status"] {
            let (_temp, root, project, _source) = fixture();
            let template = root.join("default-skills").join(name);
            let body = format!("---\nname: {name}\ndescription: Use Nowledge Mem from Proma\n---\n# Integration\nUse Nowledge Mem on explicit request.\n```bash\nnmem status\n```\nKeep the original session messages.\n");
            let adapted = format!("{}\nLocal workspace routing: choose this project's memory space.\n", body.replace("nmem status", "NMEM_SPACE=project nmem status"));
            fs::create_dir_all(&template).unwrap();
            fs::write(template.join("SKILL.md"), &body).unwrap();
            for directory in ["skills", "skills-inactive"] {
                let entry = project.join(directory).join(name);
                fs::create_dir_all(entry.join("local-data")).unwrap();
                fs::write(entry.join("SKILL.md"), &adapted).unwrap();
                fs::write(entry.join("local-data/ADAPTATION.md"), "Keep project routing.").unwrap();
            }
            let scan = inspect_in(&root, &project).unwrap();
            assert_eq!(scan.len(), 2);
            for copy in &scan {
                assert_eq!(copy.integration.as_deref(), Some("Nowledge Mem"), "{name}");
                assert_eq!(copy.status, "integration_variant");
                assert!(!copy.managed && !copy.bundled && !copy.removable && !copy.can_toggle());
                assert!(set_enabled_path_in(&root, &copy.path, copy.state == "inactive").is_err());
                assert_eq!(fs::read_to_string(copy.path.join("SKILL.md")).unwrap(), adapted);
                assert_eq!(fs::read_to_string(copy.path.join("local-data/ADAPTATION.md")).unwrap(), "Keep project routing.");
            }
            assert_eq!(crate::proma_batch::installation_key(&scan[0]), crate::proma_batch::installation_key(&scan[1]));
            assert_eq!(crate::proma_batch::installation_key(&scan[0]), format!("proma:integration:Nowledge Mem:{name}"));
        }
    }

    #[test]
    fn adapted_integrations_require_matching_identity_and_unmarked_real_files() {
        let (_temp, root, project, _source) = fixture();
        let template = root.join("default-skills/save-thread");
        let entry = project.join("skills/save-thread");
        let body = "---\nname: save-thread\ndescription: Save a Proma session to Nowledge Mem\n---\n# Save Thread\nSave the current session to Nowledge Mem.\n";
        let adapted = format!("{body}\nUse this project's memory space.\n");
        fs::create_dir_all(&template).unwrap();
        fs::create_dir_all(&entry).unwrap();
        fs::write(template.join("SKILL.md"), body).unwrap();
        fs::write(entry.join("SKILL.md"), &adapted).unwrap();
        assert_eq!(inspect_in(&root, &project).unwrap()[0].integration.as_deref(), Some("Nowledge Mem"));
        for impostor in [
            "A same-name user skill mentioning Nowledge Mem.",
            "---\nname: other\ndescription: Save a Proma session to Nowledge Mem\n---\nNowledge Mem.\n",
            "---\nname: save-thread\ndescription: An unrelated skill\n---\nNowledge Mem.\n",
            "---\nname: save-thread\ndescription: Save a Proma session to Nowledge Mem\n---\nAn unrelated body.\n",
            "---\nname: save-thread\ndescription: Save a Proma session to Nowledge Mem\n---\nThis custom skill only mentions Nowledge Mem.\n",
            "---\nname: save-thread\ndescription: [invalid metadata\n---\nNowledge Mem.\n",
        ] {
            fs::write(entry.join("SKILL.md"), impostor).unwrap();
            let copy = inspect_in(&root, &project).unwrap().remove(0);
            assert!(copy.integration.is_none() && !copy.bundled && !copy.removable && !copy.can_toggle());
        }
        fs::write(entry.join("SKILL.md"), &adapted).unwrap();
        fs::remove_file(template.join("SKILL.md")).unwrap();
        assert!(inspect_in(&root, &project).unwrap()[0].integration.is_none());
        fs::write(template.join("SKILL.md"), body).unwrap();
        fs::write(entry.join(MARKER), "invalid managed marker").unwrap();
        assert!(inspect_in(&root, &project).unwrap()[0].integration.is_none());
        fs::remove_file(entry.join(MARKER)).unwrap();
        fs::remove_file(entry.join("SKILL.md")).unwrap();
        std::os::unix::fs::symlink(template.join("SKILL.md"), entry.join("SKILL.md")).unwrap();
        assert!(inspect_in(&root, &project).unwrap()[0].integration.is_none());
    }

    #[test]
    fn integration_variants_do_not_read_template_prose_from_code_blocks() {
        let header = "---\nname: save-thread\ndescription: Save a session to Nowledge Mem\n---\n";
        let prose = "# Save Thread\nSave the current session to Nowledge Mem.\n";
        let template = format!("{header}{prose}");
        let adapted = format!("{template}\nUse this workspace's memory space.\n");
        assert!(matches_integration_variant(template.as_bytes(), adapted.as_bytes(), "save-thread"));
        for quoted in [
            format!("{header}```text\n~~~\n{prose}~~~\n```\n"),
            format!("{header}````text\n```\n{prose}```\n````\n"),
        ] {
            assert!(!matches_integration_variant(template.as_bytes(), quoted.as_bytes(), "save-thread"));
        }
    }

    #[test]
    fn bundled_copies_require_matching_template_content_and_no_managed_marker() {
        let (_temp, root, project, source) = fixture();
        let app_templates = root.join("fixture-app/default-skills");
        let defaults = root.join("default-skills");
        let template = app_templates.join("example");
        let copy = project.join("skills/example");
        fs::create_dir_all(&template).unwrap();
        fs::create_dir_all(&copy).unwrap();
        fs::copy(source.join("SKILL.md"), template.join("SKILL.md")).unwrap();
        fs::copy(template.join("SKILL.md"), copy.join("SKILL.md")).unwrap();
        let inspect = || inspect_with_bundles(&root, &project, &bundled_entry_fingerprints_in(&defaults, &app_templates)).unwrap();
        let scan = inspect();
        assert!(scan[0].bundled);
        assert!(!scan[0].removable);
        let cached = defaults.join("example");
        fs::create_dir_all(&cached).unwrap();
        fs::write(cached.join("SKILL.md"), "previous app version").unwrap();
        fs::copy(cached.join("SKILL.md"), copy.join("SKILL.md")).unwrap();
        assert!(inspect()[0].bundled);
        assert_eq!(inspect()[0].status, "preset_variant");
        assert!(!inspect()[0].can_toggle());
        let extra = defaults.join("user-added");
        fs::create_dir_all(&extra).unwrap();
        fs::write(extra.join("SKILL.md"), "added later").unwrap();
        let extra_copy = project.join("skills/user-added");
        fs::create_dir_all(&extra_copy).unwrap();
        fs::copy(extra.join("SKILL.md"), extra_copy.join("SKILL.md")).unwrap();
        assert!(!inspect().iter().find(|item| item.name == "user-added").unwrap().bundled);
        assert!(bundled_entry_fingerprints_in(&defaults, &root.join("missing-app")).is_empty());
        fs::copy(template.join("SKILL.md"), copy.join("SKILL.md")).unwrap();
        fs::create_dir_all(project.join("skills-inactive")).unwrap();
        let inactive = project.join("skills-inactive/example");
        fs::rename(&copy, &inactive).unwrap();
        assert!(inspect()[0].bundled);
        fs::write(inactive.join("SKILL.md"), "same name, custom content").unwrap();
        assert!(!inspect()[0].bundled);

        let linked_template = app_templates.join("linked");
        fs::create_dir_all(&linked_template).unwrap();
        fs::copy(template.join("SKILL.md"), linked_template.join("SKILL.md")).unwrap();
        std::os::unix::fs::symlink(&linked_template, project.join("skills/linked")).unwrap();
        assert!(!inspect().iter().find(|item| item.name == "linked").unwrap().bundled);

        let plan = plan_install(&root, &project, &source, "managed").unwrap();
        apply_plan(&plan).unwrap();
        let managed_template = app_templates.join("managed");
        fs::create_dir_all(&managed_template).unwrap();
        fs::copy(plan.destination.join("SKILL.md"), managed_template.join("SKILL.md")).unwrap();
        let scan = inspect();
        let managed = scan.iter().find(|item| item.name == "managed").unwrap();
        assert!(managed.removable);
        assert!(!managed.bundled);
    }

    #[test]
    fn business_project_resolves_only_registered_native_destination() {
        let (temp, root, native, source) = fixture();
        let business = temp.path().join("business");
        fs::create_dir_all(business.join("skills/example")).unwrap();
        fs::write(business.join("skills/example/SKILL.md"), "business data").unwrap();
        let original = workspace_for_path(&root, &native).unwrap();
        fs::write(root.join("agent-workspaces.json"), serde_json::to_vec(&serde_json::json!({
            "version": 2, "workspaces": [{"id": "workspace-1", "slug": "pilot", "name": "Pilot",
                "projectRootPath": business}]
        })).unwrap()).unwrap();
        let resolved = workspace_for_project(&root, &business).unwrap();
        assert_eq!(resolved.path, native.canonicalize().unwrap());
        assert_eq!(resolved.project_root_path, Some(business.canonicalize().unwrap()));
        assert_eq!(resolved, original, "Finder binding cannot invalidate existing transaction identity");
        let plan = plan_install(&root, &resolved.path, &source, "example").unwrap();
        apply_plan(&plan).unwrap();
        assert!(native.join("skills/example/.kitter-proma.json").is_file());
        assert_eq!(fs::read_to_string(business.join("skills/example/SKILL.md")).unwrap(), "business data");
        assert!(plan_install(&root, &business, &source, "example").is_err());
    }

    #[test]
    fn multiple_associated_workspaces_require_explicit_mutation_target() {
        let (temp, root, native, _source) = fixture();
        let business = temp.path().join("business");
        let other = root.join("agent-workspaces/other");
        fs::create_dir_all(&business).unwrap();
        fs::create_dir_all(&other).unwrap();
        fs::write(root.join("agent-workspaces.json"), serde_json::to_vec(&serde_json::json!({
            "version": 2, "workspaces": [
                {"id": "workspace-1", "slug": "pilot", "name": "Pilot", "projectRootPath": business},
                {"id": "workspace-2", "slug": "other", "name": "Other", "projectRootPath": business}
            ]
        })).unwrap()).unwrap();
        assert_eq!(workspaces_for_project(&root, &business).unwrap().len(), 2);
        assert!(workspace_for_project(&root, &business).is_err());
        assert_eq!(workspace_for_project(&root, &native).unwrap().path, native.canonicalize().unwrap());
        assert_eq!(workspace_for_project(&root, &other).unwrap().path, other.canonicalize().unwrap());
        assert!(workspaces_for_project(&root, temp.path()).unwrap().is_empty());
    }
    #[test]
    fn physical_entry_reuses_data_without_copying_credentials_and_updates_safely() {
        let (_t, root, project, source) = fixture();
        let plan = plan_install(&root, &project, &source, "example").unwrap();
        assert_eq!(plan.action, "create");
        assert!(!project.join("skills/example").exists());
        apply_plan(&plan).unwrap();
        let target = project.join("skills/example");
        assert!(
            !fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_link(target.join("local-data")).unwrap(),
            source.canonicalize().unwrap().join("local-data")
        );
        assert!(
            fs::read_to_string(target.join("SKILL.md"))
                .unwrap()
                .contains("ADAPTATION.md")
        );
        let m = update_protection::snapshot(&target).unwrap();
        assert!(!m.keys().any(|p| p.contains("private.env")));
        assert_eq!(
            fs::read_to_string(source.join("local-data/private.env")).unwrap(),
            "fixture-secret"
        );
        assert_eq!(
            plan_install(&root, &project, &source, "example")
                .unwrap()
                .action,
            "unchanged"
        );
        fs::write(source.join("new.txt"), "new version").unwrap();
        let update = plan_install(&root, &project, &source, "example").unwrap();
        assert_eq!(update.action, "update");
        apply_plan(&update).unwrap();
        assert!(target.join("new.txt").is_file());
        fs::write(target.join("new.txt"), "local edit").unwrap();
        assert!(plan_install(&root, &project, &source, "example").is_err());
        assert_eq!(inspect_in(&root, &project).unwrap()[0].status, "modified");
    }
    #[test]
    fn forged_plan_changed_identity_and_destination_are_rejected() {
        let (_t, root, project, source) = fixture();
        let mut p = plan_install(&root, &project, &source, "example").unwrap();
        p.destination = source.join("overwrite");
        assert!(apply_plan(&p).is_err());
        let p = plan_install(&root, &project, &source, "example").unwrap();
        fs::write(
            root.join("agent-workspaces.json"),
            r#"{"version":1,"workspaces":[{"id":"changed","slug":"pilot","name":"Pilot"}]}"#,
        )
        .unwrap();
        assert!(apply_plan(&p).is_err());
        assert!(!project.join("skills/example").exists());
    }
    #[test]
    fn source_and_target_changes_after_preview_are_rejected() {
        let (_t, root, project, source) = fixture();
        let p = plan_install(&root, &project, &source, "example").unwrap();
        fs::write(source.join("changed.txt"), "changed").unwrap();
        assert!(apply_plan(&p).is_err());
        let p = plan_install(&root, &project, &source, "example").unwrap();
        fs::create_dir_all(&p.destination).unwrap();
        fs::write(p.destination.join("unknown"), "keep").unwrap();
        assert!(apply_plan(&p).is_err());
        assert!(p.destination.join("unknown").is_file());
    }
    #[test]
    fn discovery_does_not_initialize_and_invalid_index_does_not_echo_content() {
        let t = tempfile::tempdir().unwrap();
        assert!(workspaces(&t.path().join("missing")).unwrap().is_empty());
        assert!(!t.path().join("missing").exists());
        fs::write(t.path().join("agent-workspaces.json"), "fixture-secret").unwrap();
        assert!(
            !workspaces(t.path())
                .unwrap_err()
                .to_string()
                .contains("fixture-secret")
        );
    }
    #[cfg(unix)]
    #[test]
    fn exact_legacy_link_is_preserved_in_recovery_and_data_reference_changes_block_updates() {
        let (_t, root, project, source) = fixture();
        let target = project.join("skills/example");
        std::os::unix::fs::symlink(&source, &target).unwrap();
        let p = plan_install(&root, &project, &source, "example").unwrap();
        assert_eq!(p.action, "migrate");
        apply_plan(&p).unwrap();
        let recovered = fs::read_dir(project.join(RECOVERY))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("previous");
        assert_eq!(fs::read_link(recovered).unwrap(), source);
        fs::remove_file(target.join("local-data")).unwrap();
        fs::create_dir(target.join("local-data")).unwrap();
        assert!(plan_install(&root, &project, &source, "example").is_err());
    }

    #[test]
    fn link_removal_preserves_targets_and_recovers_absolute_relative_and_broken_links() {
        for directory in ["skills", "skills-inactive"] {
            for relative in [false, true] {
                for broken in [false, true] {
                    let (_temp, root, project, source) = fixture();
                    let path = project.join(directory).join("example");
                    fs::create_dir_all(path.parent().unwrap()).unwrap();
                    let target = if broken { source.with_file_name("missing") } else { source.clone() };
                    let raw = if relative {
                        Path::new("../../../../library").join(target.file_name().unwrap())
                    } else { target };
                    std::os::unix::fs::symlink(&raw, &path).unwrap();
                    let source_before = update_protection::snapshot(&source).unwrap();
                    let scan = inspect_in(&root, &project).unwrap();
                    assert_eq!(scan[0].status, if broken { "broken_link" } else { "external_link" });
                    assert!(scan[0].removable && !scan[0].managed && !scan[0].can_toggle());
                    let plan = plan_remove_link(&root, &project, &path).unwrap();
                    assert_eq!(plan.raw_target, raw);
                    assert!(!project.join(RECOVERY).exists(), "planning must be read-only");
                    apply_remove_link(&plan).unwrap();
                    assert!(fs::symlink_metadata(&path).is_err());
                    let operation = fs::read_dir(project.join(RECOVERY)).unwrap().next().unwrap().unwrap().path();
                    assert_eq!(fs::read_link(operation.join("previous")).unwrap(), raw);
                    let mut transaction: Transaction = read_json(&operation.join("transaction.json")).unwrap();
                    transaction.phase = "replacing".into();
                    fs::write(operation.join("transaction.json"), serde_json::to_vec(&transaction).unwrap()).unwrap();
                    recover_in(&plan.workspace).unwrap();
                    assert_eq!(fs::read_link(&path).unwrap(), raw);
                    assert_eq!(update_protection::snapshot(&source).unwrap(), source_before);
                }
            }
        }
    }

    #[test]
    fn link_removal_freezes_identity_parents_and_public_plan_fields() {
        for change in ["link", "parent", "workspace", "registry", "path", "target", "lock"] {
            let (_temp, root, project, source) = fixture();
            let path = project.join("skills/example");
            std::os::unix::fs::symlink(&source, &path).unwrap();
            let mut plan = plan_remove_link(&root, &project, &path).unwrap();
            let native = Directory::open_anchored(&project).unwrap();
            let lock = (change == "lock").then(|| native.lock(Path::new(".kitter-proma.lock")).unwrap());
            match change {
                "link" => {
                    fs::rename(&path, project.join("saved-link")).unwrap();
                    std::os::unix::fs::symlink(&source, &path).unwrap();
                }
                "parent" => {
                    fs::rename(path.parent().unwrap(), project.join("saved-skills")).unwrap();
                    fs::create_dir(path.parent().unwrap()).unwrap();
                    fs::rename(project.join("saved-skills/example"), &path).unwrap();
                }
                "workspace" => {
                    fs::rename(&project, project.with_file_name("saved-workspace")).unwrap();
                    fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::os::unix::fs::symlink(&source, &path).unwrap();
                }
                "registry" => fs::write(root.join("agent-workspaces.json"),
                    r#"{"version":1,"workspaces":[{"id":"changed","slug":"pilot","name":"Pilot"}]}"#).unwrap(),
                "path" => plan.path = project.join("skills-inactive/example"),
                "target" => plan.raw_target = source.with_file_name("other"),
                "lock" => {},
                _ => unreachable!(),
            }
            let before = observed(&path).unwrap();
            assert!(apply_remove_link(&plan).is_err(), "{change}");
            assert_eq!(observed(&path).unwrap(), before, "{change}");
            assert!(source.join("SKILL.md").is_file());
            assert!(!project.join(RECOVERY).exists());
            drop(lock);
        }
    }

    #[test]
    fn link_removal_rejects_non_native_nested_linked_parents_and_real_packages() {
        let (temp, root, project, source) = fixture();
        let business = temp.path().join("business");
        let unregistered = root.join("agent-workspaces/unregistered");
        for parent in [business.join("skills"), unregistered.join("skills"), project.join("other"), project.join("skills/nested")] {
            fs::create_dir_all(&parent).unwrap();
            let path = parent.join("example");
            std::os::unix::fs::symlink(&source, &path).unwrap();
            assert!(plan_remove_link(&root, &project, &path).is_err());
        }
        assert!(plan_remove_link(&root, &unregistered, &unregistered.join("skills/example")).is_err());
        assert!(plan_remove_link(&root, &project, &source).is_err());
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&project, &alias).unwrap();
        assert!(plan_remove_link(&root, &alias, &alias.join("skills/nested/example")).is_err());
        let linked_parent = project.join("skills-inactive");
        std::os::unix::fs::symlink(business.join("skills"), &linked_parent).unwrap();
        assert!(plan_remove_link(&root, &project, &linked_parent.join("example")).is_err());
        fs::create_dir(project.join("skills/entity")).unwrap();
        assert!(plan_remove_link(&root, &project, &project.join("skills/entity")).is_err());
    }

    #[test]
    fn link_removal_restores_a_link_replaced_in_the_last_move_gap() {
        let (_temp, root, project, source) = fixture();
        let path = project.join("skills/example");
        std::os::unix::fs::symlink(&source, &path).unwrap();
        let plan = plan_remove_link(&root, &project, &path).unwrap();
        let changed = path.clone();
        let saved = project.join("saved-link");
        let other = source.with_file_name("other");
        let expected = other.clone();
        BEFORE_REMOVE_MOVE.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            fs::rename(&changed, saved).unwrap();
            std::os::unix::fs::symlink(other, changed).unwrap();
        })));
        assert!(apply_remove_link(&plan).is_err());
        assert_eq!(fs::read_link(&path).unwrap(), expected);
        assert!(source.join("SKILL.md").is_file());
        no_pending(&plan.workspace).unwrap();
    }

    fn inactive_fixture(linked: bool) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, PathBuf) {
        let (temp, root, project, source) = fixture();
        let inactive = project.join("skills-inactive/example");
        fs::create_dir(inactive.parent().unwrap()).unwrap();
        if linked {
            std::os::unix::fs::symlink("../../../../library/example", &inactive).unwrap();
        } else {
            apply_plan(&plan_install(&root, &project, &source, "example").unwrap()).unwrap();
            fs::rename(project.join("skills/example"), &inactive).unwrap();
        }
        (temp, root, project, source, inactive)
    }

    #[test]
    fn inactive_links_migrate_and_old_managed_copies_reinstall_under_one_lock() {
        for linked in [false, true] {
            let (_temp, root, project, source, inactive) = inactive_fixture(linked);
            let source_before = update_protection::snapshot(&source).unwrap();
            let before = observed(&inactive).unwrap();
            let plan = plan_install(&root, &project, &source, "example").unwrap();
            assert_eq!(plan.action, if linked { "migrate" } else { "reinstall" });
            assert_eq!(plan.original_path(), inactive);
            assert_eq!(plan.destination, project.join("skills/example"));
            let workspace = Directory::open_anchored(&project).unwrap();
            let lock = workspace.lock(Path::new(".kitter-proma.lock")).unwrap();
            assert!(apply_plan(&plan).is_err());
            assert_eq!(observed(&inactive).unwrap(), before);
            drop(lock);
            apply_plan(&plan).unwrap();
            assert!(fs::symlink_metadata(&inactive).is_err());
            assert!(!fs::symlink_metadata(&plan.destination).unwrap().file_type().is_symlink());
            clean_marker(&plan.destination, &plan.workspace).unwrap();
            let transaction = fs::read_dir(project.join(RECOVERY)).unwrap().map(|entry| {
                let path = entry.unwrap().path();
                read_json::<Transaction>(&path.join("transaction.json")).unwrap()
            }).find(|t| t.original.is_some()).unwrap();
            assert_eq!(transaction.original.as_ref(), Some(&inactive));
            assert_eq!(observed(transaction.backup.as_ref().unwrap()).unwrap(), before);
            assert!(!inspect_in(&root, &project).unwrap()[0].can_toggle());
            assert_eq!(update_protection::snapshot(&source).unwrap(), source_before);
        }
    }

    #[test]
    fn inactive_reinstall_preserves_concurrent_active_occupants_and_restores_original() {
        for linked in [false, true] {
            for file in [false, true] {
                let (_temp, root, project, source, inactive) = inactive_fixture(linked);
                let before = observed(&inactive).unwrap();
                let plan = plan_install(&root, &project, &source, "example").unwrap();
                let winner = plan.destination.clone();
                BEFORE_PUBLISH.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
                    if file {
                        fs::write(&winner, "concurrent winner").unwrap();
                    } else {
                        fs::create_dir(&winner).unwrap();
                        fs::write(winner.join("keep"), "concurrent winner").unwrap();
                    }
                })));
                assert!(apply_plan(&plan).is_err());
                assert_eq!(observed(&inactive).unwrap(), before);
                let winner = if file { plan.destination.clone() } else { plan.destination.join("keep") };
                assert_eq!(fs::read_to_string(winner).unwrap(), "concurrent winner");
                no_pending(&plan.workspace).unwrap();
            }
        }
    }

    #[test]
    fn inactive_reinstall_rejects_modified_foreign_duplicate_or_changed_entries() {
        for change in ["modified", "foreign", "duplicate", "replaced", "parent", "source"] {
            let (_temp, root, project, source, inactive) = inactive_fixture(false);
            let source_before = update_protection::snapshot(&source).unwrap();
            let plan = plan_install(&root, &project, &source, "example").unwrap();
            match change {
                "modified" => {
                    fs::write(inactive.join("extra"), "local edit").unwrap();
                    assert!(plan_install(&root, &project, &source, "example").is_err());
                }
                "foreign" => {
                    let foreign = source.with_file_name("foreign");
                    fs::create_dir(&foreign).unwrap();
                    fs::write(foreign.join("SKILL.md"), "foreign source").unwrap();
                    assert!(plan_install(&root, &project, &foreign, "example").is_err());
                    continue;
                }
                "duplicate" => fs::create_dir(&plan.destination).unwrap(),
                "replaced" => {
                    fs::rename(&inactive, project.join("saved-original")).unwrap();
                    std::os::unix::fs::symlink(&source, &inactive).unwrap();
                }
                "parent" => {
                    fs::rename(inactive.parent().unwrap(), project.join("saved-inactive")).unwrap();
                    fs::create_dir(inactive.parent().unwrap()).unwrap();
                    fs::rename(project.join("saved-inactive/example"), &inactive).unwrap();
                }
                "source" => fs::write(source.join("SKILL.md"), "changed upstream").unwrap(),
                _ => unreachable!(),
            }
            let before = observed(&inactive).unwrap();
            assert!(apply_plan(&plan).is_err(), "{change}");
            assert_eq!(observed(&inactive).unwrap(), before);
            if change != "source" {
                assert_eq!(update_protection::snapshot(&source).unwrap(), source_before);
            }
        }
    }

    #[test]
    fn cross_directory_recovery_restores_original_inactive_position_at_each_gap() {
        for linked in [false, true] {
            for phase in ["prepared", "backed_up", "published", "active_winner", "active_file", "inactive_winner", "invalid_original"] {
                let (_temp, root, project, source, inactive) = inactive_fixture(linked);
                let plan = plan_install(&root, &project, &source, "example").unwrap();
                let source_before = update_protection::snapshot(&source).unwrap();
                let operation = project.join(RECOVERY).join("interrupted-cross-directory");
                fs::create_dir_all(operation.join("staging")).unwrap();
                copy_package(&plan, &Directory::open_anchored(&operation.join("staging")).unwrap()).unwrap();
                let mut transaction = Transaction {
                    workspace: plan.workspace.clone(), workspace_signature: plan.workspace_signature.clone(),
                    target: plan.destination.clone(), original: Some(inactive.clone()),
                    backup: Some(operation.join("previous")), before: observed(&inactive).unwrap(),
                    after: observed(&operation.join("staging")).unwrap(), phase: "replacing".into(),
                };
                if phase != "prepared" {
                    package_fs::rename_exclusive(&inactive, transaction.backup.as_ref().unwrap()).unwrap();
                }
                if matches!(phase, "published" | "inactive_winner") {
                    package_fs::rename_exclusive(&operation.join("staging"), &plan.destination).unwrap();
                }
                if phase == "active_winner" {
                    fs::create_dir(&plan.destination).unwrap();
                    fs::write(plan.destination.join("keep"), "active winner").unwrap();
                }
                if phase == "active_file" {
                    fs::write(&plan.destination, "active file").unwrap();
                }
                if phase == "inactive_winner" {
                    fs::create_dir(&inactive).unwrap();
                    fs::write(inactive.join("keep"), "inactive winner").unwrap();
                }
                if phase == "invalid_original" { transaction.original = Some(source.clone()); }
                fs::write(operation.join("transaction.json"), serde_json::to_vec(&transaction).unwrap()).unwrap();
                if phase == "invalid_original" {
                    assert!(recover_in(&plan.workspace).is_err());
                    assert_eq!(observed(transaction.backup.as_ref().unwrap()).unwrap(), transaction.before);
                    assert!(fs::symlink_metadata(&inactive).is_err());
                } else if phase == "inactive_winner" {
                    assert!(recover_in(&plan.workspace).is_err());
                    assert_eq!(fs::read_to_string(inactive.join("keep")).unwrap(), "inactive winner");
                    assert_eq!(observed(transaction.backup.as_ref().unwrap()).unwrap(), transaction.before);
                    assert_eq!(observed(&plan.destination).unwrap(), transaction.after);
                } else {
                    recover_in(&plan.workspace).unwrap();
                    assert_eq!(observed(&inactive).unwrap(), transaction.before, "{phase}");
                    if phase == "active_winner" {
                        assert_eq!(fs::read_to_string(plan.destination.join("keep")).unwrap(), "active winner");
                    } else if phase == "active_file" {
                        assert_eq!(fs::read_to_string(&plan.destination).unwrap(), "active file");
                    } else { assert!(fs::symlink_metadata(&plan.destination).is_err()); }
                    transaction = read_json(&operation.join("transaction.json")).unwrap();
                    assert_eq!(transaction.phase, "rolled_back");
                }
                assert_eq!(update_protection::snapshot(&source).unwrap(), source_before);
            }
        }
    }
    #[test]
    fn backup_gap_can_be_restored_without_touching_unknown_target_content() {
        let (_t, root, project, source) = fixture();
        let p = plan_install(&root, &project, &source, "example").unwrap();
        apply_plan(&p).unwrap();
        let target = p.destination;
        let before = observed(&target).unwrap();
        let op = project.join(RECOVERY).join("interrupted");
        fs::create_dir(&op).unwrap();
        let backup = op.join("previous");
        package_fs::rename_exclusive(&target, &backup).unwrap();
        let t = Transaction {
            workspace: p.workspace,
            workspace_signature: p.workspace_signature,
            target: target.clone(),
            original: None,
            backup: Some(backup.clone()),
            before,
            after: "unused".into(),
            phase: "replacing".into(),
        };
        let mut legacy = serde_json::to_value(&t).unwrap();
        legacy.as_object_mut().unwrap().remove("original");
        fs::write(op.join("transaction.json"), serde_json::to_vec(&legacy).unwrap()).unwrap();
        recover_in(&t.workspace).unwrap();
        restore_transaction(&t, &op).unwrap();
        assert!(target.join("SKILL.md").is_file());
        package_fs::rename_exclusive(&target, &backup).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(target.join("unknown"), "keep").unwrap();
        assert!(restore_transaction(&t, &op).is_err());
        assert!(target.join("unknown").is_file());
        assert!(backup.join("SKILL.md").is_file());
    }
    #[test]
    fn edit_in_last_publish_gap_is_restored_at_original_location() {
        let (_t, root, project, source) = fixture();
        apply_plan(&plan_install(&root, &project, &source, "example").unwrap()).unwrap();
        fs::write(source.join("new.txt"), "new version").unwrap();
        let p = plan_install(&root, &project, &source, "example").unwrap();
        let target = p.destination.clone();
        BEFORE_PUBLISH.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                fs::write(target.join("SKILL.md"), "concurrent edit").unwrap();
            }))
        });
        assert!(apply_plan(&p).is_err());
        assert_eq!(
            fs::read_to_string(p.destination.join("SKILL.md")).unwrap(),
            "concurrent edit"
        );
        assert!(!p.destination.join("new.txt").exists());
    }

    #[test]
    fn same_content_concurrent_winner_is_not_moved_by_failed_create() {
        let (_t, root, project, source) = fixture();
        let p = plan_install(&root, &project, &source, "example").unwrap();
        let target = p.destination.clone();
        let native = p.workspace.path.clone();
        let data = p.local_data.clone().unwrap();
        BEFORE_PUBLISH.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                let staging = fs::read_dir(native.join(RECOVERY))
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path()
                    .join("staging");
                crate::package_fs::copy_tree(&staging, &target).unwrap();
                std::os::unix::fs::symlink(data, target.join("local-data")).unwrap();
            }))
        });
        assert!(apply_plan(&p).is_err());
        assert!(p.destination.join("SKILL.md").is_file());
        assert!(clean_marker(&p.destination, &p.workspace).is_ok());
    }

    #[test]
    fn recovery_parent_link_substitution_never_writes_to_outside_directory() {
        let (temp, root, project, source) = fixture();
        let p = plan_install(&root, &project, &source, "example").unwrap();
        let external = temp.path().join("external");
        fs::create_dir(&external).unwrap();
        let recovery = p.workspace.path.join(RECOVERY);
        let moved = p.workspace.path.join("old-recovery");
        let outside = external.clone();
        BEFORE_STAGING.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                fs::rename(&recovery, moved).unwrap();
                std::os::unix::fs::symlink(outside, recovery).unwrap();
            }))
        });
        assert!(apply_plan(&p).is_err());
        assert_eq!(fs::read_dir(external).unwrap().count(), 0);
        assert!(!p.destination.exists());
    }

    #[test]
    fn private_directory_case_alias_is_never_copied() {
        let (_t, root, project, source) = fixture();
        fs::rename(source.join("local-data"), source.join("LOCAL-DATA")).unwrap();
        let p = plan_install(&root, &project, &source, "example").unwrap();
        apply_plan(&p).unwrap();
        assert!(
            !Directory::open_anchored(&p.destination)
                .unwrap()
                .installation_snapshot()
                .unwrap()
                .keys()
                .any(|p| p.contains("private.env"))
        );
        for entry in fs::read_dir(p.workspace.path.join(RECOVERY)).unwrap() {
            assert!(
                !Directory::open_anchored(&entry.unwrap().path())
                    .unwrap()
                    .installation_snapshot()
                    .unwrap()
                    .keys()
                    .any(|p| p.contains("private.env"))
            );
        }
    }

    #[test]
    fn corrupted_backup_blocks_recovery_before_moving_current_target() {
        let (_t, root, project, source) = fixture();
        let p = plan_install(&root, &project, &source, "example").unwrap();
        apply_plan(&p).unwrap();
        let operation = p.workspace.path.join(RECOVERY).join("corrupt-backup");
        fs::create_dir(&operation).unwrap();
        let backup = operation.join("previous");
        crate::package_fs::copy_tree(&p.destination, &backup).unwrap();
        let before = observed(&backup).unwrap();
        let after = observed(&p.destination).unwrap();
        fs::write(backup.join("SKILL.md"), "modified backup").unwrap();
        let t = Transaction {
            workspace: p.workspace,
            workspace_signature: p.workspace_signature,
            target: p.destination.clone(),
            original: None,
            backup: Some(backup),
            before,
            after,
            phase: "replacing".into(),
        };
        assert!(restore_transaction(&t, &operation).is_err());
        assert!(p.destination.join("SKILL.md").is_file());
        assert!(!operation.join("failed").exists());
    }
    #[test]
    fn committed_source_refresh_only_updates_existing_clean_active_followers() {
        let (_t, root, project, source) = fixture();
        apply_plan(&plan_install(&root, &project, &source, "example").unwrap()).unwrap();
        fs::write(source.join("version.txt"), "v2").unwrap();
        assert!(sync_existing_source_in(&root, &source).is_empty());
        let target = project.join("skills/example");
        assert_eq!(
            fs::read_to_string(target.join("version.txt")).unwrap(),
            "v2"
        );
        fs::write(target.join("version.txt"), "local edit").unwrap();
        fs::write(source.join("version.txt"), "v3").unwrap();
        assert_eq!(sync_existing_source_in(&root, &source).len(), 1);
        assert_eq!(
            fs::read_to_string(target.join("version.txt")).unwrap(),
            "local edit"
        );
        fs::create_dir(project.join("skills-inactive")).unwrap();
        fs::rename(&target, project.join("skills-inactive/example")).unwrap();
        assert!(sync_existing_source_in(&root, &source).is_empty());
        assert!(!target.exists());
    }

    #[test]
    fn local_auto_sync_debounces_edits_and_updates_inactive_without_enabling() {
        let (_t, root, project, source) = fixture();
        apply_plan(&plan_install(&root, &project, &source, "example").unwrap()).unwrap();
        let second = root.join("agent-workspaces/second");
        let empty = root.join("agent-workspaces/empty");
        fs::create_dir_all(second.join("skills")).unwrap();
        fs::create_dir_all(&empty).unwrap();
        fs::write(root.join("agent-workspaces.json"), serde_json::to_vec(&serde_json::json!({
            "version":1,"workspaces":[
                {"id":"workspace-1","slug":"pilot","name":"Pilot"},
                {"id":"workspace-2","slug":"second","name":"Second"},
                {"id":"workspace-3","slug":"empty","name":"Empty"}
            ]
        })).unwrap()).unwrap();
        apply_plan(&plan_install(&root, &second, &source, "example").unwrap()).unwrap();
        fs::create_dir(second.join("skills-inactive")).unwrap();
        fs::rename(second.join("skills/example"), second.join("skills-inactive/example")).unwrap();
        let sources = BTreeSet::from([source.clone()]);
        let mut sync = LocalMirrorSync::default();
        assert_eq!(sync.poll(&root, &sources).unwrap().updated, 0);
        fs::write(source.join("version.txt"), "v2").unwrap();
        assert_eq!(sync.poll(&root, &sources).unwrap().updated, 0);
        fs::write(source.join("version.txt"), "v3").unwrap();
        assert_eq!(sync.poll(&root, &sources).unwrap().updated, 0);
        let report = sync.poll(&root, &sources).unwrap();
        assert!(report.warnings.is_empty());
        assert_eq!((report.updated, report.inactive_updated), (2, 1));
        for path in [project.join("skills/example"), second.join("skills-inactive/example")] {
            assert_eq!(fs::read_to_string(path.join("version.txt")).unwrap(), "v3");
        }
        assert!(!second.join("skills/example").exists());
        assert!(!empty.join("skills").exists());
        assert_eq!(sync.poll(&root, &sources).unwrap().updated, 0);
    }

    #[test]
    fn local_auto_sync_preserves_modified_unmanaged_and_unregistered_copies() {
        let (_t, root, project, source) = fixture();
        apply_plan(&plan_install(&root, &project, &source, "example").unwrap()).unwrap();
        apply_plan(&plan_install(&root, &project, &source, "disabled").unwrap()).unwrap();
        fs::create_dir(project.join("skills-inactive")).unwrap();
        fs::rename(project.join("skills/disabled"), project.join("skills-inactive/disabled")).unwrap();
        for path in [project.join("skills/example"), project.join("skills-inactive/disabled")] {
            fs::write(path.join("version.txt"), "workspace edit").unwrap();
        }
        let unmanaged = project.join("skills/unmanaged");
        fs::create_dir(&unmanaged).unwrap();
        fs::write(unmanaged.join("SKILL.md"), "unmanaged content").unwrap();
        let other = source.parent().unwrap().join("other");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("SKILL.md"), "other content").unwrap();
        apply_plan(&plan_install(&root, &project, &other, "other").unwrap()).unwrap();
        let sources = BTreeSet::from([source.clone()]);
        let mut sync = LocalMirrorSync::default();
        sync.poll(&root, &sources).unwrap();
        fs::write(source.join("version.txt"), "source edit").unwrap();
        fs::write(other.join("SKILL.md"), "other edit").unwrap();
        sync.poll(&root, &sources).unwrap();
        let report = sync.poll(&root, &sources).unwrap();
        assert_eq!(report.updated, 0);
        assert_eq!(report.warnings.len(), 2);
        for path in [project.join("skills/example"), project.join("skills-inactive/disabled")] {
            assert_eq!(fs::read_to_string(path.join("version.txt")).unwrap(), "workspace edit");
        }
        assert_eq!(fs::read_to_string(unmanaged.join("SKILL.md")).unwrap(), "unmanaged content");
        assert_eq!(fs::read_to_string(project.join("skills/other/SKILL.md")).unwrap(), "other content");
        assert!(!project.join("skills/disabled").exists());
    }

    #[test]
    fn local_auto_sync_does_not_recreate_removed_or_replaced_copies() {
        for inactive in [false, true] {
            for replace_with_link in [false, true] {
                let (_t, root, project, source) = fixture();
                apply_plan(&plan_install(&root, &project, &source, "example").unwrap()).unwrap();
                let target = if inactive {
                    fs::create_dir(project.join("skills-inactive")).unwrap();
                    let path = project.join("skills-inactive/example");
                    fs::rename(project.join("skills/example"), &path).unwrap();
                    path
                } else { project.join("skills/example") };
                let sources = BTreeSet::from([source.clone()]);
                let mut sync = LocalMirrorSync::default();
                fs::write(source.join("version.txt"), "v2").unwrap();
                sync.poll(&root, &sources).unwrap();
                let saved = project.join("removed");
                let changed_target = target.clone();
                let origin = source.clone();
                BEFORE_FOLLOWER_REFRESH.with(|hook| {
                    *hook.borrow_mut() = Some(Box::new(move || {
                        fs::rename(&changed_target, saved).unwrap();
                        if replace_with_link { std::os::unix::fs::symlink(origin, changed_target).unwrap(); }
                    }));
                });
                let report = sync.poll(&root, &sources).unwrap();
                assert_eq!((report.updated, report.warnings.len()), (0, 1));
                if replace_with_link {
                    assert!(fs::symlink_metadata(&target).unwrap().file_type().is_symlink());
                } else { assert!(!target.exists()); }
            }
        }
    }

    #[test]
    fn local_auto_sync_rechecks_source_stability_before_publish() {
        let (_t, root, project, source) = fixture();
        apply_plan(&plan_install(&root, &project, &source, "example").unwrap()).unwrap();
        fs::write(source.join("version.txt"), "v2").unwrap();
        let sources = BTreeSet::from([source.clone()]);
        let mut sync = LocalMirrorSync::default();
        sync.poll(&root, &sources).unwrap();
        let changed_source = source.clone();
        BEFORE_FOLLOWER_REFRESH.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || { fs::write(changed_source.join("version.txt"), "v3").unwrap(); }));
        });
        let report = sync.poll(&root, &sources).unwrap();
        assert_eq!((report.updated, report.warnings.len()), (0, 1));
        assert!(!project.join("skills/example/version.txt").exists());
        assert_eq!(sync.poll(&root, &sources).unwrap().updated, 0);
        assert_eq!(sync.poll(&root, &sources).unwrap().updated, 1);
        assert_eq!(fs::read_to_string(project.join("skills/example/version.txt")).unwrap(), "v3");
    }

    #[test]
    fn local_auto_sync_respects_removal_during_plan_observation() {
        for inactive in [false, true] {
            let (_t, root, project, source) = fixture();
            apply_plan(&plan_install(&root, &project, &source, "example").unwrap()).unwrap();
            let directory = if inactive { "skills-inactive" } else { "skills" };
            if inactive {
                fs::create_dir(project.join(directory)).unwrap();
                fs::rename(project.join("skills/example"), project.join(directory).join("example")).unwrap();
            }
            fs::write(source.join("version.txt"), "v2").unwrap();
            let mut invalid = plan_install_at(&root, &project, &source, "example", directory, true).unwrap();
            invalid.expected = "absent".into();
            assert!(apply_plan(&invalid).is_err());
            let sources = BTreeSet::from([source.clone()]);
            let mut sync = LocalMirrorSync::default();
            sync.poll(&root, &sources).unwrap();
            let target = project.join(directory).join("example");
            let removed = project.join("removed-during-plan");
            let moved = target.clone();
            BEFORE_PLAN_OBSERVATION.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || { fs::rename(moved, removed).unwrap(); }));
            });
            let report = sync.poll(&root, &sources).unwrap();
            assert_eq!((report.updated, report.warnings.len()), (0, 1));
            assert!(!target.exists());
        }
    }
    #[test]
    fn source_refresh_does_not_recreate_a_removed_or_replaced_follower() {
        for replace_with_link in [false, true] {
            let (_t, root, project, source) = fixture();
            let p = plan_install(&root, &project, &source, "example").unwrap();
            apply_plan(&p).unwrap();
            fs::write(source.join("version.txt"), "v2").unwrap();
            let target = p.destination.clone();
            let saved = p.workspace.path.join("removed");
            let origin = p.source.clone();
            BEFORE_FOLLOWER_REFRESH.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    fs::rename(&target, saved).unwrap();
                    if replace_with_link {
                        std::os::unix::fs::symlink(origin, target).unwrap();
                    }
                }))
            });
            assert_eq!(sync_existing_source_in(&root, &source).len(), 1);
            if replace_with_link {
                assert!(
                    fs::symlink_metadata(&p.destination)
                        .unwrap()
                        .file_type()
                        .is_symlink()
                );
            } else {
                assert!(!p.destination.exists());
            }
        }
    }

    #[test]
    fn missing_managed_entry_reports_modified_and_preserves_user_deletion() {
        let (_t, root, project, source) = fixture();
        let p = plan_install(&root, &project, &source, "example").unwrap();
        apply_plan(&p).unwrap();
        fs::remove_file(p.destination.join("SKILL.md")).unwrap();
        assert_eq!(inspect_in(&root, &project).unwrap()[0].status, "modified");
        assert_eq!(sync_existing_source_in(&root, &source).len(), 1);
        assert!(!p.destination.join("SKILL.md").exists());
    }

    #[test]
    fn workspace_lock_rejects_a_second_writer_without_installing() {
        let (_t, root, project, source) = fixture();
        let p = plan_install(&root, &project, &source, "example").unwrap();
        let workspace = Directory::open_anchored(&p.workspace.path).unwrap();
        let _lock = workspace.lock(Path::new(".kitter-proma.lock")).unwrap();
        assert!(apply_plan(&p).is_err());
        assert!(!p.destination.exists());
    }
    #[test]
    fn unobservable_original_is_restored_after_last_publish_gap() {
        use std::os::unix::net::UnixListener;
        let (_t, root, project, source) = fixture();
        apply_plan(&plan_install(&root, &project, &source, "example").unwrap()).unwrap();
        fs::write(source.join("version.txt"), "v2").unwrap();
        let p = plan_install(&root, &project, &source, "example").unwrap();
        let target = p.destination.clone();
        let short = tempfile::tempdir().unwrap();
        let socket = short.path().join("s");
        let listener = UnixListener::bind(&socket).unwrap();
        BEFORE_PUBLISH.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                fs::rename(socket, target.join("concurrent.sock")).unwrap();
                drop(listener);
            }))
        });
        assert!(apply_plan(&p).is_err());
        assert!(p.destination.join("concurrent.sock").exists());
        assert!(p.destination.join("SKILL.md").is_file());
        assert!(!p.destination.join("version.txt").exists());
    }

    #[test]
    fn rollback_rechecks_the_actual_objects_moved_after_its_preflight() {
        for replace_backup in [false, true] {
            let (_t, root, project, source) = fixture();
            let p = plan_install(&root, &project, &source, "example").unwrap();
            apply_plan(&p).unwrap();
            let operation = p.workspace.path.join(RECOVERY).join("rollback-gap");
            fs::create_dir(&operation).unwrap();
            let backup = operation.join("previous");
            crate::package_fs::copy_tree(&p.destination, &backup).unwrap();
            let t = Transaction {
                workspace: p.workspace,
                workspace_signature: p.workspace_signature,
                target: p.destination.clone(),
                original: None,
                backup: Some(backup.clone()),
                before: observed(&backup).unwrap(),
                after: observed(&p.destination).unwrap(),
                phase: "replacing".into(),
            };
            let selected = if replace_backup {
                backup.clone()
            } else {
                p.destination.clone()
            };
            let moved = operation.join("concurrent-original");
            BEFORE_ROLLBACK_MOVE.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    fs::rename(&selected, moved).unwrap();
                    fs::create_dir(&selected).unwrap();
                    fs::write(selected.join("unknown"), "keep").unwrap();
                }))
            });
            assert!(restore_transaction(&t, &operation).is_err());
            if replace_backup {
                assert!(backup.join("unknown").is_file());
                assert!(p.destination.join("SKILL.md").is_file());
            } else {
                assert!(p.destination.join("unknown").is_file());
                assert!(backup.join("SKILL.md").is_file());
            }
        }
    }

    #[test]
    fn recovery_retains_operation_descriptor_and_rejects_replaced_directory() {
        let (_t, root, project, source) = fixture();
        let p = plan_install(&root, &project, &source, "example").unwrap();
        apply_plan(&p).unwrap();
        let operation = p.workspace.path.join(RECOVERY).join("install-interrupted");
        fs::create_dir(&operation).unwrap();
        let backup = operation.join("previous");
        let before = observed(&p.destination).unwrap();
        crate::package_fs::rename_exclusive(&p.destination, &backup).unwrap();
        let t = Transaction {
            workspace: p.workspace.clone(),
            workspace_signature: p.workspace_signature,
            target: p.destination.clone(),
            original: None,
            backup: Some(backup.clone()),
            before,
            after: "unpublished".into(),
            phase: "replacing".into(),
        };
        Directory::open_anchored(&operation)
            .unwrap()
            .write(
                Path::new("transaction.json"),
                &serde_json::to_vec(&t).unwrap(),
            )
            .unwrap();
        let saved = operation.with_file_name("saved-original");
        let replaced = operation.clone();
        BEFORE_RECOVERY_MUTATION.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                fs::rename(&replaced, saved).unwrap();
                fs::create_dir(&replaced).unwrap();
                fs::write(replaced.join("transaction.json"), "keep unknown record").unwrap();
            }))
        });
        assert!(recover_in(&p.workspace).is_err());
        assert_eq!(
            fs::read_to_string(operation.join("transaction.json")).unwrap(),
            "keep unknown record"
        );
        assert!(!p.destination.exists());
        assert!(
            operation
                .with_file_name("saved-original")
                .join("previous/SKILL.md")
                .is_file()
        );
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn toggle_rejects_unsupported_writes_without_changing_directories() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("proma");
        let project = root.join("agent-workspaces/pilot");
        let active = project.join("skills/example");
        fs::create_dir_all(&active).unwrap();
        fs::write(root.join("agent-workspaces.json"),
            r#"{"version":1,"workspaces":[{"id":"workspace-1","slug":"pilot","name":"Pilot"}]}"#).unwrap();
        fs::write(active.join("SKILL.md"), "builtin fixture").unwrap();
        let fingerprint = Sha256::digest(b"builtin fixture").into();
        let bundles = BTreeMap::from([("example".into(), BTreeSet::from([fingerprint]))]);
        assert!(set_enabled_path_with_bundles(&root, &active, false, &bundles).is_err());
        assert_eq!(fs::read_to_string(active.join("SKILL.md")).unwrap(), "builtin fixture");
        assert!(!project.join("skills-inactive").exists());
        assert!(!project.join(".kitter-proma.lock").exists());
    }
}
