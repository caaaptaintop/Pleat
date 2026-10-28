use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};

use crate::{
    InstallTarget, ProjectSkill, ProjectSkillInstallation,
    agents::{PROJECT_INSTALL_TARGETS, installation_root},
    directory_link,
};

#[derive(Clone, Debug)]
pub enum RemovalKind {
    ManagedInstallation,
    ExternalLink { source: PathBuf },
    SourceFiles,
}

#[derive(Default)]
pub struct RemovalReport {
    pub removed: usize,
    pub failures: Vec<String>,
}

#[derive(Default)]
pub struct BatchInstallReport {
    /// Completed project/skill/target operations, including unchanged installs.
    pub installed: usize,
    pub installed_projects: BTreeSet<PathBuf>,
    pub failures: Vec<String>,
}

/// Use the existing per-target installer and report partial failures precisely.
pub fn install_batch(
    projects: &[PathBuf],
    skills: &[(String, PathBuf)],
    targets: &[InstallTarget],
) -> BatchInstallReport {
    let mut report = BatchInstallReport::default();
    let projects = projects.iter().map(|path| crate::project_discovery::path_identity(path))
        .collect::<BTreeSet<_>>();
    let mut seen = HashSet::new();
    for project in projects {
        for (name, source) in skills {
            for target in targets {
                if !seen.insert((project.clone(), name.clone(),
                    crate::project_discovery::path_identity(source), *target)) {
                    continue;
                }
                match install_from_path(&project, source, name, &[*target]) {
                    Ok(()) => {
                        report.installed += 1;
                        report.installed_projects.insert(project.clone());
                    }
                    Err(error) => report.failures.push(format!(
                        "{} / {} / {target:?}: {error}", project.display(), name,
                    )),
                }
            }
        }
    }
    report
}

pub fn install(
    project: &Path,
    library_dir: &Path,
    name: &str,
    targets: &[InstallTarget],
) -> Result<()> {
    crate::library::validate_name(name)?;
    if !project.is_dir() {
        bail!("项目文件夹不存在：{}", project.display());
    }
    let source = library_dir
        .join(name)
        .canonicalize()
        .context("技能不存在")?;
    install_from_path(project, &source, name, targets)
}

pub fn install_from_path(
    project: &Path,
    source: &Path,
    name: &str,
    targets: &[InstallTarget],
) -> Result<()> {
    crate::library::validate_name(name)?;
    if !project.is_dir() {
        bail!("项目文件夹不存在：{}", project.display());
    }
    let roots = targets.iter().filter(|target| **target != InstallTarget::Proma)
        .map(|target| installation_root(project, *target)).collect::<Result<Vec<_>>>()?;
    if targets.contains(&InstallTarget::OpenClaw) {
        crate::agents::validate_openclaw_link(&installation_root(project, InstallTarget::OpenClaw)?, source)?;
    }
    let hermes_root = if targets.contains(&InstallTarget::Hermes)
        && dirs::home_dir().is_some_and(|home| home.canonicalize().ok() != project.canonicalize().ok()) {
        let root = crate::agents::validate_hermes_project(project)?;
        anyhow::ensure!(root == crate::agents::hermes::hermes_directory_identity(&installation_root(project, InstallTarget::Hermes)?)?,
            "Hermes 项目作用域已变化，请刷新后重试");
        let git_root = crate::agents::hermes_project_root(project, &dirs::home_dir().context("无法取得 Hermes 用户目录")?)?;
        Some((root, git_root))
    } else { None };
    let mut instructions = targets.iter().filter(|target| **target != InstallTarget::Proma)
        .map(|target| crate::host_instructions::RulePlan::for_install(*target, &installation_root(project, *target)?.join(name), source))
        .collect::<Result<Vec<_>>>()?.into_iter().flatten().collect::<Vec<_>>();
    for target in targets.iter().filter(|target| **target != InstallTarget::Proma) {
        instructions.extend(crate::host_instructions::RulePlan::for_file_hosts_if_affected(
            project, &installation_root(project, *target)?.join(name), source)?);
    }
    let instructions = crate::host_instructions::RulePlan::merge_all(instructions)?;
    // Native instruction publication requires the Unix anchored transaction.
    // Reject before creating installation links on unsupported platforms.
    #[cfg(not(unix))]
    ensure!(instructions.is_empty(), "此平台尚未支持原生适配受管写入，未创建安装链接");
    if let Some((expected, git_root)) = hermes_root {
        anyhow::ensure!(crate::agents::validate_hermes_project(project)? == expected
            && crate::agents::hermes_project_root(project, &dirs::home_dir().context("无法取得 Hermes 用户目录")?)? == git_root,
            "Hermes 信任或项目作用域已变化，未创建安装链接");
    }
    if targets.contains(&InstallTarget::Proma) {
        // The mirror API validates the registered native workspace before writing.
        let workspace = crate::proma::installation_workspace(project)?;
        crate::proma::install(&workspace.path, source, name)?;
    }
    let created = install_to_roots(
        project,
        source,
        name,
        roots,
    )?;
    #[cfg(test)]
    AFTER_LINKS_INSTALL.with(|hook| { if let Some(callback) = hook.borrow_mut().take() { callback(); } });
    let mut created_instructions = Vec::new();
    for mut instruction in instructions {
        let result = (|| -> Result<bool> {
            if !created.links.is_empty() {
                ensure!(crate::library::filesystem_identity(&created.project)? == created.project_identity,
                    "项目目录身份已变化，未写入原生适配入口");
            }
            for link in &created.links {
                ensure!(crate::library::filesystem_identity(&link.path)? == link.identity
                    && directory_link::inspect(&link.path)?.as_ref() == Some(&link.expected),
                    "刚创建的安装入口身份已变化，已保留");
            }
            instruction.install()
        })();
        match result {
            Ok(true) => created_instructions.push(instruction),
            Ok(false) => {},
            Err(error) => {
                let mut failures = Vec::new();
                // Bound hooks validate their installation while detaching.
                // Unwind instructions before their newly created links.
                for mut rule in created_instructions {
                    if let Err(error) = rule.rollback_install() {
                        failures.push(format!("{}：{error}", rule.path.display()));
                    }
                }
                failures.extend(rollback_links(&created));
                bail!("安装原生适配入口失败：{error}{}", if failures.is_empty() { String::new() } else { format!("；回滚失败：{}", failures.join("；")) });
            }
        }
    }
    Ok(())
}

struct CreatedLink {
    path: PathBuf,
    expected: directory_link::DirectoryLink,
    raw_target: Option<PathBuf>,
    identity: String,
}

struct CreatedLinks {
    project: PathBuf,
    project_identity: String,
    links: Vec<CreatedLink>,
}

#[cfg(test)]
thread_local! {
    static AFTER_LINKS_INSTALL: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

fn install_to_roots(
    project: &Path,
    source: &Path,
    name: &str,
    roots: impl IntoIterator<Item = PathBuf>,
) -> Result<CreatedLinks> {
    crate::library::validate_name(name)?;
    let project = project.canonicalize().context("项目文件夹不存在")?;
    let project_identity = crate::library::filesystem_identity(&project)?;
    let source = source.canonicalize().context("技能不存在")?;
    let mut pending = Vec::<(PathBuf, PathBuf)>::new();
    for parent in roots {
        let link = parent.join(name);
        match link.symlink_metadata() {
            Ok(_) => {
                if link.canonicalize().ok().as_ref() == Some(&source) {
                    continue;
                }
                bail!("安装位置已被占用：{}", link.display());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).context(format!("无法检查安装位置：{}", link.display()));
            }
        }
        if !pending
            .iter()
            .any(|(_, pending_link)| installation_key(pending_link) == installation_key(&link))
        {
            pending.push((parent, link));
        }
    }

    let mut created = CreatedLinks { project, project_identity, links: Vec::new() };
    for (parent, link) in pending {
        let result = (|| -> Result<CreatedLink> {
            fs::create_dir_all(&parent)?;
            // Retain the physical parent so aliases do not redirect rollback.
            let path = parent.canonicalize()?.join(name);
            directory_link::create(&source, &path)?;
            let identity = crate::library::filesystem_identity(&path)?;
            let expected = directory_link::inspect(&path)?.context("刚创建的安装链接已变化，已保留")?;
            let raw_target = match expected.kind {
                directory_link::DirectoryLinkKind::SymbolicLink => Some(fs::read_link(&path)?),
                #[cfg(windows)]
                directory_link::DirectoryLinkKind::Junction => fs::read_link(&path).ok(),
            };
            // Compare only DOS/verbatim spelling. Resolving the stored target
            // would lose migration aliases needed by rollback and recovery.
            ensure!(crate::library::storage::storage_path_spelling(&expected.target)
                    == crate::library::storage::storage_path_spelling(&source)
                && (expected.kind != directory_link::DirectoryLinkKind::SymbolicLink || raw_target.as_ref() == Some(&source))
                && crate::library::filesystem_identity(&path)? == identity,
                "刚创建的安装链接身份或目标已变化，已保留");
            Ok(CreatedLink { path, expected, raw_target, identity })
        })();
        match result {
            Ok(link) => created.links.push(link),
            Err(error) => {
                let rollback_failures = rollback_links(&created);
                let rollback = if rollback_failures.is_empty() {
                    String::new()
                } else {
                    format!("；回滚失败：{}", rollback_failures.join("；"))
                };
                bail!("创建安装链接失败：{}：{error:#}{rollback}", link.display());
            }
        }
    }
    Ok(created)
}

pub fn uninstall(
    project: &Path,
    library_dir: &Path,
    name: &str,
    targets: &[InstallTarget],
) -> Result<()> {
    crate::library::validate_name(name)?;
    let roots = targets.iter().filter(|target| **target != InstallTarget::Proma)
        .map(|target| installation_root(project, *target)).collect::<Result<Vec<_>>>()?;
    let mut instructions = targets.iter().filter(|target| **target != InstallTarget::Proma && !crate::host_instructions::RulePlan::is_file_host(**target))
        .map(|target| crate::host_instructions::RulePlan::for_target(*target, &installation_root(project, *target)?.join(name)))
        .collect::<Result<Vec<_>>>()?.into_iter().flatten().collect::<Vec<_>>();
    instructions.extend(file_host_removal_rules(project, name, targets)?);
    let instructions = crate::host_instructions::RulePlan::merge_all(instructions)?;
    let source = library_dir.join(name).canonicalize().ok();
    let mut removable = Vec::new();
    let mut removable_keys = HashSet::new();
    if targets.contains(&InstallTarget::Proma) {
        let workspace = crate::proma::installation_workspace(project)?;
        for installation in crate::proma::inspect(&workspace.path)? {
            if installation.name != name {
                continue;
            }
            let Some(source) = &source else {
                bail!("技能不存在，无法安全验证 Proma 镜像");
            };
            if !crate::proma::is_managed(&installation.path, source) {
                bail!("不会删除来源不匹配的 Proma 安装");
            }
            crate::proma::uninstall_path(&installation.path)?;
        }
    }
    for root in roots {
        let link = root.join(name);
        match link.symlink_metadata() {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).context(format!("无法检查安装位置：{}", link.display()));
            }
        }
        let Some(directory_link) = directory_link::inspect(&link)? else {
            bail!("不会删除非 Kitter 管理的目录：{}", link.display());
        };
        let Some(source) = &source else {
            bail!("技能不存在，无法安全验证安装链接：{}", link.display());
        };
        if link.canonicalize().ok().as_ref() != Some(source) {
            bail!("不会删除指向其他位置的链接：{}", link.display());
        }
        if removable_keys.insert(installation_key(&link)) {
            removable.push((link, directory_link));
        }
    }
    for mut instruction in instructions { instruction.detach()?; }
    for (link, directory_link) in removable {
        directory_link::remove(&link, directory_link)?;
    }
    Ok(())
}

pub fn uninstall_all(project: &Path, name: &str, library_dir: &Path) -> Result<()> {
    crate::library::validate_name(name)?;
    let source = library_dir.join(name).canonicalize().ok();
    uninstall_all_from_source(project, name, source.as_deref())
}

pub fn uninstall_all_from_path(project: &Path, name: &str, source: &Path) -> Result<()> {
    crate::library::validate_name(name)?;
    let source = source.canonicalize().ok();
    uninstall_all_from_source(project, name, source.as_deref())
}

fn proma_removal_paths(project: &Path, name: &str, source: Option<&Path>) -> Result<Vec<PathBuf>> {
    let workspaces = crate::proma::project_workspaces(project).unwrap_or_default();
    let mut proma_paths = Vec::new();
    for workspace in &workspaces {
        for installation in crate::proma::inspect(&workspace.path)? {
            if installation.name == name
                && source.is_some_and(|source| crate::proma::is_managed(&installation.path, source))
            {
                proma_paths.push(installation.path);
            }
        }
    }
    if workspaces.len() > 1 && !proma_paths.is_empty() {
        bail!("项目关联多个 Proma 工作区，请指定一个原生工作区目录或精确安装位置");
    }
    Ok(proma_paths)
}

pub fn validate_uninstall_all_from_path(project: &Path, name: &str, source: &Path) -> Result<()> {
    crate::library::validate_name(name)?;
    for target in PROJECT_INSTALL_TARGETS {
        if crate::host_instructions::RulePlan::is_file_host(*target) { continue; }
        let link = installation_root(project, *target)?.join(name);
        crate::host_instructions::RulePlan::for_target(*target, &link)?;
    }
    let source = source.canonicalize().ok();
    file_host_source_removal_rules(project, name, source.as_deref())?;
    proma_removal_paths(project, name, source.as_deref()).map(|_| ())
}

fn uninstall_all_from_source(project: &Path, name: &str, source: Option<&Path>) -> Result<()> {
    let roots = PROJECT_INSTALL_TARGETS.iter().map(|target| installation_root(project, *target)).collect::<Result<Vec<_>>>()?;
    let mut instructions = PROJECT_INSTALL_TARGETS.iter().filter(|target| !crate::host_instructions::RulePlan::is_file_host(**target))
        .map(|target| crate::host_instructions::RulePlan::for_target(*target, &installation_root(project, *target)?.join(name)))
        .collect::<Result<Vec<_>>>()?.into_iter().flatten().collect::<Vec<_>>();
    instructions.extend(file_host_source_removal_rules(project, name, source)?);
    for path in proma_removal_paths(project, name, source)? {
        crate::proma::uninstall_path(&path)?;
    }
    for mut instruction in instructions { instruction.detach()?; }
    for root in roots {
        let link = root.join(name);
        let Some(directory_link) = directory_link::inspect(&link)? else {
            continue;
        };
        if let Some(source) = source
            && link
                .canonicalize()
                .ok()
                .is_some_and(|resolved| resolved == source)
        {
            directory_link::remove(&link, directory_link)?;
        }
    }
    Ok(())
}

fn file_host_removal_rules(project: &Path, name: &str, targets: &[InstallTarget]) -> Result<Vec<crate::host_instructions::RulePlan>> {
    let mut plans = Vec::new();
    for target in targets.iter().filter(|target| **target != InstallTarget::Proma) {
        plans.extend(crate::host_instructions::RulePlan::file_host_removals(
            project, &installation_root(project, *target)?.join(name))?);
    }
    crate::host_instructions::RulePlan::merge_all(plans)
}

fn file_host_source_removal_rules(project: &Path, name: &str, source: Option<&Path>) -> Result<Vec<crate::host_instructions::RulePlan>> {
    let mut plans = Vec::new();
    for target in PROJECT_INSTALL_TARGETS.iter().filter(|target| **target != InstallTarget::Proma) {
        let link = installation_root(project, *target)?.join(name);
        let selected = match link.symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(error) => return Err(error.into()),
            Ok(_) => directory_link::inspect(&link)?.is_some()
                && source.is_some_and(|source| link.canonicalize().ok().as_deref() == Some(source)),
        };
        if selected {
            plans.extend(crate::host_instructions::RulePlan::file_host_removals(project, &link)?);
        }
    }
    crate::host_instructions::RulePlan::merge_all(plans)
}

pub fn removal_kind(installation: &ProjectSkillInstallation) -> RemovalKind {
    if installation.managed {
        return RemovalKind::ManagedInstallation;
    }
    let Ok(Some(directory_link)) = directory_link::inspect(&installation.path) else {
        return RemovalKind::SourceFiles;
    };
    let source = directory_link
        .target
        .canonicalize()
        .unwrap_or(directory_link.target);
    RemovalKind::ExternalLink { source }
}

fn is_supported_installation_path(installation: &ProjectSkillInstallation) -> bool {
    installation.path.parent().is_some_and(|parent| {
        crate::agents::project_target_directory(installation.target).is_ok_and(|suffix| parent.ends_with(suffix))
            || dirs::home_dir().is_some_and(|home| {
                crate::agents::global_target_root(&home, installation.target).is_ok_and(|root| parent == root)
            })
    })
}

pub fn remove_project_skill(installation: &ProjectSkillInstallation) -> Result<()> {
    remove_project_skill_in(None, installation)
}

pub fn remove_project_skill_at(project: &Path, installation: &ProjectSkillInstallation) -> Result<()> {
    remove_project_skill_in(Some(project), installation)
}

fn installation_project(installation: &ProjectSkillInstallation) -> Option<PathBuf> {
    let root = installation.path.parent()?;
    if let Some(home) = dirs::home_dir()
        && crate::agents::global_target_root(&home, installation.target).ok().as_deref() == Some(root) {
        return Some(home);
    }
    let suffix = crate::agents::project_target_directory(installation.target).ok()?;
    if !root.ends_with(&suffix) { return None; }
    let mut project = root;
    for _ in suffix.components() { project = project.parent()?; }
    Some(project.to_path_buf())
}

fn remove_project_skill_in(project: Option<&Path>, installation: &ProjectSkillInstallation) -> Result<()> {
    if installation.target == InstallTarget::Proma {
        return crate::proma::uninstall_path(&installation.path);
    }
    if !is_supported_installation_path(installation) {
        bail!(
            "不会删除 skills 目标目录之外的技能：{}",
            installation.path.display()
        );
    }
    let inferred = installation_project(installation);
    let project = project.or(inferred.as_deref());
    let mut instructions = if crate::host_instructions::RulePlan::is_file_host(installation.target) { Vec::new() } else {
        crate::host_instructions::RulePlan::for_target(installation.target, &installation.path)?.into_iter().collect()
    };
    if let Some(project) = project {
        instructions.extend(crate::host_instructions::RulePlan::file_host_removals(project, &installation.path)?);
    } else if crate::host_instructions::RulePlan::is_file_host(installation.target)
        || installation.target == InstallTarget::Copilot {
        bail!("无法确定受管 Rule 的项目目录：{}", installation.path.display());
    }
    let metadata = installation
        .path
        .symlink_metadata()
        .with_context(|| format!("找不到：{}", installation.path.display()))?;
    for instruction in &instructions { instruction.validate()?; }
    for mut instruction in instructions { instruction.detach()?; }
    if let Some(directory_link) = directory_link::inspect(&installation.path)? {
        directory_link::remove(&installation.path, directory_link)?;
    } else if metadata.is_file() {
        fs::remove_file(&installation.path)?;
    } else if metadata.is_dir() {
        fs::remove_dir_all(&installation.path)?;
    } else {
        bail!("无法删除这个技能");
    }
    Ok(())
}

/// Return the physical installation entry, resolving aliases in the parent
/// directory without resolving the Skill link itself. This keeps two paths
/// such as `.agents/skills/foo` and `.claude/skills/foo` from being removed
/// twice when one parent directory is a symlink to the other.
pub fn installation_key(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let parent = parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf());
    path.file_name()
        .map(|name| parent.join(name))
        .unwrap_or(parent)
}

fn rollback_links(created: &CreatedLinks) -> Vec<String> {
    created.links
        .iter()
        .rev()
        .filter_map(|link| {
            let result = (|| -> Result<()> {
                ensure!(crate::library::filesystem_identity(&created.project)? == created.project_identity,
                    "项目目录身份已变化，已保留安装链接");
                ensure!(directory_link::inspect(&link.path)?.as_ref() == Some(&link.expected)
                    && fs::read_link(&link.path).ok() == link.raw_target
                    && crate::library::filesystem_identity(&link.path)? == link.identity,
                    "安装链接归属冲突：身份或目标已变化，已保留");
                // Never adopt an identity observed during rollback. remove_link
                // also verifies the moved entry and restores a concurrent swap.
                crate::cross_project::remove_link(&link.path, &link.expected, &link.identity,
                    &created.project, &created.project_identity)
            })();
            result.err().map(|error| format!("{}：{error:#}", link.path.display()))
        })
        .collect()
}

pub fn remove_project_skills<'a>(
    installations: impl IntoIterator<Item = &'a ProjectSkillInstallation>,
) -> RemovalReport {
    remove_project_skills_in(None, installations)
}

pub fn remove_project_skills_at<'a>(
    project: &Path,
    installations: impl IntoIterator<Item = &'a ProjectSkillInstallation>,
) -> RemovalReport {
    remove_project_skills_in(Some(project), installations)
}

fn remove_project_skills_in<'a>(
    project: Option<&Path>,
    installations: impl IntoIterator<Item = &'a ProjectSkillInstallation>,
) -> RemovalReport {
    let mut report = RemovalReport::default();
    let mut removed_keys = HashSet::new();
    for installation in installations {
        if !removed_keys.insert(installation_key(&installation.path)) {
            continue;
        }
        match installation.path.symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                report
                    .failures
                    .push(format!("找不到：{}", installation.path.display()));
            }
            Err(error) => {
                report
                    .failures
                    .push(format!("{}：{}", installation.path.display(), error))
            }
            Ok(_) => match remove_project_skill_in(project, installation) {
                Ok(()) => report.removed += 1,
                Err(error) => report
                    .failures
                    .push(format!("{}：{error}", installation.path.display())),
            },
        }
    }
    report
}

pub fn list(project: &Path, library_dir: &Path) -> Result<Vec<ProjectSkill>> {
    let mut grouped = BTreeMap::<String, Vec<ProjectSkillInstallation>>::new();
    for workspace in crate::proma::project_workspaces(project).unwrap_or_default() {
        for installation in crate::proma::inspect(&workspace.path)? {
            let toggleable = installation.can_toggle();
            grouped.entry(installation.name).or_default().push(ProjectSkillInstallation {
                target: InstallTarget::Proma,
                path: installation.path,
                managed: installation.managed,
                source: installation.source,
                removable: installation.removable,
                toggleable,
                bundled: installation.bundled,
                integration: installation.integration,
                state: Some(installation.state),
                status: Some(installation.status),
            });
        }
    }
    let canonical_library = library_dir
        .canonicalize()
        .unwrap_or_else(|_| library_dir.to_path_buf());
    let linked_sources = fs::read_dir(library_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            directory_link::inspect(&path).ok().flatten()?;
            path.join("SKILL.md")
                .is_file()
                .then(|| path.canonicalize().ok())
                .flatten()
        })
        .collect::<HashSet<_>>();
    for target in PROJECT_INSTALL_TARGETS.iter().copied() {
        let Ok(parent) = installation_root(project, target) else { continue; };
        let Ok(entries) = fs::read_dir(&parent) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.join("SKILL.md").is_file() {
                continue;
            }
            let managed = path.canonicalize().ok().is_some_and(|resolved| {
                resolved.starts_with(&canonical_library)
                    || (linked_sources.contains(&resolved)
                        && directory_link::inspect(&path).ok().flatten().is_some())
            });
            grouped
                .entry(
                    crate::library::read_frontmatter(&path.join("SKILL.md"))
                        .ok()
                        .and_then(|(name, _)| name)
                        .unwrap_or_else(|| entry.file_name().to_string_lossy().into_owned()),
                )
                .or_default()
                .push(ProjectSkillInstallation {
                    target,
                    path,
                    managed,
                    source: None,
                    removable: true, toggleable: false,
                    bundled: false,
                    integration: None,
                    state: None,
                    status: None,
                });
        }
    }
    Ok(grouped
        .into_iter()
        .map(|(name, installations)| ProjectSkill {
            name,
            installations,
        })
        .collect())
}

pub fn is_installed_any(project: &Path, name: &str, library_dir: &Path) -> bool {
    is_installed_any_from_path(project, name, &library_dir.join(name))
}

pub fn is_installed_any_from_path(project: &Path, name: &str, source: &Path) -> bool {
    let source = source.canonicalize().ok();
    let Some(source) = source else {
        return false;
    };
    if crate::proma::project_workspaces(project).unwrap_or_default().iter().any(|workspace| {
        ["skills", "skills-inactive"].iter().any(|directory| {
            crate::proma::is_managed(&workspace.path.join(directory).join(name), &source)
        })
    }) {
        return true;
    }
    PROJECT_INSTALL_TARGETS.iter().any(|target| {
        let Ok(parent) = installation_root(project, *target) else { return false; };
        let path = parent.join(name);
        path.canonicalize()
            .ok()
            .is_some_and(|resolved| resolved == source)
            || fs::read_dir(parent)
                .into_iter()
                .flatten()
                .flatten()
                .any(|entry| entry.path().canonicalize().ok().as_ref() == Some(&source))
    })
}

#[cfg(test)]
mod tests {
    use crate::agents::target_directory;
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    struct Fixture {
        _temp: tempfile::TempDir,
        project: PathBuf,
        library: PathBuf,
        source: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let project = temp.path().join("project");
            let library = temp.path().join("library");
            let source = library.join("example");
            fs::create_dir_all(&project).unwrap();
            fs::create_dir_all(&source).unwrap();
            fs::write(source.join("SKILL.md"), "# Example").unwrap();
            Self {
                _temp: temp,
                project,
                library,
                source,
            }
        }

        fn installation(&self, target: InstallTarget) -> PathBuf {
            self.project.join(target_directory(target)).join("example")
        }
    }

    #[cfg(unix)]
    #[test]
    fn batch_install_covers_selected_projects_and_supported_agents() {
        let fixture = Fixture::new();
        let second = fixture._temp.path().join("second");
        let unselected = fixture._temp.path().join("unselected");
        let extra = fixture.library.join("extra");
        for path in [&second, &unselected, &extra] { fs::create_dir_all(path).unwrap(); }
        fs::write(extra.join("SKILL.md"), "# Extra").unwrap();
        let mut projects = vec![fixture.project.clone(), second.clone(), second.clone()];
        #[cfg(unix)] {
            let alias = fixture._temp.path().join("second-alias");
            symlink(&second, &alias).unwrap();
            projects.push(alias);
        }
        // OpenClaw and Hermes require isolated native trust; covered by their
        // workspace integration tests, without changing this test's HOME.
        let targets = PROJECT_INSTALL_TARGETS.iter().copied()
            .filter(|target| !matches!(target, InstallTarget::OpenClaw | InstallTarget::Hermes))
            .collect::<Vec<_>>();
        let skills = vec![("example".into(), fixture.source.clone()), ("extra".into(), extra.clone())];
        for _ in 0..2 {
            let report = install_batch(&projects, &skills, &targets);
            assert!(report.failures.is_empty(), "{:?}", report.failures);
            assert_eq!(report.installed, 2 * 2 * targets.len());
            assert_eq!(report.installed_projects.len(), 2);
        }
        for project in [&fixture.project, &second] {
            for (name, source) in &skills {
                for target in &targets {
                    let entry = project.join(target_directory(*target)).join(name);
                    assert_eq!(entry.canonicalize().unwrap(), source.canonicalize().unwrap());
                }
            }
        }
        assert_eq!(fs::read_dir(unselected).unwrap().count(), 0);
        assert_eq!(fs::read_to_string(fixture.source.join("SKILL.md")).unwrap(), "# Example");
    }

    #[cfg(unix)]
    #[test]
    fn batch_install_reports_conflict_without_overwriting_it() {
        let fixture = Fixture::new();
        let second = fixture._temp.path().join("second");
        let conflict = second.join(target_directory(InstallTarget::Codex)).join("example");
        fs::create_dir_all(&conflict).unwrap();
        fs::write(conflict.join("SKILL.md"), "User-owned skill").unwrap();
        let report = install_batch(&[fixture.project.clone(), second.clone()],
            &[("example".into(), fixture.source.clone())],
            &[InstallTarget::Codex, InstallTarget::ClaudeCode]);
        assert_eq!(report.installed, 3);
        assert_eq!(report.installed_projects.len(), 2);
        assert_eq!(report.failures.len(), 1);
        assert!(report.failures[0].contains(
            &crate::project_discovery::path_identity(&second).display().to_string()
        ));
        assert!(report.failures[0].contains("Codex"));
        assert_eq!(fs::read_to_string(conflict.join("SKILL.md")).unwrap(), "User-owned skill");
        assert_eq!(fs::read_to_string(fixture.source.join("SKILL.md")).unwrap(), "# Example");
    }

    #[test]
    fn proma_rejects_business_repo_install_and_removal() {
        let fixture = Fixture::new();
        assert!(install_from_path(&fixture.project, &fixture.source, "example", &[InstallTarget::Proma]).is_err());
        let path = fixture.project.join("skills/example");
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("SKILL.md"), "business fixture").unwrap();
        let installation = ProjectSkillInstallation {
            target: InstallTarget::Proma, path: path.clone(), managed: true, source: None, removable: false, toggleable: false, bundled: false, integration: None,
            state: Some("active".into()), status: Some("current".into()),
        };
        assert!(remove_project_skill(&installation).is_err());
        assert!(path.join("SKILL.md").is_file());
        let listed = list(&fixture.project, &fixture.library).unwrap();
        assert!(listed.iter().flat_map(|skill| &skill.installations)
            .all(|installation| installation.target != InstallTarget::Proma));
        // A plain skills/ directory is an OpenClaw workspace source, so its
        // visibility must not be mistaken for a Proma business-repo install.
        assert!(listed.iter().flat_map(|skill| &skill.installations)
            .any(|installation| installation.target == InstallTarget::OpenClaw
                && installation.path == path));
        assert!(!is_installed_any_from_path(&fixture.project, "example", &fixture.source));
    }

    #[test]
    fn global_installation_uses_user_roots_and_keeps_source_intact() {
        let fixture = Fixture::new();
        let home = fixture._temp.path().join("home");
        // Inject Copilot's default resolver; inherited COPILOT_HOME must never
        // move a fixture installation outside this temporary home.
        let targets = [
            InstallTarget::Universal,
            InstallTarget::Antigravity,
            InstallTarget::Copilot,
        ];
        let roots = targets.map(|target| if target == InstallTarget::Copilot {
            crate::agents::copilot_config_root_from(&home, None).unwrap().join("skills")
        } else { crate::agents::global_target_root(&home, target).unwrap() });
        assert!(roots.iter().all(|root| root.starts_with(&home)));
        fs::create_dir_all(&home).unwrap();
        for _ in 0..2 {
            install_to_roots(&home, &fixture.source, "example", roots.clone()).unwrap();
        }
        for root in roots {
            assert_eq!(
                root.join("example").canonicalize().unwrap(),
                fixture.source.canonicalize().unwrap()
            );
        }
        assert!(!home.join(".agent/skills").exists());
        assert!(!home.join(".github/skills").exists());
        assert!(fixture.source.join("SKILL.md").is_file());
    }

    #[cfg(windows)]
    #[test]
    fn unsupported_native_writes_reject_before_creating_links_or_changing_sources() {
        let fixture = Fixture::new();
        for target in [InstallTarget::Universal, InstallTarget::ClaudeCode, InstallTarget::Cursor,
            InstallTarget::Pi, InstallTarget::Omp, InstallTarget::Grok,
            InstallTarget::Antigravity, InstallTarget::Droid, InstallTarget::Copilot,
            InstallTarget::Trae, InstallTarget::WorkBuddy] {
            let error = install_from_path(&fixture.project, &fixture.source, "example", &[target])
                .unwrap_err().to_string();
            assert!(error.contains("此平台尚未支持原生适配受管写入"), "{target:?}: {error}");
            assert_eq!(fs::read_dir(&fixture.project).unwrap().count(), 0, "{target:?}");
            assert_eq!(fs::read(fixture.source.join("SKILL.md")).unwrap(), b"# Example");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_plain_link_installation_and_mixed_batch_keep_exact_results() {
        let fixture = Fixture::new();
        let report = install_batch(&[fixture.project.clone()],
            &[("example".into(), fixture.source.clone())],
            &[InstallTarget::Codex, InstallTarget::ClaudeCode]);
        assert_eq!(report.installed, 1, "{:?}", report.failures);
        assert_eq!(report.failures.len(), 1);
        assert!(report.failures[0].contains("此平台尚未支持原生适配受管写入"));
        assert!(!fixture.project.join(".claude").exists());
        let link = fixture.installation(InstallTarget::Codex);
        assert_eq!(link.canonicalize().unwrap(), fixture.source.canonicalize().unwrap());
        install_from_path(&fixture.project, &fixture.source, "example", &[InstallTarget::Codex]).unwrap();
        uninstall(&fixture.project, &fixture.library, "example", &[InstallTarget::Codex]).unwrap();
        assert!(link.symlink_metadata().is_err());
        assert_eq!(fs::read(fixture.source.join("SKILL.md")).unwrap(), b"# Example");
    }

    #[test]
    fn global_installation_copilot_override_cannot_escape_fixture() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let sentinel = root.join("external-copilot");
        fs::create_dir(&sentinel).unwrap();
        fs::write(sentinel.join("sentinel"), "KEEP_EXTERNAL").unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "project::tests::global_installation_uses_user_roots_and_keeps_source_intact"])
            .env_clear().env("HOME", &root).env("USERPROFILE", &root)
            .env("COPILOT_HOME", &sentinel).output().unwrap();
        assert!(status.status.success());
        assert_eq!(fs::read(sentinel.join("sentinel")).unwrap(), b"KEEP_EXTERNAL");
        assert_eq!(fs::read_dir(sentinel).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn copilot_failed_multi_target_install_rolls_back_only_new_alias_entry() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.source.join("local-data")).unwrap();
        fs::write(fixture.source.join("local-data/ADAPTATION.md"), "KEEP_LOCAL").unwrap();
        let shared = fixture.project.join(".agents/skills");
        fs::create_dir_all(&shared).unwrap();
        fs::create_dir_all(fixture.project.join(".github")).unwrap();
        std::os::unix::fs::symlink(&shared, fixture.project.join(".github/skills")).unwrap();
        directory_link::create(&fixture.source, &shared.join("example")).unwrap();
        let link_identity = crate::library::filesystem_identity(&shared.join("example")).unwrap();
        let mut original = crate::host_instructions::RulePlan::for_copilot(
            &fixture.project, &fixture.project.join(".github/skills/example")).unwrap();
        original.install().unwrap();
        let instruction = fixture.project.join(".github/copilot-instructions.md");
        fs::set_permissions(&instruction, fs::Permissions::from_mode(0o640)).unwrap();
        let before = fs::read(&instruction).unwrap();
        let changed_config = fixture.project.join(".opencode/opencode.jsonc");
        AFTER_LINKS_INSTALL.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            fs::create_dir_all(changed_config.parent().unwrap()).unwrap();
            fs::write(&changed_config, "// USER_CHANGED_AFTER_PREVIEW\n{}").unwrap();
        })));
        assert!(install_from_path(&fixture.project, &fixture.source, "example",
            &[InstallTarget::Copilot, InstallTarget::Universal, InstallTarget::OpenCode]).is_err());
        assert_eq!(fs::read(&instruction).unwrap(), before);
        assert_eq!(fs::metadata(&instruction).unwrap().permissions().mode() & 0o777, 0o640);
        assert_eq!(crate::library::filesystem_identity(&shared.join("example")).unwrap(), link_identity);
        assert_eq!(fs::read(fixture.source.join("SKILL.md")).unwrap(), b"# Example");
        assert_eq!(fs::read(fixture.source.join("local-data/ADAPTATION.md")).unwrap(), b"KEEP_LOCAL");
    }

    #[cfg(unix)]
    #[test]
    fn installs_idempotently_and_uninstalls_without_touching_the_source() {
        let fixture = Fixture::new();
        let targets = [InstallTarget::Universal, InstallTarget::ClaudeCode];

        install(&fixture.project, &fixture.library, "example", &targets).unwrap();
        install(&fixture.project, &fixture.library, "example", &targets).unwrap();

        for target in targets {
            let path = fixture.installation(target);
            assert_eq!(
                path.canonicalize().unwrap(),
                fixture.source.canonicalize().unwrap()
            );
            assert!(directory_link::inspect(&path).unwrap().is_some());
        }

        uninstall(&fixture.project, &fixture.library, "example", &targets).unwrap();
        for target in targets {
            assert!(fixture.installation(target).symlink_metadata().is_err());
        }
        assert_eq!(
            fs::read_to_string(fixture.source.join("SKILL.md")).unwrap(),
            "# Example"
        );
    }

    #[cfg(unix)]
    #[test]
    fn direct_instruction_failure_preserves_replaced_link_and_original() {
        // A second link to the same source must also fail the ownership check.
        for same_target in [false, true] {
            let fixture = Fixture::new();
            let existing = fixture.installation(InstallTarget::Universal);
            fs::create_dir_all(existing.parent().unwrap()).unwrap();
            directory_link::create(&fixture.source, &existing).unwrap();
            let existing_identity = crate::library::filesystem_identity(&existing).unwrap();
            let other = fixture.library.join("user-skill");
            fs::create_dir(&other).unwrap();
            fs::write(other.join("sentinel"), "user target").unwrap();
            fs::write(fixture.source.join("sentinel"), "source target").unwrap();
            let prepared = fixture._temp.path().join("prepared-link");
            let target = if same_target { &fixture.source } else { &other };
            directory_link::create(target, &prepared).unwrap();
            let expected = directory_link::inspect(&prepared).unwrap().unwrap();
            let raw_target = fs::read_link(&prepared).ok();
            let identity = crate::library::filesystem_identity(&prepared).unwrap();
            let link = fixture.installation(InstallTarget::ClaudeCode);
            let retained = fixture.project.join("original-link");
            let rule = crate::host_instructions::RulePlan::for_target(InstallTarget::ClaudeCode, &link)
                .unwrap().unwrap().path;
            let (swap_link, swap_retained, conflict) = (link.clone(), retained.clone(), rule.clone());
            AFTER_LINKS_INSTALL.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
                fs::rename(&swap_link, &swap_retained).unwrap();
                fs::rename(&prepared, &swap_link).unwrap();
                fs::create_dir_all(conflict.parent().unwrap()).unwrap();
                fs::write(conflict, "user rule").unwrap();
            })));

            let error = install_from_path(&fixture.project, &fixture.source, "example",
                &[InstallTarget::Universal, InstallTarget::ClaudeCode]).unwrap_err().to_string();

            assert!(error.contains("安装原生适配入口失败"), "{error}");
            assert!(error.contains("回滚失败") && error.contains("归属冲突"), "{error}");
            assert!(error.contains(&link.display().to_string()), "{error}");
            assert_eq!(directory_link::inspect(&link).unwrap().as_ref(), Some(&expected));
            assert_eq!(fs::read_link(&link).ok(), raw_target);
            assert_eq!(crate::library::filesystem_identity(&link).unwrap(), identity);
            assert_eq!(crate::library::filesystem_identity(&existing).unwrap(), existing_identity);
            assert_eq!(retained.canonicalize().unwrap(), fixture.source.canonicalize().unwrap());
            #[cfg(unix)]
            assert_eq!(fs::read_link(&retained).unwrap(), fixture.source.canonicalize().unwrap());
            assert_eq!(fs::read_to_string(rule).unwrap(), "user rule");
            assert_eq!(fs::read_to_string(other.join("sentinel")).unwrap(), "user target");
            assert_eq!(fs::read_to_string(fixture.source.join("sentinel")).unwrap(), "source target");
            assert_eq!(fs::read_to_string(fixture.source.join("SKILL.md")).unwrap(), "# Example");
        }
    }

    #[cfg(unix)]
    #[test]
    fn direct_instruction_failure_detaches_only_created_link_for_recovery() {
        let fixture = Fixture::new();
        let link = fixture.installation(InstallTarget::ClaudeCode);
        let rule = crate::host_instructions::RulePlan::for_target(InstallTarget::ClaudeCode, &link)
            .unwrap().unwrap().path;
        let conflict = rule.clone();
        AFTER_LINKS_INSTALL.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            fs::create_dir_all(conflict.parent().unwrap()).unwrap();
            fs::write(conflict, "user rule").unwrap();
        })));

        let error = install_from_path(&fixture.project, &fixture.source, "example",
            &[InstallTarget::ClaudeCode]).unwrap_err().to_string();

        assert!(error.contains("安装原生适配入口失败"), "{error}");
        assert!(link.symlink_metadata().is_err());
        let recovery = fs::read_dir(&fixture.project).unwrap().map(|entry| entry.unwrap())
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".kitter-removed-"))
            .map(|entry| entry.path()).collect::<Vec<_>>();
        assert_eq!(recovery.len(), 1, "{error}");
        let retained = recovery[0].join("installation");
        assert!(directory_link::inspect(&retained).unwrap().is_some());
        assert_eq!(retained.canonicalize().unwrap(), fixture.source.canonicalize().unwrap());
        #[cfg(unix)]
        assert_eq!(fs::read_link(&retained).unwrap(), fixture.source.canonicalize().unwrap());
        assert!(recovery[0].join("restore.json").is_file());
        assert_eq!(fs::read_to_string(rule).unwrap(), "user rule");
        assert_eq!(fs::read_to_string(fixture.source.join("SKILL.md")).unwrap(), "# Example");
    }

    #[cfg(unix)]
    #[test]
    fn direct_instruction_failure_preserves_replaced_project() {
        let fixture = Fixture::new();
        let project = fixture.project.clone();
        let original_project = fixture._temp.path().join("original-project");
        let link = fixture.installation(InstallTarget::ClaudeCode);
        let other = fixture.library.join("user-skill");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("sentinel"), "user target").unwrap();
        let rule = crate::host_instructions::RulePlan::for_target(InstallTarget::ClaudeCode, &link)
            .unwrap().unwrap().path;
        let (retained, replacement, conflict) = (original_project.clone(), link.clone(), rule.clone());
        AFTER_LINKS_INSTALL.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            fs::rename(&project, retained).unwrap();
            fs::create_dir_all(replacement.parent().unwrap()).unwrap();
            directory_link::create(&other, &replacement).unwrap();
            fs::create_dir_all(conflict.parent().unwrap()).unwrap();
            fs::write(conflict, "user rule").unwrap();
        })));

        let error = install_from_path(&fixture.project, &fixture.source, "example",
            &[InstallTarget::ClaudeCode]).unwrap_err().to_string();

        assert!(error.contains("安装原生适配入口失败"), "{error}");
        assert!(error.contains("回滚失败") && error.contains("项目目录身份已变化"), "{error}");
        assert_eq!(link.canonicalize().unwrap(), fixture.library.join("user-skill").canonicalize().unwrap());
        assert_eq!(original_project.join(target_directory(InstallTarget::ClaudeCode)).join("example")
            .canonicalize().unwrap(), fixture.source.canonicalize().unwrap());
        assert_eq!(fs::read_to_string(link.join("sentinel")).unwrap(), "user target");
        assert_eq!(fs::read_to_string(rule).unwrap(), "user rule");
        assert_eq!(fs::read_to_string(fixture.source.join("SKILL.md")).unwrap(), "# Example");
    }

    #[cfg(unix)]
    #[test]
    fn antigravity_rules_follow_public_shared_and_alias_removals() {
        for aliased in [false, true] {
            for entry in 0..5 {
                let f = Fixture::new();
                let shared = f.installation(InstallTarget::Universal);
                let target = if aliased {
                    fs::create_dir_all(shared.parent().unwrap()).unwrap();
                    fs::create_dir_all(f.project.join(".codex")).unwrap();
                    symlink(shared.parent().unwrap(), f.project.join(".codex/skills")).unwrap();
                    InstallTarget::Codex
                } else { InstallTarget::Universal };
                install_from_path(&f.project, &f.source, "example", &[target]).unwrap();
                let rule = crate::host_instructions::RulePlan::for_file_host(
                    &f.project, InstallTarget::Antigravity, &f.installation(target)).unwrap().path;
                assert!(rule.is_file());
                match entry {
                    0 | 1 => {
                        let listed = list(&f.project, &f.library).unwrap();
                        let item = listed.iter().flat_map(|skill| &skill.installations)
                            .find(|item| item.target == InstallTarget::Universal).unwrap();
                        if entry == 0 { remove_project_skill(item).unwrap(); }
                        else { remove_project_skill_at(&f.project, item).unwrap(); }
                    }
                    2 => uninstall(&f.project, &f.library, "example", &[InstallTarget::Universal]).unwrap(),
                    3 => uninstall_all(&f.project, "example", &f.library).unwrap(),
                    _ => {
                        validate_uninstall_all_from_path(&f.project, "example", &f.source).unwrap();
                        assert!(rule.is_file() && shared.exists());
                        uninstall_all_from_path(&f.project, "example", &f.source).unwrap();
                    }
                }
                assert!(!rule.exists(), "alias={aliased}, entry={entry}");
                assert!(shared.symlink_metadata().is_err());
                assert_eq!(fs::read_to_string(f.source.join("SKILL.md")).unwrap(), "# Example");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn antigravity_source_filtered_alias_removal_preserves_same_name_native_rule() {
        for explicit_source in [false, true] {
            let f = Fixture::new();
            let other = f._temp.path().join("other/example");
            fs::create_dir_all(&other).unwrap();
            fs::write(other.join("SKILL.md"), "other source").unwrap();
            let shared = f.project.join(".agents/skills");
            fs::create_dir_all(&shared).unwrap();
            fs::create_dir_all(f.project.join(".codex")).unwrap();
            symlink(&shared, f.project.join(".codex/skills")).unwrap();
            install_from_path(&f.project, &f.source, "example", &[InstallTarget::Codex]).unwrap();
            install_from_path(&f.project, &other, "example", &[InstallTarget::Antigravity]).unwrap();
            let shared_rule = crate::host_instructions::RulePlan::for_file_host(
                &f.project, InstallTarget::Antigravity, &f.installation(InstallTarget::Codex)).unwrap().path;
            let native_rule = crate::host_instructions::RulePlan::for_file_host(
                &f.project, InstallTarget::Antigravity, &f.installation(InstallTarget::Antigravity)).unwrap().path;
            assert_ne!(shared_rule, native_rule);
            let native_before = fs::read(&native_rule).unwrap();
            if explicit_source {
                validate_uninstall_all_from_path(&f.project, "example", &f.source).unwrap();
                uninstall_all_from_path(&f.project, "example", &f.source).unwrap();
            } else {
                uninstall_all(&f.project, "example", &f.library).unwrap();
            }
            assert!(!shared_rule.exists());
            assert_eq!(fs::read(native_rule).unwrap(), native_before);
            assert_eq!(f.installation(InstallTarget::Antigravity).canonicalize().unwrap(), other.canonicalize().unwrap());
            assert_eq!(fs::read_to_string(other.join("SKILL.md")).unwrap(), "other source");
            assert_eq!(fs::read_to_string(f.source.join("SKILL.md")).unwrap(), "# Example");
        }
    }

    #[cfg(unix)]
    #[test]
    fn grok_public_project_hooks_and_legacy_rules_follow_all_removal_entries() {
        for entry in 0..5 {
            let f = Fixture::new();
            fs::create_dir_all(f.source.join("local-data")).unwrap();
            fs::write(f.source.join("local-data/ADAPTATION.md"), "KEEP_LOCAL").unwrap();
            let slot = f.installation(InstallTarget::Grok);
            install_from_path(&f.project, &f.source, "example", &[InstallTarget::Grok]).unwrap();
            let hook = crate::host_instructions::RulePlan::grok_hooks_for_install(&f.project, &slot, &f.source).unwrap()[0].path.clone();
            let legacy = crate::host_instructions::RulePlan::for_file_host(&f.project, InstallTarget::Grok, &slot).unwrap().path;
            let identity = crate::library::filesystem_identity(&hook).unwrap();
            install_from_path(&f.project, &f.source, "example", &[InstallTarget::Grok]).unwrap();
            assert_eq!(crate::library::filesystem_identity(&hook).unwrap(), identity);
            assert_eq!(fs::read_dir(hook.parent().unwrap()).unwrap().count(), 1);
            assert!(legacy.is_file());
            match entry {
                0 => uninstall(&f.project, &f.library, "example", &[InstallTarget::Grok]).unwrap(),
                1 => uninstall_all(&f.project, "example", &f.library).unwrap(),
                2 => uninstall_all_from_path(&f.project, "example", &f.source).unwrap(),
                _ => {
                    let installed = list(&f.project, &f.library).unwrap();
                    let item = installed.iter().flat_map(|skill| &skill.installations)
                        .find(|item| item.target == InstallTarget::Grok).unwrap();
                    if entry == 3 { remove_project_skill_at(&f.project, item).unwrap(); }
                    else { remove_project_skill(item).unwrap(); }
                }
            }
            assert!(!hook.exists() && !legacy.exists());
            assert!(slot.symlink_metadata().is_err());
            assert_eq!(fs::read(f.source.join("local-data/ADAPTATION.md")).unwrap(), b"KEEP_LOCAL");
        }
    }

    #[cfg(unix)]
    #[test]
    fn grok_public_compatibility_shared_slot_and_same_name_sources_stay_separate() {
        let f = Fixture::new();
        let shared_source = f._temp.path().join(".shared/library/example");
        fs::create_dir_all(&shared_source).unwrap();
        fs::write(shared_source.join("SKILL.md"), "SHARED_SOURCE").unwrap();
        let shared = f.project.join(".shared/skills");
        fs::create_dir_all(&shared).unwrap();
        for root in [".agents", ".codex"] {
            fs::create_dir(f.project.join(root)).unwrap();
            symlink(&shared, f.project.join(root).join("skills")).unwrap();
        }
        install_from_path(&f.project, &shared_source, "example", &[InstallTarget::Universal]).unwrap();
        install_from_path(&f.project, &f.source, "example", &[InstallTarget::Grok]).unwrap();
        let shared_slot = f.installation(InstallTarget::Universal);
        let native_slot = f.installation(InstallTarget::Grok);
        let shared_hook = crate::host_instructions::RulePlan::grok_hooks_for_install(&f.project, &shared_slot, &shared_source).unwrap()[0].path.clone();
        let native_hook = crate::host_instructions::RulePlan::grok_hooks_for_install(&f.project, &native_slot, &f.source).unwrap()[0].path.clone();
        assert_ne!(shared_hook, native_hook);
        let native_before = fs::read(&native_hook).unwrap();
        uninstall_all_from_path(&f.project, "example", &shared_source).unwrap();
        assert!(!shared_hook.exists());
        assert_eq!(fs::read(&native_hook).unwrap(), native_before);
        assert_eq!(native_slot.canonicalize().unwrap(), f.source.canonicalize().unwrap());
        assert!(shared_source.join("SKILL.md").is_file());
        uninstall(&f.project, &f.library, "example", &[InstallTarget::Grok]).unwrap();
        assert!(!native_hook.exists());
    }

    #[cfg(unix)]
    #[test]
    fn grok_public_install_cas_conflict_rolls_back_only_new_link() {
        let f = Fixture::new();
        let slot = f.installation(InstallTarget::Grok);
        let hook = crate::host_instructions::RulePlan::grok_hooks_for_install(&f.project, &slot, &f.source).unwrap()[0].path.clone();
        let conflict = hook.clone();
        AFTER_LINKS_INSTALL.with(|callback| *callback.borrow_mut() = Some(Box::new(move || {
            fs::create_dir_all(conflict.parent().unwrap()).unwrap();
            fs::write(conflict, "USER_HOOK").unwrap();
        })));
        assert!(install_from_path(&f.project, &f.source, "example", &[InstallTarget::Grok]).is_err());
        assert!(slot.symlink_metadata().is_err());
        assert_eq!(fs::read(&hook).unwrap(), b"USER_HOOK");
        assert_eq!(fs::read(f.source.join("SKILL.md")).unwrap(), b"# Example");
    }

    #[cfg(unix)]
    #[test]
    fn grok_public_old_reader_detach_sync_failure_restores_registration_and_retry() {
        let f = Fixture::new(); let slot = f.installation(InstallTarget::Grok);
        install_from_path(&f.project, &f.source, "example", &[InstallTarget::Grok]).unwrap();
        let hook = crate::host_instructions::RulePlan::grok_hooks_for_install(&f.project, &slot, &f.source).unwrap()[0].path.clone();
        let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&hook).unwrap()).unwrap();
        let reader = f._temp.path().join("retired-reader");
        let id = crate::library::filesystem_identity(&f.source).unwrap();
        let handler = value.pointer_mut("/hooks/PostToolUse/0/hooks/0").unwrap();
        let installed = handler["env"]["PLEAT_GROK_INSTALLATION"].as_str().unwrap().to_owned();
        handler["env"]["PLEAT_GROK_READER"] = serde_json::json!(reader);
        handler["command"] = serde_json::json!(format!("'{}' --pleat-native-grok-adaptation-v1 --installation '{}' --source-identity '{}'", reader.display(), installed, id));
        value.sort_all_objects();
        let old = format!("{}\n", serde_json::to_string(&value).unwrap()).into_bytes();
        fs::write(&hook, &old).unwrap();
        let slot_id = crate::library::filesystem_identity(&slot).unwrap();
        crate::host_instructions::BEFORE_DETACH_SYNC.with(|callback| *callback.borrow_mut() = Some(Box::new(|| {
            anyhow::bail!("injected public detach durability failure")
        })));
        assert!(install_from_path(&f.project, &f.source, "example", &[InstallTarget::Grok]).is_err());
        assert!(fs::read(&hook).unwrap() == old);
        assert_eq!(crate::library::filesystem_identity(&slot).unwrap(), slot_id);
        assert_eq!(fs::read(f.source.join("SKILL.md")).unwrap(), b"# Example");
        install_from_path(&f.project, &f.source, "example", &[InstallTarget::Grok]).unwrap();
        assert!(fs::read(&hook).unwrap() != old);
    }

    #[cfg(unix)]
    #[test]
    fn grok_public_later_legacy_conflict_rolls_back_published_hook_before_link() {
        let f = Fixture::new(); let slot = f.installation(InstallTarget::Grok);
        let hook = crate::host_instructions::RulePlan::grok_hooks_for_install(&f.project, &slot, &f.source).unwrap()[0].path.clone();
        let legacy = crate::host_instructions::RulePlan::for_file_host(&f.project, InstallTarget::Grok, &slot).unwrap().path;
        let conflict = legacy.clone();
        AFTER_LINKS_INSTALL.with(|callback| *callback.borrow_mut() = Some(Box::new(move || {
            fs::create_dir_all(conflict.parent().unwrap()).unwrap();
            fs::write(conflict, "USER_LEGACY_RULE").unwrap();
        })));
        assert!(install_from_path(&f.project, &f.source, "example", &[InstallTarget::Grok]).is_err());
        assert!(!hook.exists()); assert!(slot.symlink_metadata().is_err());
        assert_eq!(fs::read(legacy).unwrap(), b"USER_LEGACY_RULE");
    }

    #[cfg(unix)]
    #[test]
    fn grok_public_install_preserves_slot_replaced_after_link_creation() {
        let f = Fixture::new();
        let slot = f.installation(InstallTarget::Grok);
        let hook = crate::host_instructions::RulePlan::grok_hooks_for_install(&f.project, &slot, &f.source).unwrap()[0].path.clone();
        let replacement = slot.clone(); let source = f.source.clone();
        let retained = f._temp.path().join("retained-slot");
        AFTER_LINKS_INSTALL.with(|callback| *callback.borrow_mut() = Some(Box::new(move || {
            fs::rename(&replacement, retained).unwrap();
            symlink(&source, &replacement).unwrap();
        })));
        assert!(install_from_path(&f.project, &f.source, "example", &[InstallTarget::Grok]).is_err());
        assert!(!hook.exists());
        assert_eq!(slot.canonicalize().unwrap(), f.source.canonicalize().unwrap());
        assert!(f._temp.path().join("retained-slot").symlink_metadata().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn grok_public_exact_removal_does_not_require_a_live_source() {
        let f = Fixture::new();
        let slot = f.installation(InstallTarget::Grok);
        install_from_path(&f.project, &f.source, "example", &[InstallTarget::Grok]).unwrap();
        let hook = crate::host_instructions::RulePlan::grok_hooks_for_install(&f.project, &slot, &f.source).unwrap()[0].path.clone();
        let installed = list(&f.project, &f.library).unwrap();
        let item = installed.iter().flat_map(|skill| &skill.installations)
            .find(|item| item.target == InstallTarget::Grok).unwrap();
        fs::rename(&f.source, f._temp.path().join("retained-source")).unwrap();
        remove_project_skill_at(&f.project, item).unwrap();
        assert!(!hook.exists()); assert!(slot.symlink_metadata().is_err());
        assert!(f._temp.path().join("retained-source/SKILL.md").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn grok_global_public_lifecycle_runs_in_environment_cleared_child() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for custom in [false, true] {
            let home = root.join(if custom { "custom-home" } else { "default-home" });
            fs::create_dir(&home).unwrap();
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child.args(["--exact", "project::tests::grok_global_public_lifecycle_child", "--ignored"])
                .env_clear().env("HOME", &home).env("USERPROFILE", &home)
                .env("PLEAT_GROK_PRODUCT_TEST_HOME", &home);
            if custom { child.env("GROK_HOME", home.join("custom-grok")); }
            assert!(child.output().unwrap().status.success());
        }
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "only launched with a temporary HOME by the parent test"]
    fn grok_global_public_lifecycle_child() {
        let home = PathBuf::from(std::env::var_os("PLEAT_GROK_PRODUCT_TEST_HOME").expect("isolated parent required"));
        assert_eq!(dirs::home_dir().unwrap(), home);
        let library = home.join("manager/skills"); let source = library.join("example");
        fs::create_dir_all(source.join("local-data")).unwrap();
        fs::write(source.join("SKILL.md"), "GLOBAL_SOURCE").unwrap();
        fs::write(source.join("local-data/ADAPTATION.md"), "GLOBAL_LOCAL").unwrap();
        // No trigger policy API or live registry is involved in these operations.
        fs::write(home.join("manager/registry.json"), r#"{"trigger_overrides":{}}"#).unwrap();
        let slot = installation_root(&home, InstallTarget::Grok).unwrap().join("example");
        assert!(slot.starts_with(&home));
        install_from_path(&home, &source, "example", &[InstallTarget::Grok]).unwrap();
        let hook = crate::host_instructions::RulePlan::grok_hooks_for_install(&home, &slot, &source).unwrap()[0].path.clone();
        let legacy = crate::host_instructions::RulePlan::for_file_host(&home, InstallTarget::Grok, &slot).unwrap().path;
        assert_eq!(hook.parent().unwrap(), slot.parent().unwrap().parent().unwrap().join("hooks"));
        assert!(hook.is_file() && legacy.is_file());
        install_from_path(&home, &source, "example", &[InstallTarget::Grok]).unwrap();
        assert_eq!(fs::read_dir(hook.parent().unwrap()).unwrap().count(), 1);
        uninstall(&home, &library, "example", &[InstallTarget::Grok]).unwrap();
        assert!(!hook.exists() && !legacy.exists());
        assert!(slot.symlink_metadata().is_err());
        assert_eq!(fs::read(source.join("local-data/ADAPTATION.md")).unwrap(), b"GLOBAL_LOCAL");
    }

    #[cfg(unix)]
    #[test]
    fn lists_and_removes_every_supported_project_install_target() {
        let fixture = Fixture::new();
        // Trust-protected scopes have their own isolated workspace contracts.
        let targets = PROJECT_INSTALL_TARGETS.iter().copied()
            .filter(|target| !matches!(target, InstallTarget::OpenClaw | InstallTarget::Hermes))
            .collect::<Vec<_>>();

        install(&fixture.project, &fixture.library, "example", &targets).unwrap();

        let listed = list(&fixture.project, &fixture.library).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].installations.len(), targets.len());
        for target in targets.iter().copied() {
            assert!(fixture.installation(target).symlink_metadata().is_ok());
            assert!(listed[0].installations.iter().any(|installation| {
                installation.target == target
                    && installation.path == fixture.installation(target)
                    && installation.managed
            }));
        }

        uninstall(&fixture.project, &fixture.library, "example", &targets).unwrap();
        for target in targets {
            assert!(fixture.installation(target).symlink_metadata().is_err());
        }
    }

    #[test]
    fn refuses_an_occupied_directory_before_creating_any_links() {
        let fixture = Fixture::new();
        let occupied = fixture.installation(InstallTarget::ClaudeCode);
        fs::create_dir_all(&occupied).unwrap();
        fs::write(occupied.join("keep"), "user data").unwrap();

        #[cfg(unix)]
        let error = install(
            &fixture.project,
            &fixture.library,
            "example",
            &[InstallTarget::Universal, InstallTarget::ClaudeCode],
        )
        .unwrap_err();
        // The public native-write path is unsupported on Windows. Exercise
        // the same link preflight with two plain directory-link roots.
        #[cfg(windows)]
        let error = install_to_roots(&fixture.project, &fixture.source, "example",
            [fixture.project.join(target_directory(InstallTarget::Universal)),
             fixture.project.join(target_directory(InstallTarget::ClaudeCode))]).err().unwrap();

        assert!(error.to_string().contains("安装位置已被占用"));
        assert!(
            fixture
                .installation(InstallTarget::Universal)
                .symlink_metadata()
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(occupied.join("keep")).unwrap(),
            "user data"
        );
    }

    #[test]
    fn refuses_to_uninstall_a_link_to_another_source() {
        let fixture = Fixture::new();
        let other = fixture.library.join("other");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("SKILL.md"), "# Other").unwrap();
        let link = fixture.installation(InstallTarget::Universal);
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        directory_link::create(&other, &link).unwrap();

        let error = uninstall(
            &fixture.project,
            &fixture.library,
            "example",
            &[InstallTarget::Universal],
        )
        .unwrap_err();

        assert!(error.to_string().contains("不会删除指向其他位置的链接"));
        assert_eq!(link.canonicalize().unwrap(), other.canonicalize().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_uninstall_when_the_managed_source_is_missing() {
        let fixture = Fixture::new();
        let link = fixture.installation(InstallTarget::Universal);
        install(
            &fixture.project,
            &fixture.library,
            "example",
            &[InstallTarget::Universal],
        )
        .unwrap();
        fs::remove_dir_all(&fixture.source).unwrap();

        let error = uninstall(
            &fixture.project,
            &fixture.library,
            "example",
            &[InstallTarget::Universal],
        )
        .unwrap_err();

        assert!(error.to_string().contains("无法安全验证安装链接"));
        assert!(link.symlink_metadata().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn removes_an_aliased_installation_only_once() {
        let fixture = Fixture::new();
        install(
            &fixture.project,
            &fixture.library,
            "example",
            &[InstallTarget::Universal],
        )
        .unwrap();
        let claude_root = fixture.project.join(".claude");
        fs::create_dir_all(&claude_root).unwrap();
        symlink(
            fixture.project.join(".agents/skills"),
            claude_root.join("skills"),
        )
        .unwrap();

        let listed = list(&fixture.project, &fixture.library).unwrap();
        assert_eq!(listed[0].installations.len(), 2);
        assert_eq!(
            installation_key(&listed[0].installations[0].path),
            installation_key(&listed[0].installations[1].path)
        );

        let report = remove_project_skills(listed[0].installations.iter());
        assert_eq!(report.removed, 1);
        assert!(report.failures.is_empty());
        assert!(
            fixture
                .installation(InstallTarget::Universal)
                .symlink_metadata()
                .is_err()
        );
    }

    #[test]
    fn removes_a_direct_unmanaged_installation() {
        let fixture = Fixture::new();
        let path = fixture.installation(InstallTarget::Universal);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("SKILL.md"), "# External").unwrap();
        let installation = ProjectSkillInstallation {
            target: InstallTarget::Universal,
            path: path.clone(),
            managed: false, source: None,
            removable: true, toggleable: false,
            bundled: false,
            integration: None,
            state: None,
            status: None,
        };

        let report = remove_project_skills(std::iter::once(&installation));

        assert_eq!(report.removed, 1);
        assert!(report.failures.is_empty());
        assert!(path.symlink_metadata().is_err());
    }

    #[test]
    fn refuses_to_remove_an_installation_outside_a_supported_skill_root() {
        let fixture = Fixture::new();
        let path = fixture.project.join("external").join("example");
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("SKILL.md"), "# External").unwrap();
        let installation = ProjectSkillInstallation {
            target: InstallTarget::Universal,
            path: path.clone(),
            managed: false, source: None,
            removable: false, toggleable: false,
            bundled: false,
            integration: None,
            state: None,
            status: None,
        };

        let report = remove_project_skills(std::iter::once(&installation));

        assert_eq!(report.removed, 0);
        assert_eq!(report.failures.len(), 1);
        assert!(path.join("SKILL.md").is_file());
    }

    #[test]
    fn rejects_skill_names_that_can_escape_the_target_directory() {
        let fixture = Fixture::new();
        let error = install(
            &fixture.project,
            &fixture.library,
            "../example",
            &[InstallTarget::Universal],
        )
        .unwrap_err();

        assert!(error.to_string().contains("技能名称无效"));
    }
}
