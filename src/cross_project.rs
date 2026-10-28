//! One explicit plan for project links and registered Proma workspace copies.
use crate::{InstallTarget, agents::{self, AgentIconInfo}, directory_link, proma, proma_batch, project_discovery::ProjectEntry};
use anyhow::{Context, Result, bail, ensure};
use std::{collections::{BTreeMap, BTreeSet}, fs, path::{Path, PathBuf}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action { Install, Remove, Convert, Enable, Disable }

impl Action {
    pub fn is_state(self) -> bool { matches!(self, Self::Enable | Self::Disable) }
    pub fn supported(self, agents: &BTreeSet<String>) -> bool {
        !agents.is_empty() && (!(self.is_state() || self == Self::Convert) || agents.iter().all(|id| id == "proma"))
    }
}

#[derive(Clone, Debug)]
pub struct Skill {
    pub key: String,
    pub name: String,
    pub source: PathBuf,
    pub provider: &'static str,
    /// Only library skills may be newly installed. Other entries are inspectable.
    pub install_name: Option<String>,
}

impl Skill {
    pub fn is_preset(&self) -> bool { matches!(self.provider, "builtin" | "integration") }
}

#[derive(Clone, Debug)]
struct LinkEntry { target: InstallTarget, path: PathBuf, key: String, hermes_only: bool }

#[derive(Clone, Debug)]
pub struct Catalog {
    pub skills: Vec<Skill>,
    pub projects: Vec<ProjectEntry>,
    pub installed_agents: Vec<AgentIconInfo>,
    pub proma: Option<proma_batch::Catalog>,
    pub proma_error: Option<String>,
    links: BTreeMap<PathBuf, Vec<LinkEntry>>,
}

/// Resolve the existing prefix, including directory aliases, but not the entry.
pub(crate) fn physical_entry(path: &Path) -> Result<PathBuf> {
    let name = path.file_name().context("安装位置缺少名称")?;
    let mut parent = path.parent().context("安装位置缺少父目录")?;
    let mut missing = Vec::new();
    loop {
        match parent.symlink_metadata() {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(parent.file_name().context("缺少项目目录")?.to_os_string());
                parent = parent.parent().context("缺少项目目录")?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let mut result = parent.canonicalize().context("无法解析安装目录")?;
    ensure!(result.is_dir(), "安装父目录不是文件夹");
    for part in missing.into_iter().rev() { result.push(part); }
    Ok(result.join(name))
}

impl Catalog {
    pub fn scan(mut skills: Vec<Skill>, mut projects: Vec<ProjectEntry>, installed_agents: Vec<AgentIconInfo>, proma_root: Option<&Path>) -> Self {
        let (proma, proma_error) = match proma_root.map(proma_batch::Catalog::scan) {
            Some(Ok(catalog)) => (Some(catalog), None),
            Some(Err(error)) => (None, Some(format!("{error:#}"))),
            None => (None, None),
        };
        if let Some(catalog) = &proma {
            for project in &mut projects {
                if !project.archive_states.is_empty() {
                    project.archive_states.retain(|state| state.agent != crate::effective_skills::AgentKind::Proma);
                    project.archived = !project.archive_states.is_empty() && project.archive_states.iter().all(|state| state.archived == Some(true));
                }
            }
            for skill in &catalog.skills {
                if !skills.iter().any(|s| s.key == skill.key) {
                    skills.push(Skill { key: skill.key.clone(), name: skill.name.clone(), source: skill.source.clone(),
                        provider: skill.provider, install_name: None });
                }
            }
            // Keep workspace-only projects reachable, including archived ones.
            for entry in &catalog.workspaces {
                let w = &entry.workspace;
                let path = w.project_root_path.as_ref().unwrap_or(&w.path);
                if let Some(project) = projects.iter_mut().find(|p| &p.path == path) {
                    project.record_archive(crate::effective_skills::AgentKind::Proma, w.archived);
                    if !project.agents.contains(&crate::effective_skills::AgentKind::Proma) { project.agents.push(crate::effective_skills::AgentKind::Proma); }
                    if !project.proma_workspaces.contains(&w.path) { project.proma_workspaces.push(w.path.clone()); }
                } else {
                    projects.push(ProjectEntry { path: path.clone(), name: w.name.clone(), agents: vec![crate::effective_skills::AgentKind::Proma],
                        proma_workspaces: vec![w.path.clone()], native_workspaces: Vec::new(),
                        archive_states: vec![crate::project_discovery::NativeArchiveState { agent: crate::effective_skills::AgentKind::Proma, archived: Some(w.archived) }],
                        archived: w.archived, hidden: false, retained: false });
                }
            }
        }
        projects.sort_by_key(|p| (!p.is_active(), p.name.to_lowercase(), p.path.clone()));
        projects.dedup_by(|a, b| a.path == b.path);
        let mut links = BTreeMap::new();
        for project in &projects {
            let mut entries = Vec::new();
            for target in agents::PROJECT_INSTALL_TARGETS {
                let Ok(root) = agents::installation_root(&project.path, *target) else { continue; };
                let Ok(children) = fs::read_dir(&root) else { continue; };
                for entry in children.flatten() {
                    let path = entry.path();
                    if !path.join("SKILL.md").is_file() { continue; }
                    let key = proma_batch::source_key(&path.join("SKILL.md"));
                    if !skills.iter().any(|s| s.key == key) {
                        let name = crate::library::read_frontmatter(&path.join("SKILL.md")).ok()
                            .and_then(|(name, _)| name).unwrap_or_else(|| entry.file_name().to_string_lossy().into_owned());
                        skills.push(Skill { key: key.clone(), name, source: path.clone(), provider: "project", install_name: None });
                    }
                    entries.push(LinkEntry { target: *target, path, key, hermes_only: false });
                }
            }
            // Hermes resolves the selected folder to one nearest Git root;
            // its shared root can differ from the UI project folder.
            if let Some(home) = dirs::home_dir()
                && let Ok(Some(root)) = agents::hermes_project_root(&project.path, &home)
            {
                let shared = root.join(".agents/skills");
                if shared != project.path.join(".agents/skills")
                    && let Ok(children) = fs::read_dir(shared)
                {
                    for entry in children.flatten() {
                        let path = entry.path();
                        if !path.join("SKILL.md").is_file() { continue; }
                        let key = proma_batch::source_key(&path.join("SKILL.md"));
                        if !skills.iter().any(|s| s.key == key) {
                            skills.push(Skill { key: key.clone(), name: entry.file_name().to_string_lossy().into_owned(),
                                source: path.clone(), provider: "project", install_name: None });
                        }
                        entries.push(LinkEntry { target: InstallTarget::Universal, path, key, hermes_only: true });
                    }
                }
            }
            links.insert(project.path.clone(), entries);
        }
        skills.sort_by_key(|s| (s.name.to_lowercase(), s.key.clone()));
        Self { skills, projects, installed_agents, proma, proma_error, links }
    }

    pub fn proma_workspace_ids(&self, project: &Path) -> Vec<String> {
        self.proma.as_ref().map(|c| c.workspaces.iter().filter(|w| w.workspace.project_root_path.as_deref().unwrap_or(&w.workspace.path) == project)
            .map(|w| w.workspace.id.clone()).collect()).unwrap_or_default()
    }

    pub fn available_agents(&self) -> Vec<AgentIconInfo> {
        agents::project_agents(&self.installed_agents, &self.projects)
    }

    pub fn native_workspaces(&self) -> Vec<&crate::project_discovery::NativeWorkspace> {
        let mut seen = BTreeSet::new();
        self.projects.iter().flat_map(|project| &project.native_workspaces)
            .filter(|workspace| seen.insert(workspace.id())).collect()
    }

    pub fn workspace_ids(&self, project: &Path) -> Vec<String> {
        let mut ids = self.proma_workspace_ids(project);
        ids.extend(self.native_workspaces().into_iter().filter(|workspace| workspace.project_roots.iter().any(|root| root == project)).map(|workspace| workspace.id()));
        ids
    }

    pub fn plan(&self, action: Action, skills: &BTreeSet<String>, selected_agents: &BTreeSet<String>, projects: &BTreeSet<PathBuf>, workspaces: &BTreeSet<String>) -> Result<Plan> {
        ensure!(action.supported(selected_agents), "预设启停和软链接转换仅支持 Proma，请仅选择 Proma");
        let available = self.available_agents();
        let selected = available.iter().filter(|a| selected_agents.contains(a.id)).copied().collect::<Vec<_>>();
        ensure!(selected.len() == selected_agents.len() && selected.iter().all(|a| !a.global_only), "Agent 无关联项目且未检测到安装，或不支持项目技能，请重新选择");
        let native = self.native_workspaces();
        for id in workspaces.iter().filter(|id| id.starts_with("native:")) {
            ensure!(native.iter().any(|workspace| workspace.id() == *id), "原生工作区已变化，请刷新后重新选择");
        }
        let mut items = Vec::new();
        let mut instruction_pool = crate::host_instructions::RulePool::default();
        for project in &self.projects {
            for agent in selected.iter().filter(|a| a.id != "proma") {
                let mut workspace_paths = native.iter().filter(|workspace| workspace.agent.id() == agent.id
                    && workspaces.contains(&workspace.id()) && workspace.project_roots.contains(&project.path))
                    .map(|workspace| if agent.id == "hermes" { project.path.clone() } else { workspace.path.clone() }).collect::<Vec<_>>();
                if matches!(agent.id, "openclaw" | "hermes") { workspace_paths.sort(); workspace_paths.dedup(); }
                if !projects.contains(&project.path) && workspace_paths.is_empty() { continue; }
                for skill in self.skills.iter().filter(|s| skills.contains(&s.key)) {
                    if action == Action::Install {
                        let target = agents::INDEPENDENT_INSTALL_TARGETS.iter().find(|t| agent.owns_target(t.target))
                            .map(|t| t.target).unwrap_or(InstallTarget::Universal);
                        let name = skill.install_name.as_deref().unwrap_or(&skill.name);
                        let logical = agents::installation_root(&project.path, target)?.join(name);
                        let mut item = Item::new(skill, &project.name, &project.path, target, logical, &available);
                        item.native_workspace_paths = workspace_paths.clone();
                        if target == InstallTarget::OpenClaw {
                            item.openclaw_bindings = native.iter().filter(|workspace| workspace.agent.id() == "openclaw"
                                && workspace.project_roots.contains(&project.path)
                                && (projects.contains(&project.path) || workspaces.contains(&workspace.id())))
                                .filter_map(|workspace| workspace.source_id.as_ref().map(|id| (id.clone(), workspace.path.clone())))
                                .collect();
                        }
                        if agent.id == "hermes" { item.bind_hermes(project, workspaces, projects.contains(&project.path)); }
                        item.prepare_link(action, skill.install_name.is_some(), &mut instruction_pool);
                        items.push(item);
                    } else {
                        let matching = self.links.get(&project.path).into_iter().flatten()
                            .filter(|e| e.key == skill.key && agent.supports_target(e.target)
                                && (!e.hermes_only || agent.id == "hermes")
                                && (agent.id != "hermes" || hermes_owns_path(&project.path, &e.path))).collect::<Vec<_>>();
                        if matching.is_empty() {
                            let mut item = Item::new(skill, &project.name, &project.path, InstallTarget::Universal, project.path.clone(), &available);
                            item.native_workspace_paths = workspace_paths.clone();
                            item.affected_agents = vec![*agent]; item.disposition = Disposition::Missing;
                            items.push(item);
                        }
                        for entry in matching {
                            let mut item = Item::new(skill, &project.name, &project.path, entry.target, entry.path.clone(), &available);
                            item.native_workspace_paths = workspace_paths.clone();
                            if agent.id == "hermes" { item.bind_hermes(project, workspaces, projects.contains(&project.path)); }
                            item.prepare_link(action, skill.install_name.is_some(), &mut instruction_pool);
                            items.push(item);
                        }
                    }
                }
            }
        }
        if selected_agents.contains("proma") {
            ensure!(self.proma_error.is_none(), "Proma 无法扫描：{}", self.proma_error.as_deref().unwrap_or_default());
            if let Some(catalog) = &self.proma {
                for entry in catalog.workspaces.iter().filter(|w| workspaces.contains(&w.workspace.id)) {
                    for skill in self.skills.iter().filter(|s| skills.contains(&s.key)) {
                        let w = &entry.workspace;
                        let mut item = Item::new(skill, &w.name, &w.path, InstallTarget::Proma, w.path.join("skills").join(skill.install_name.as_deref().unwrap_or(&skill.name)), &available);
                        item.workspace = Some(w.clone());
                        item.operation = None;
                        let copies = entry.installations.iter().filter(|c| entry.identities.get(&c.path) == Some(&skill.key)).collect::<Vec<_>>();
                        if let Some(error) = &entry.error { item.block(Disposition::Unavailable, error); }
                        else if action.is_state() {
                            let plan = catalog.plan(&BTreeSet::from([skill.key.clone()]), &BTreeSet::from([w.id.clone()]), action == Action::Enable);
                            if let Some(first) = plan.items.first() {
                                item.disposition = match first.disposition {
                                    proma_batch::Disposition::Change => Disposition::Change,
                                    proma_batch::Disposition::Unchanged => Disposition::Unchanged,
                                    proma_batch::Disposition::Missing => Disposition::Missing,
                                    proma_batch::Disposition::Protected => Disposition::Protected,
                                    proma_batch::Disposition::Unavailable => Disposition::Unavailable,
                                };
                                item.reason = first.reason.clone();
                                if let Some(copy) = &first.installation { item.path = copy.path.clone(); }
                                item.operation = Some(Operation::Toggle(plan));
                            } else { item.disposition = Disposition::Missing; }
                        } else if skill.is_preset() { item.block(Disposition::Protected, "内置预设仅支持启用、停用；集成项由提供方维护"); }
                        else if copies.len() > 1 { item.block(Disposition::Protected, "工作区存在重复副本，请先检查"); }
                        else if let Some(copy) = copies.first() {
                            item.path = copy.path.clone();
                            if let Err(error) = proma::validate_previewed_installation(&catalog.root, w, copy) {
                                item.block(Disposition::Protected, &format!("{error:#}"));
                                items.push(item);
                                continue;
                            }
                            let linked = matches!(copy.status.as_str(), "external_link" | "broken_link");
                            if linked && action == Action::Remove {
                                match proma::plan_remove_link(&catalog.root, &w.path, &copy.path) {
                                    Ok(plan) if plan.workspace == *w => {
                                        item.operation = Some(Operation::RemovePromaLink(plan)); item.disposition = Disposition::Change;
                                        item.reason = Some("仅移除所选工作区的软链接，链接指向的源文件保留".into());
                                    }
                                    Ok(_) => item.block(Disposition::Protected, "工作区身份与扫描记录不一致，请刷新"),
                                    Err(error) => item.block(Disposition::Protected, &format!("{error:#}")),
                                }
                            } else if action == Action::Convert || (action == Action::Install && copy.state == "inactive" && copy.managed && copy.removable) {
                                if action == Action::Convert && !linked {
                                    item.disposition = Disposition::Unchanged; item.reason = Some("已是实体副本，无需转换".into());
                                } else if skill.install_name.is_none() {
                                    item.block(Disposition::Protected, "请先将链接来源纳入 Kitter 技能库，再转换为副本");
                                } else {
                                    match proma::plan_install(&catalog.root, &w.path, &skill.source, &copy.name) {
                                        Ok(plan) if plan.workspace != *w => item.block(Disposition::Protected, "工作区身份与扫描记录不一致，请刷新"),
                                        Ok(plan) if matches!(plan.action.as_str(), "migrate" | "reinstall") => {
                                            item.reason = Some(if linked && copy.state == "inactive" { "移除旧停用目录中的链接，在启用目录安装实体副本；源文件保留" }
                                                else if linked { "将旧软链接替换为工作区实体副本，源文件保留" } else { "移除旧停用副本，从 Kitter 重新安装到启用目录" }.into());
                                            item.operation = Some(Operation::InstallProma(plan)); item.disposition = Disposition::Change;
                                        }
                                        Ok(_) => item.block(Disposition::Protected, "安装位置已变化，请刷新后重新预览"),
                                        Err(error) => item.block(Disposition::Protected, &format!("{error:#}")),
                                    }
                                }
                            } else if linked {
                                item.block(Disposition::Protected, "这是旧软链接安装，请选择“转换为副本”或“从项目卸载”");
                            } else if action == Action::Install { item.disposition = Disposition::Unchanged; }
                            else if copy.managed && copy.removable && skill.install_name.is_some() {
                                item.operation = Some(Operation::RemoveProma { root: catalog.root.clone(), workspace: w.clone(), copy: (*copy).clone() });
                                item.disposition = Disposition::Change;
                                item.reason = Some("卸载此工作区的用户技能副本，Kitter 技能源与本机配置保留".into());
                            } else { item.block(Disposition::Protected, "内置、集成、未托管或已修改的副本保留"); }
                            if item.disposition == Disposition::Change
                                && let Err(error) = proma::validate_previewed_installation(&catalog.root, w, copy)
                            {
                                item.block(Disposition::Protected, &format!("{error:#}"));
                            }
                        } else if matches!(action, Action::Remove | Action::Convert) { item.disposition = Disposition::Missing; }
                        else if skill.install_name.is_none() { item.block(Disposition::Protected, "请先将技能纳入技能库，再安装到其他项目"); }
                        else {
                            match proma::plan_install(&catalog.root, &w.path, &skill.source, skill.install_name.as_deref().unwrap()) {
                                Ok(plan) if plan.workspace.id != w.id || plan.workspace.slug != w.slug || plan.workspace.path != w.path =>
                                    item.block(Disposition::Protected, "工作区身份在扫描后发生变化，请刷新后重新选择"),
                                Ok(plan) if plan.action == "create" => { item.operation = Some(Operation::InstallProma(plan)); item.disposition = Disposition::Change; }
                                Ok(_) => item.block(Disposition::Protected, "安装位置已有其他来源，请刷新后检查"),
                                Err(error) => item.block(Disposition::Protected, &format!("{error:#}")),
                            }
                        }
                        items.push(item);
                    }
                }
            }
        }
        // Alias directories and shared targets produce one physical operation.
        let mut unique: Vec<Item> = Vec::new();
        for mut item in items {
            if matches!(item.operation, Some(Operation::Link { .. })) && self.proma.as_ref().is_some_and(|c| c.workspaces.iter().any(|w| {
                item.path.starts_with(w.workspace.path.join("skills")) || item.path.starts_with(w.workspace.path.join("skills-inactive"))
            })) {
                item.block(Disposition::Protected, "安装路径与 Proma 实体副本目录重叠，请通过 Proma 工作区管理");
            }
            if let Some(existing) = unique.iter_mut().find(|e| e.path == item.path && e.skill.key == item.skill.key) {
                if let Some(route) = item.hermes_route.take() {
                    if existing.hermes_route.as_ref().is_some_and(|previous| previous.git_root != route.git_root || previous.profile != route.profile) {
                        existing.block(Disposition::Protected, "同一物理安装涉及不同 Hermes profile 或 Git 根，请分别预览");
                    }
                    if let Some(previous) = &mut existing.hermes_route {
                        for binding in route.bindings { if !previous.bindings.contains(&binding) { previous.bindings.push(binding); } }
                    } else { existing.hermes_route = Some(route); }
                }
                existing.openclaw_install |= item.openclaw_install;
                for binding in std::mem::take(&mut item.openclaw_bindings) { if !existing.openclaw_bindings.contains(&binding) { existing.openclaw_bindings.push(binding); } }
                if existing.operation_identity() != item.operation_identity() {
                    existing.block(Disposition::Protected, "同一安装路径涉及不同类型或工作区的操作，请分开选择");
                }
                if existing.disposition != Disposition::Protected && item.disposition == Disposition::Change { existing.disposition = Disposition::Change; }
                for instruction in item.instructions {
                    if let Some(previous) = existing.instructions.iter_mut().find(|plan| plan.path == instruction.path) {
                        if let Err(error) = previous.merge(instruction) { existing.block(Disposition::Protected, &format!("{error:#}")); }
                    } else {
                        existing.instruction_paths.push(instruction.path.clone());
                        existing.instructions.push(instruction);
                    }
                }
                for agent in item.affected_agents { if !existing.affected_agents.contains(&agent) { existing.affected_agents.push(agent); } }
                for path in item.native_workspace_paths { if !existing.native_workspace_paths.contains(&path) { existing.native_workspace_paths.push(path); } }
                if !existing.location.contains(&item.location) { existing.location.push_str(&format!(" / {}", item.location)); }
            } else { unique.push(item); }
        }
        let mut conflicts = BTreeMap::<PathBuf, BTreeSet<String>>::new();
        for item in &unique { if item.disposition == Disposition::Change { conflicts.entry(item.path.clone()).or_default().insert(item.skill.key.clone()); } }
        for item in &mut unique {
            if conflicts.get(&item.path).is_some_and(|s| s.len() > 1) { item.block(Disposition::Protected, "所选同名技能来源不同，安装位置冲突，请分开选择"); }
            item.affected_agents.sort_by_key(|a| a.name.to_lowercase());
        }
        Ok(Plan { action, items: unique })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition { Change, Unchanged, Missing, Protected, Unavailable }

#[derive(Clone, Debug)]
enum Operation {
    Link { logical: PathBuf, expected: Option<directory_link::DirectoryLink>, identity: Option<String> },
    InstallProma(proma::InstallPlan),
    RemoveProma { root: PathBuf, workspace: proma::Workspace, copy: proma::Installation },
    RemovePromaLink(proma::LinkRemovalPlan),
    Toggle(proma_batch::Plan),
}

#[derive(Clone, Debug)]
pub struct Item {
    pub skill: Skill,
    pub location: String,
    pub project: PathBuf,
    pub workspace: Option<proma::Workspace>,
    pub path: PathBuf,
    pub affected_agents: Vec<AgentIconInfo>,
    pub disposition: Disposition,
    pub reason: Option<String>,
    /// Native instruction files included in this operation's impact preview.
    pub instruction_paths: Vec<PathBuf>,
    /// Explicit workspace selections expand to these previewed member projects.
    pub native_workspace_paths: Vec<PathBuf>,
    instructions: Vec<crate::host_instructions::RulePlan>,
    operation: Option<Operation>,
    anchor: Option<(PathBuf, String)>,
    project_identity: Option<String>,
    openclaw_install: bool,
    openclaw_bindings: Vec<(String, PathBuf)>,
    hermes_route: Option<HermesRoute>,
}

#[derive(Clone, Debug)]
struct HermesRoute { profile: Option<PathBuf>, git_root: Option<PathBuf>, bindings: Vec<(String, PathBuf)> }

fn hermes_owns_path(project: &Path, path: &Path) -> bool {
    dirs::home_dir().and_then(|home| agents::hermes_project_root(project, &home).ok().flatten())
        .is_some_and(|root| path.parent().is_some_and(|parent| [root.join(".hermes/skills"), root.join(".agents/skills")]
            .iter().any(|candidate| crate::project_discovery::path_identity(candidate) == crate::project_discovery::path_identity(parent))))
}

impl Item {
    fn operation_identity(&self) -> (&'static str, Option<&str>) {
        match &self.operation {
            Some(Operation::Link { .. }) => ("link", None),
            Some(Operation::InstallProma(plan)) => ("proma-install", Some(&plan.workspace.id)),
            Some(Operation::RemoveProma { workspace, .. }) => ("proma-remove", Some(&workspace.id)),
            Some(Operation::RemovePromaLink(plan)) => ("proma-link-remove", Some(&plan.workspace.id)),
            Some(Operation::Toggle(plan)) => ("proma-toggle", plan.items.first().map(|i| i.workspace.id.as_str())),
            None => ("protected", None),
        }
    }
    pub fn operation_kind(&self, action: Action) -> &'static str {
        match &self.operation {
            Some(Operation::RemovePromaLink(_)) => "remove_link",
            Some(Operation::RemoveProma { .. }) => "remove_copy",
            Some(Operation::InstallProma(plan)) if plan.action == "migrate" => "convert_link",
            Some(Operation::InstallProma(plan)) if plan.action == "reinstall" => "reinstall_copy",
            Some(Operation::InstallProma(_)) => "install_copy",
            Some(Operation::Link { .. }) if action == Action::Remove => "remove_link",
            Some(Operation::Link { .. }) => "install_link",
            Some(Operation::Toggle(_)) if action == Action::Enable => "enable_preset",
            Some(Operation::Toggle(_)) => "disable_preset",
            None => "none",
        }
    }
    fn new(skill: &Skill, location: &str, project: &Path, target: InstallTarget, logical: PathBuf, candidates: &[AgentIconInfo]) -> Self {
        let physical = physical_entry(&logical).unwrap_or_else(|_| logical.clone());
        let mut affected_agents = candidates.iter().filter(|a| {
            if a.global_only { return false; }
            if a.id == "hermes" && !hermes_owns_path(project, &logical) { return false; }
            a.supports_target(target) || (target != InstallTarget::Proma && a.install_targets.iter().chain(a.shared_agents.then_some(&InstallTarget::Universal))
                .filter(|t| **t != InstallTarget::Proma).any(|t| agents::installation_root(project, *t).ok()
                    .and_then(|root| physical_entry(&root.join(logical.file_name().unwrap_or_default())).ok()).as_ref() == Some(&physical)))
        }).copied().collect::<Vec<_>>();
        affected_agents.sort_by_key(|a| a.name.to_lowercase());
        Self { skill: skill.clone(), location: location.into(), project: project.to_path_buf(), workspace: None,
            path: physical, affected_agents, disposition: Disposition::Protected, reason: None,
            instruction_paths: Vec::new(), native_workspace_paths: Vec::new(), instructions: Vec::new(),
            operation: Some(Operation::Link { logical, expected: None, identity: None }), anchor: None, project_identity: None,
            openclaw_install: target == InstallTarget::OpenClaw, openclaw_bindings: Vec::new(), hermes_route: None }
    }

    fn block(&mut self, disposition: Disposition, reason: &str) {
        self.disposition = disposition; self.reason = Some(reason.into()); self.operation = None;
    }

    fn bind_hermes(&mut self, project: &ProjectEntry, selected: &BTreeSet<String>, all: bool) {
        let bindings = project.native_workspaces.iter().filter(|w| w.agent == crate::effective_skills::AgentKind::Hermes
            && (all || selected.contains(&w.id())))
            .filter_map(|w| w.source_id.as_ref().map(|id| (id.clone(), w.path.clone()))).collect();
        let home = dirs::home_dir();
        self.hermes_route = Some(HermesRoute { profile: home.as_ref().and_then(|home| agents::hermes_home(home).ok()),
            git_root: home.as_ref().and_then(|home| agents::hermes_project_root(&self.project, home).ok().flatten()), bindings });
    }

    fn validate_hermes_route(&self, logical: &Path) -> Result<()> {
        let Some(route) = &self.hermes_route else { return Ok(()); };
        let home = dirs::home_dir().context("无法取得 Hermes 用户目录")?;
        ensure!(route.profile.as_ref() == Some(&agents::hermes_home(&home)?), "Hermes 当前 profile 已变化，请刷新后重新预览");
        ensure!(route.git_root == agents::hermes_project_root(&self.project, &home)?, "Hermes 当前 Git 根已变化，请刷新后重新预览");
        agents::hermes::validate_hermes_skill_root(&self.project,
            logical.parent().context("缺少 Hermes 安装目录")?)?;
        if !route.bindings.is_empty() {
            let current = crate::project_discovery::discover_agent_projects()
                .map_err(|_| anyhow::anyhow!("Hermes 原生项目来源不可读，归属未知"))?;
            ensure!(route.bindings.iter().all(|(id, source)| current.iter()
                .filter(|p| !p.retained && crate::project_discovery::path_identity(&p.path) == crate::project_discovery::path_identity(&self.project))
                .flat_map(|p| &p.native_workspaces).any(|w| w.agent == crate::effective_skills::AgentKind::Hermes
                    && w.source_id.as_ref() == Some(id) && &w.path == source
                    && w.project_roots.iter().any(|p| crate::project_discovery::path_identity(p) == crate::project_discovery::path_identity(&self.project)))),
                "Hermes 原生项目/文件夹归属变化或来源不可读，状态未知；请刷新后重新预览");
        }
        Ok(())
    }

    fn validate_openclaw_bindings(&self) -> Result<()> {
        if self.openclaw_bindings.is_empty() { return Ok(()); }
        let home = dirs::home_dir().context("无法取得 OpenClaw 用户目录")?;
        let snapshot = agents::openclaw_snapshot(&home)
            .map_err(|failure| anyhow::anyhow!("{}", failure.message()))?;
        ensure!(self.openclaw_bindings.iter().all(|(id, path)| snapshot.routes.iter().any(|(current_id, current_path)|
            id == current_id && crate::project_discovery::path_identity(path) == crate::project_discovery::path_identity(current_path))),
            "OpenClaw 原生 agentId/workspace 归属已变化，请刷新后重新预览");
        Ok(())
    }

    fn prepare_link(&mut self, action: Action, trusted: bool, instruction_pool: &mut crate::host_instructions::RulePool) {
        let result = (|| -> Result<()> {
            let logical = match &self.operation { Some(Operation::Link { logical, .. }) => logical.clone(), _ => unreachable!() };
            crate::library::validate_name(self.skill.install_name.as_deref().unwrap_or(&self.skill.name))?;
            ensure!(self.project.is_dir() && dirs::home_dir().is_none_or(|home| home.canonicalize().ok() != self.project.canonicalize().ok()), "请选择实际项目，全局安装不在此操作");
            self.path = physical_entry(&logical)?;
            self.validate_hermes_route(&logical)?;
            if self.openclaw_install && action == Action::Install {
                self.validate_openclaw_bindings()?;
                agents::validate_openclaw_link(logical.parent().context("缺少 OpenClaw 技能目录")?, &self.skill.source)?;
            }
            // Use the logical target before resolving aliases in skill roots.
            // Each host owns its native instruction entry even when Skill
            // installation directories resolve to the same physical link.
            if self.affected_agents.iter().any(|a| a.id == "claude-code") {
                let claude_link = agents::installation_root(&self.project, InstallTarget::ClaudeCode)?
                    .join(logical.file_name().context("缺少技能名称")?);
                self.instructions = crate::host_instructions::RulePlan::for_target(InstallTarget::ClaudeCode, &claude_link)?.into_iter().collect();
            }
            if self.affected_agents.iter().any(|a| a.id == "opencode") {
                self.instructions.push(crate::host_instructions::RulePlan::for_opencode(&self.project, &logical)?);
            }
            if self.affected_agents.iter().any(|a| a.id == "droid") {
                let droid_link = agents::installation_root(&self.project, InstallTarget::Droid)?
                    .join(logical.file_name().context("缺少技能名称")?);
                self.instructions.extend(crate::host_instructions::RulePlan::for_target(InstallTarget::Droid, &droid_link)?);
            }
            if self.affected_agents.iter().any(|a| a.id == "copilot") {
                if action == Action::Install {
                    self.instructions.push(crate::host_instructions::RulePlan::for_copilot(&self.project, &logical)?);
                } else {
                    self.instructions.extend(crate::host_instructions::RulePlan::copilot_removals(&self.project, &logical)?);
                }
            }
            for (host, id) in [(InstallTarget::Trae, "trae"), (InstallTarget::Antigravity, "antigravity"), (InstallTarget::WorkBuddy, "workbuddy"), (InstallTarget::Grok, "grok")] {
                if self.affected_agents.iter().any(|a| a.id == id) {
                    if action == Action::Install {
                        self.instructions.push(crate::host_instructions::RulePlan::for_file_host(&self.project, host, &logical)?);
                        if host == InstallTarget::Grok {
                            self.instructions.extend(crate::host_instructions::RulePlan::grok_hooks_for_install(
                                &self.project, &logical, &self.skill.source)?);
                        }
                    } else {
                        self.instructions.extend(crate::host_instructions::RulePlan::file_host_removals_for(&self.project, host, &logical)?);
                    }
                }
            }
            if self.affected_agents.iter().any(|a| a.id == "pi") {
                self.instructions.push(crate::host_instructions::RulePlan::for_pi(&self.project, &logical, &self.skill.source)?);
            }
            if self.affected_agents.iter().any(|a| a.id == "omp") {
                self.instructions.push(crate::host_instructions::RulePlan::for_omp(&self.project, &logical, &self.skill.source)?);
            }
            if self.affected_agents.iter().any(|a| a.id == "hermes") {
                self.instructions.push(crate::host_instructions::RulePlan::for_hermes(&logical, &self.skill.source, action == Action::Install)?);
            }
            if self.affected_agents.iter().any(|a| a.id == "openclaw") {
                self.instructions.push(crate::host_instructions::RulePlan::for_openclaw(&self.project, &logical, &self.skill.source, action == Action::Install)?);
            }
            self.instructions = crate::host_instructions::RulePlan::merge_all(std::mem::take(&mut self.instructions))?;
            #[cfg(not(unix))]
            ensure!(self.instructions.is_empty(), "此平台尚未支持原生适配受管写入，未修改安装入口");
            // Share each config snapshot immediately, before accumulating a
            // batch of skills that all refer to that same native document.
            for instruction in &mut self.instructions { instruction.coalesce(instruction_pool)?; }
            self.instruction_paths = self.instructions.iter().flat_map(|p| p.paths()).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
            self.project_identity = Some(entry_identity(&self.project.canonicalize()?)?);
            let mut anchor = self.path.parent().context("缺少安装父目录")?;
            while !anchor.exists() { anchor = anchor.parent().context("缺少项目目录")?; }
            self.anchor = Some((anchor.to_path_buf(), entry_identity(anchor)?));
            ensure!(trusted, "仅管理技能库中的安装链接；普通项目技能源文件保留");
            ensure!(self.skill.source.join("SKILL.md").canonicalize()? == Path::new(self.skill.key.strip_prefix("file:").context("技能源无效")?), "技能源与预览不一致");
            ensure!(!self.path.starts_with(self.skill.source.canonicalize()?), "安装位置位于技能源目录，已保护");
            let metadata = match self.path.symlink_metadata() {
                Ok(metadata) => Some(metadata),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            if metadata.is_none() {
                self.disposition = if action == Action::Install { Disposition::Change } else { Disposition::Missing };
                return Ok(());
            }
            let link = directory_link::inspect(&self.path)?.context("安装位置是实体目录，已保留现有文件")?;
            ensure!(self.path.canonicalize()? == self.skill.source.canonicalize()?, "同名安装指向其他来源，已保留");
            self.operation = Some(Operation::Link { logical, expected: Some(link), identity: Some(entry_identity(&self.path)?) });
            self.disposition = if action == Action::Install && self.instructions.iter().all(|p| !p.needs_install()) { Disposition::Unchanged } else { Disposition::Change };
            Ok(())
        })();
        if let Err(error) = result { self.block(Disposition::Protected, &format!("{error:#}")); }
    }

    fn apply(&mut self, action: Action) -> Result<bool> {
        if self.disposition != Disposition::Change { return Ok(false); }
        ensure!(matches!(self.operation, Some(Operation::RemovePromaLink(_))) || self.skill.install_name.is_none() || self.skill.source.join("SKILL.md").canonicalize()? == Path::new(self.skill.key.strip_prefix("file:").context("技能源身份无效")?), "技能源实际位置已变化，请刷新后检查");
        let operation = self.operation.clone().context("缺少预览操作")?;
        match &operation {
            Operation::InstallProma(plan) => { proma::apply_plan(plan)?; }
            Operation::RemoveProma { root, workspace, copy } => {
                let file = Path::new(self.skill.key.strip_prefix("file:").context("缺少受管来源")?);
                proma::uninstall_previewed_in(root, workspace, copy, file)?;
            }
            Operation::RemovePromaLink(plan) => { proma::apply_remove_link(plan)?; }
            Operation::Toggle(plan) => {
                let result = plan.execute();
                if let Some(error) = result.outcomes.iter().find_map(|o| o.error.as_ref()) { bail!("{error}"); }
                return Ok(result.changed() > 0);
            }
            Operation::Link { logical, expected, identity } => {
                self.validate_hermes_route(logical)?;
                ensure!(self.project_identity.as_deref() == Some(entry_identity(&self.project.canonicalize()?)?.as_str()), "项目目录身份已变化，请重新预览");
                let (anchor, anchor_identity) = self.anchor.as_ref().context("缺少安装目录预览身份")?;
                ensure!(&entry_identity(anchor)? == anchor_identity, "安装父目录身份已变化，请重新预览");
                ensure!(physical_entry(logical)? == self.path, "项目安装目录实际位置已变化，请重新预览");
                for instruction in &self.instructions { instruction.validate()?; }
                if action == Action::Install {
                    if self.openclaw_install {
                        self.validate_openclaw_bindings()?;
                        agents::validate_openclaw_link(logical.parent().context("缺少 OpenClaw 技能目录")?, &self.skill.source)?;
                    }
                    if expected.is_some() {
                        ensure!(directory_link::inspect(&self.path)?.as_ref() == expected.as_ref()
                            && identity.as_deref() == Some(entry_identity(&self.path)?.as_str())
                            && self.path.canonicalize()? == self.skill.source.canonicalize()?, "安装链接与预览不一致，已保留");
                    } else {
                        ensure!(self.path.symlink_metadata().is_err(), "安装位置已变化，请刷新后检查");
                        let source = self.skill.source.canonicalize()?;
                        install_link(&self.path, &source)?;
                        let created = directory_link::inspect(&self.path)?.context("缺少刚创建的链接")?;
                        ensure!(created.target == source, "新链接被替换，已保留当前对象");
                        self.operation = Some(Operation::Link { logical: logical.clone(), expected: Some(created), identity: Some(entry_identity(&self.path)?) });
                    }
                    #[cfg(test)]
                    BEFORE_LINK_INSTRUCTIONS.with(|hook| { if let Some(callback) = hook.borrow_mut().take() { callback(); } });
                    if self.instructions.iter().any(|instruction| instruction.binds_installation()) {
                        let current = match &self.operation {
                            Some(Operation::Link { expected, identity, .. }) => directory_link::inspect(&self.path)?.as_ref() == expected.as_ref()
                                && identity.as_deref() == Some(entry_identity(&self.path)?.as_str()),
                            _ => false,
                        };
                        ensure!(current, "Grok Hook 安装入口身份已变化，已保留");
                    }
                    for instruction in &mut self.instructions {
                        if let Err(error) = instruction.install() {
                            // Keep recorded progress for retry. Never adopt the
                            // identity of a concurrent replacement for rollback.
                            let unchanged = match &self.operation {
                                Some(Operation::Link { expected, identity, .. }) => directory_link::inspect(&self.path)?.as_ref() == expected.as_ref()
                                    && identity.as_deref() == Some(entry_identity(&self.path)?.as_str()),
                                _ => false,
                            };
                            ensure!(unchanged, "适配入口失败且安装链接归属已变化，当前链接保留：{error}");
                            return Err(error.context("安装链接保留，可重试原生适配入口"));
                        }
                    }
                } else {
                    ensure!(directory_link::inspect(&self.path)?.as_ref() == expected.as_ref()
                        && identity.as_deref() == Some(entry_identity(&self.path)?.as_str())
                        && self.path.canonicalize()? == self.skill.source.canonicalize()?, "安装链接与预览不一致，已保留");
                    // Recovery preparation can fail without changing the install.
                    // Complete it before removing any still-needed adapters.
                    let removal = prepare_link_removal(&self.path, expected.as_ref().context("缺少预览链接")?, identity.as_deref().context("缺少链接身份")?, &self.project, self.project_identity.as_deref().context("缺少项目身份")?)?;
                    for instruction in &mut self.instructions { instruction.detach()?; }
                    removal.commit()?;
                }
            }
        }
        Ok(true)
    }
}

fn entry_identity(path: &Path) -> Result<String> {
    crate::library::filesystem_identity(path)
}

fn install_link(path: &Path, source: &Path) -> Result<()> {
    #[cfg(unix)] {
        let parent = path.parent().context("缺少父目录")?;
        let mut prefix = parent;
        let mut missing = Vec::new();
        while !prefix.exists() {
            missing.push(prefix.file_name().context("缺少目录名称")?.to_owned());
            prefix = prefix.parent().context("缺少项目目录")?;
        }
        let mut directory = crate::package_fs::Directory::open_anchored(prefix)?;
        for part in missing.into_iter().rev() { directory.mkdir(Path::new(&part))?; directory = directory.child(Path::new(&part))?; }
        directory.verify_location(parent)?;
        directory.symlink(Path::new(path.file_name().context("缺少技能名称")?), source.to_str().context("技能源路径不是UTF-8")?, true)?;
    }
    #[cfg(not(unix))] { fs::create_dir_all(path.parent().context("缺少父目录")?)?; directory_link::create(source, path)?; }
    Ok(())
}

/// Move aside first; a replaced entry is restored or retained, never deleted.
pub(crate) fn remove_link(path: &Path, expected: &directory_link::DirectoryLink, identity: &str, project: &Path, project_identity: &str) -> Result<()> {
    prepare_link_removal(path, expected, identity, project, project_identity)?.commit()
}

struct PreparedLinkRemoval {
    path: PathBuf,
    expected: directory_link::DirectoryLink,
    identity: String,
    project: PathBuf,
    backup: PathBuf,
    backup_dir: crate::package_fs::Directory,
    raw_link: Option<PathBuf>,
    #[cfg(unix)]
    root: crate::package_fs::Directory,
    #[cfg(unix)]
    source_parent: crate::package_fs::Directory,
    #[cfg(not(unix))]
    backup_identity: String,
    #[cfg(not(unix))]
    project_identity: String,
}

fn prepare_link_removal(path: &Path, expected: &directory_link::DirectoryLink, identity: &str, project: &Path, project_identity: &str) -> Result<PreparedLinkRemoval> {
    use std::io::Write;
    use crate::package_fs::Directory;
    let project = project.canonicalize()?;
    #[cfg(unix)]
    let root = Directory::open_anchored(&project)?;
    #[cfg(unix)]
    ensure!(root.identity()? == project_identity, "项目目录身份已变化，未执行卸载");
    // Pi and DimAgent follow hidden directories inside skill roots, so recovery
    // links must live outside those roots too.
    #[cfg(unix)]
    let (backup_dir, backup) = {
        let temporary = tempfile::Builder::new().prefix(".kitter-removed-").make_in(&project, |path| {
            root.mkdir_exclusive(Path::new(path.file_name().unwrap())).map_err(std::io::Error::other)
        })?;
        let (directory, path) = temporary.into_parts();
        (directory, path.keep()?)
    };
    #[cfg(not(unix))]
    let (backup_dir, backup) = {
        ensure!(entry_identity(&project)? == project_identity, "项目目录身份已变化，未执行卸载");
        let path = tempfile::Builder::new().prefix(".kitter-removed-").tempdir_in(&project)?.keep();
        (Directory::open(&path)?, path)
    };
    #[cfg(not(unix))]
    let backup_identity = entry_identity(&backup)?;
    #[cfg(unix)]
    let source_parent = Directory::open_anchored(path.parent().context("缺少安装父目录")?)?;
    let raw_link = fs::read_link(path).ok();
    #[cfg(test)]
    BEFORE_RECOVERY_NOTE.with(|hook| { if let Some(callback) = hook.borrow_mut().take() { callback(&backup); } });
    // Prepare recovery metadata before the mutation. Failure leaves the install in place.
    #[cfg(unix)]
    { root.verify_location(&project)?; backup_dir.verify_location(&backup)?; }
    #[cfg(not(unix))]
    ensure!(entry_identity(&project)? == project_identity && entry_identity(&backup)? == backup_identity, "恢复目录身份已变化，未执行卸载");
    let mut note = backup_dir.create_file(Path::new("restore.json"), 0o600)?;
    note.write_all(&serde_json::to_vec_pretty(&serde_json::json!({"original":path,"entry":"installation","raw_target":raw_link}))?)?;
    note.sync_all()?;
    ensure!(directory_link::inspect(path)?.as_ref() == Some(expected) && entry_identity(path)? == identity,
        "安装对象在准备卸载时变化，已保留");
    Ok(PreparedLinkRemoval { path: path.to_path_buf(), expected: expected.clone(), identity: identity.to_owned(),
        project, backup, backup_dir, raw_link,
        #[cfg(unix)] root,
        #[cfg(unix)] source_parent,
        #[cfg(not(unix))] backup_identity,
        #[cfg(not(unix))] project_identity: project_identity.to_owned(),
    })
}

impl PreparedLinkRemoval {
    fn commit(self) -> Result<()> {
        let Self { path, expected, identity, project, backup, backup_dir, raw_link,
            #[cfg(unix)] root,
            #[cfg(unix)] source_parent,
            #[cfg(not(unix))] backup_identity,
            #[cfg(not(unix))] project_identity,
        } = self;
        let name = Path::new(path.file_name().context("缺少安装名称")?);
        let moved = backup.join("installation");
        #[cfg(unix)]
        { root.verify_location(&project)?; backup_dir.verify_location(&backup)?; source_parent.verify_location(path.parent().unwrap())?; }
        #[cfg(not(unix))]
        ensure!(entry_identity(&project)? == project_identity && entry_identity(&backup)? == backup_identity,
            "恢复目录身份已变化，未执行卸载");
        #[cfg(unix)]
        source_parent.rename_to(name, &backup_dir, Path::new("installation"))?;
        #[cfg(not(unix))]
        crate::package_fs::rename_exclusive(&path, &moved)?;
        let same_link = match &raw_link {
            Some(raw) => fs::read_link(&moved).is_ok_and(|current| &current == raw)
                && directory_link::inspect(&moved)?.is_some_and(|link| link.kind == expected.kind),
            None => directory_link::inspect(&moved)?.as_ref() == Some(&expected),
        };
        if !same_link || entry_identity(&moved)? != identity {
            #[cfg(unix)]
            let restored = backup_dir.rename_to(Path::new("installation"), &source_parent, name);
            #[cfg(not(unix))]
            let restored = crate::package_fs::rename_exclusive(&moved, &path);
            bail!("安装对象在执行时变化；{}", if restored.is_ok() { "已恢复原位置".into() } else { format!("保留于 {}", moved.display()) });
        }
        // Keep the detached link and a recovery note; the source package is untouched.
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    static BEFORE_RECOVERY_NOTE: std::cell::RefCell<Option<Box<dyn FnOnce(&Path)>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_LINK_INSTRUCTIONS: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[derive(Clone, Debug)]
pub struct Plan { pub action: Action, pub items: Vec<Item> }
impl Plan {
    pub fn count(&self, disposition: Disposition) -> usize { self.items.iter().filter(|i| i.disposition == disposition).count() }
    pub fn execute(&self) -> BatchResult {
        BatchResult { action: self.action, outcomes: self.items.iter().map(|original| {
            let mut item = original.clone();
            match item.apply(self.action) {
                Ok(changed) => Outcome { item, changed, error: None },
                Err(error) => Outcome { item, changed: false, error: Some(format!("{error:#}")) },
            }
        }).collect() }
    }
}

#[derive(Clone, Debug)]
pub struct Outcome { pub item: Item, pub changed: bool, pub error: Option<String> }
#[derive(Clone, Debug)]
pub struct BatchResult { pub action: Action, pub outcomes: Vec<Outcome> }
impl BatchResult {
    pub fn changed(&self) -> usize { self.outcomes.iter().filter(|o| o.changed).count() }
    pub fn failed(&self) -> usize { self.outcomes.iter().filter(|o| o.error.is_some()).count() }
    pub fn retry_plan(&self) -> Plan { Plan { action: self.action, items: self.outcomes.iter().filter(|o| o.error.is_some()).map(|o| o.item.clone()).collect() } }
    pub fn merge_retry(&mut self, retry: Self) {
        for outcome in retry.outcomes {
            if let Some(old) = self.outcomes.iter_mut().find(|o| o.item.path == outcome.item.path && o.item.skill.key == outcome.item.skill.key) { *old = outcome; }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Fixture { _temp: tempfile::TempDir, base: PathBuf, source: PathBuf, projects: Vec<ProjectEntry>, root: PathBuf }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap(); let base = temp.path().canonicalize().unwrap();
            let source = base.join("library/alpha"); fs::create_dir_all(&source).unwrap();
            fs::write(source.join("SKILL.md"), "---\nname: alpha\ndescription: Test\n---\nKeep source\n").unwrap();
            let projects = ["Project A", "Project B"].into_iter().map(|name| {
                let path = base.join(name); fs::create_dir(&path).unwrap();
                ProjectEntry { name: name.into(), path, agents: vec![], proma_workspaces: vec![], native_workspaces: Vec::new(), archive_states: Vec::new(), archived: false, hidden: false, retained: false }
            }).collect::<Vec<_>>();
            let root = base.join("proma");
            for slug in ["a", "b", "c"] { fs::create_dir_all(root.join("agent-workspaces").join(slug)).unwrap(); }
            fs::write(root.join("agent-workspaces.json"), serde_json::to_vec(&serde_json::json!({"version":2,"workspaces":[
                {"id":"a","slug":"a","name":"Workspace A","projectRootPath":projects[0].path},
                {"id":"b","slug":"b","name":"Workspace B","projectRootPath":projects[0].path},
                {"id":"c","slug":"c","name":"Workspace C","projectRootPath":projects[1].path}
            ]})).unwrap()).unwrap();
            Self { _temp: temp, base, source, projects, root }
        }
        fn skill(&self) -> Skill { Skill { key: proma_batch::source_key(&self.source.join("SKILL.md")), name:"alpha".into(), source:self.source.clone(), provider:"library", install_name:Some("alpha".into()) } }
        fn catalog(&self) -> Catalog {
            let installed = agents::AGENT_ICON_ORDER.iter().filter(|a| ["codex","cursor","proma","dimagent","trae"].contains(&a.id)).copied().collect();
            Catalog::scan(vec![self.skill()], self.projects.clone(), installed, Some(&self.root))
        }
        fn plan(&self, catalog: &Catalog, action: Action, ids: &[&str], workspaces: &[&str]) -> Plan {
            catalog.plan(action, &BTreeSet::from([self.skill().key]), &ids.iter().map(|id| id.to_string()).collect(),
                &self.projects.iter().map(|p| p.path.clone()).collect(), &workspaces.iter().map(|id| id.to_string()).collect()).unwrap()
        }
        fn file_host_install_plan(&self) -> Plan {
            let mut beta = self.skill();
            beta.source = self.base.join("library/beta");
            fs::create_dir_all(&beta.source).unwrap();
            fs::write(beta.source.join("SKILL.md"), "# Beta\n").unwrap();
            beta.key = proma_batch::source_key(&beta.source.join("SKILL.md"));
            beta.name = "beta".into(); beta.install_name = Some("beta".into());
            let skills = vec![self.skill(), beta];
            let selected_skills = skills.iter().map(|skill| skill.key.clone()).collect();
            let installed = agents::AGENT_ICON_ORDER.iter()
                .filter(|agent| ["antigravity", "trae"].contains(&agent.id)).copied().collect();
            Catalog::scan(skills, self.projects.clone(), installed, None).plan(Action::Install,
                &selected_skills, &BTreeSet::from(["antigravity".into(), "trae".into()]),
                &self.projects.iter().map(|project| project.path.clone()).collect(), &BTreeSet::new()).unwrap()
        }
    }

    #[test]
    fn batch_install_retry_recovers_only_absent_failed_slot() {
        let f = Fixture::new();
        let plan = f.file_host_install_plan();
        assert_eq!(plan.count(Disposition::Change), 8);
        let path = f.projects[0].path.join(".agent/skills/alpha");
        let reviewed = plan.items.iter().find(|item| item.path == path).unwrap();
        assert!(matches!(reviewed.operation, Some(Operation::Link { expected: None, .. })));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "preserve conflict\n").unwrap();
        let conflict_identity = entry_identity(&path).unwrap();
        let mut result = plan.execute();
        assert_eq!((result.changed(), result.failed()), (7, 1));
        let successes = result.outcomes.iter().filter(|outcome| outcome.changed).map(|outcome| {
            (outcome.item.path.clone(), entry_identity(&outcome.item.path).unwrap(), fs::read_link(&outcome.item.path).unwrap())
        }).collect::<Vec<_>>();
        let rules = result.outcomes.iter().filter(|outcome| outcome.changed)
            .flat_map(|outcome| &outcome.item.instruction_paths).map(|path| {
                (path.clone(), entry_identity(path).unwrap(), fs::read(path).unwrap())
            }).collect::<Vec<_>>();
        let retry = result.retry_plan();
        assert_eq!(retry.items.len(), 1);
        let item = &retry.items[0];
        assert_eq!(item.path, reviewed.path);
        assert_eq!(item.skill.key, reviewed.skill.key);
        assert_eq!(item.skill.source, reviewed.skill.source);
        assert_eq!(item.project, reviewed.project);
        assert_eq!(item.instruction_paths, reviewed.instruction_paths);
        assert_eq!(item.native_workspace_paths, reviewed.native_workspace_paths);
        assert_eq!(item.affected_agents, reviewed.affected_agents);
        assert_eq!(item.anchor, reviewed.anchor);
        assert_eq!(item.project_identity, reviewed.project_identity);
        assert!(matches!(item.operation, Some(Operation::Link { expected: None, .. })));
        let refused = retry.execute();
        assert_eq!((refused.changed(), refused.failed()), (0, 1));
        assert!(refused.outcomes[0].error.as_ref().unwrap().contains("安装位置已变化，请刷新后检查"));
        assert_eq!(entry_identity(&path).unwrap(), conflict_identity);
        result.merge_retry(refused);
        assert_eq!((result.changed(), result.failed()), (7, 1));
        let retained = f.base.join("retained-conflict");
        fs::rename(&path, &retained).unwrap();
        assert_eq!(path.symlink_metadata().unwrap_err().kind(), std::io::ErrorKind::NotFound);
        let retry = result.retry_plan();
        assert_eq!(retry.items.len(), 1);
        assert_eq!(retry.items[0].path, path);
        let retried = retry.execute();
        assert_eq!((retried.changed(), retried.failed()), (1, 0), "{retried:?}");
        result.merge_retry(retried);
        assert_eq!((result.changed(), result.failed()), (8, 0));
        assert!(result.retry_plan().items.is_empty());
        assert_eq!(fs::read_link(&path).unwrap(), f.source);
        assert_eq!(entry_identity(&retained).unwrap(), conflict_identity);
        assert_eq!(fs::read(&retained).unwrap(), b"preserve conflict\n");
        for (path, identity, target) in successes {
            assert_eq!(entry_identity(&path).unwrap(), identity);
            assert_eq!(fs::read_link(&path).unwrap(), target);
        }
        for (path, identity, bytes) in rules {
            assert_eq!(entry_identity(&path).unwrap(), identity);
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn batch_install_retry_refuses_redirected_source() {
        let f = Fixture::new();
        let plan = f.file_host_install_plan();
        let path = f.projects[0].path.join(".agent/skills/alpha");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "conflict").unwrap();
        let mut result = plan.execute();
        assert_eq!((result.changed(), result.failed()), (7, 1));
        fs::rename(&path, f.base.join("retained-conflict")).unwrap();
        fs::rename(&f.source, f.base.join("original-source")).unwrap();
        let replacement = f.base.join("replacement-source");
        fs::create_dir(&replacement).unwrap();
        fs::write(replacement.join("SKILL.md"), "# Another source\n").unwrap();
        symlink(&replacement, &f.source).unwrap();
        let retry = result.retry_plan();
        assert_eq!(retry.items.len(), 1);
        assert_eq!(retry.items[0].skill.key, plan.items.iter().find(|item| item.path == path).unwrap().skill.key);
        let refused = retry.execute();
        assert_eq!((refused.changed(), refused.failed()), (0, 1));
        assert!(refused.outcomes[0].error.as_ref().unwrap().contains("技能源实际位置已变化"));
        result.merge_retry(refused);
        assert_eq!((result.changed(), result.failed()), (7, 1));
        assert_eq!(path.symlink_metadata().unwrap_err().kind(), std::io::ErrorKind::NotFound);
        assert!(replacement.join("SKILL.md").is_file());
    }

    #[test]
    fn batch_install_retry_refuses_replaced_parent() {
        let f = Fixture::new();
        let parent = f.projects[0].path.join(".agent/skills");
        fs::create_dir_all(&parent).unwrap();
        let plan = f.file_host_install_plan();
        let path = parent.join("alpha");
        fs::write(&path, "conflict").unwrap();
        let result = plan.execute();
        assert_eq!((result.changed(), result.failed()), (7, 1));
        fs::rename(&path, f.base.join("retained-conflict")).unwrap();
        let retained = f.base.join("original-skills");
        fs::rename(&parent, &retained).unwrap();
        let success_identity = entry_identity(&retained.join("beta")).unwrap();
        fs::create_dir(&parent).unwrap();
        let retry = result.retry_plan();
        assert_eq!(retry.items.len(), 1);
        let refused = retry.execute();
        assert_eq!((refused.changed(), refused.failed()), (0, 1));
        assert!(refused.outcomes[0].error.as_ref().unwrap().contains("安装父目录身份已变化"));
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 0);
        assert_eq!(entry_identity(&retained.join("beta")).unwrap(), success_identity);
        assert_eq!(fs::read_link(retained.join("beta")).unwrap(), f.base.join("library/beta"));
    }

    #[test]
    fn batch_install_retry_refuses_redirected_parent() {
        let f = Fixture::new();
        let plan = f.file_host_install_plan();
        let config = f.projects[0].path.join(".agent");
        let path = config.join("skills/alpha");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "conflict").unwrap();
        let result = plan.execute();
        assert_eq!((result.changed(), result.failed()), (7, 1));
        fs::rename(&path, f.base.join("retained-conflict")).unwrap();
        fs::rename(&config, f.base.join("original-agent")).unwrap();
        let replacement = f.base.join("redirected-agent");
        fs::create_dir(&replacement).unwrap();
        symlink(&replacement, &config).unwrap();
        let retry = result.retry_plan();
        assert_eq!(retry.items.len(), 1);
        let refused = retry.execute();
        assert_eq!((refused.changed(), refused.failed()), (0, 1));
        assert!(refused.outcomes[0].error.as_ref().unwrap().contains("项目安装目录实际位置已变化"));
        assert_eq!(fs::read_dir(&replacement).unwrap().count(), 0);
    }

    #[test]
    fn native_owner_remains_manageable_without_detected_program() {
        use crate::effective_skills::AgentKind;
        let f = Fixture::new();
        let registry = f.base.join("runner.json");
        fs::write(&registry, "{}\n").unwrap();
        let workspace = crate::project_discovery::NativeWorkspace {
            agent: AgentKind::Amp, source_id: Some("owned-runner".into()),
            display_name: None, archived: None, path: registry.clone(),
            project_roots: std::sync::Arc::new(f.projects.iter().map(|entry| entry.path.clone()).collect()),
        };
        let mut projects = f.projects.clone();
        for project in &mut projects {
            project.agents.push(AgentKind::Amp);
            project.native_workspaces.push(workspace.clone());
        }
        let catalog = Catalog::scan(vec![f.skill()], projects.clone(), Vec::new(), None);
        let plan = catalog.plan(Action::Install, &BTreeSet::from([f.skill().key]),
            &BTreeSet::from(["amp".into()]), &BTreeSet::new(), &BTreeSet::from([workspace.id()])).unwrap();
        assert_eq!(plan.count(Disposition::Change), 2);
        let expected = projects.iter().map(|entry| entry.path.join(".agents/skills/alpha")).collect::<BTreeSet<_>>();
        assert_eq!(plan.items.iter().map(|item| item.path.clone()).collect::<BTreeSet<_>>(), expected);
        assert!(plan.items.iter().all(|item| item.affected_agents.iter().any(|agent| agent.id == "amp")));
        assert_eq!(fs::read(&registry).unwrap(), b"{}\n");
    }

    #[test]
    fn droid_batch_adaptation_retry_and_uninstall_preserve_user_instructions() {
        let f = Fixture::new();
        let mut beta = f.skill();
        beta.source = f.base.join("library/beta");
        fs::create_dir_all(&beta.source).unwrap();
        fs::write(beta.source.join("SKILL.md"), "# Beta\n").unwrap();
        beta.key = proma_batch::source_key(&beta.source.join("SKILL.md"));
        beta.name = "beta".into(); beta.install_name = Some("beta".into());
        let skills = vec![f.skill(), beta];
        for skill in &skills {
            fs::create_dir_all(skill.source.join("local-data")).unwrap();
            fs::write(skill.source.join("local-data/ADAPTATION.md"), "PRIVATE_ADAPTATION\n").unwrap();
        }
        for project in &f.projects {
            fs::create_dir_all(project.path.join(".factory/skills")).unwrap();
            fs::write(project.path.join("AGENTS.md"), "USER_INSTRUCTIONS\r\n").unwrap();
        }
        let selected_skills = skills.iter().map(|skill| skill.key.clone()).collect();
        let selected_agents = BTreeSet::from(["droid".into()]);
        let projects = f.projects.iter().map(|project| project.path.clone()).collect();
        let installed = agents::AGENT_ICON_ORDER.iter().filter(|agent| agent.id == "droid").copied().collect::<Vec<_>>();
        let catalog = Catalog::scan(skills.clone(), f.projects.clone(), installed.clone(), None);
        let plan = catalog.plan(Action::Install, &selected_skills, &selected_agents, &projects, &BTreeSet::new()).unwrap();
        assert_eq!(plan.count(Disposition::Change), 4);
        for item in &plan.items { assert_eq!(item.instruction_paths, vec![item.project.join("AGENTS.md")]); }
        let conflict = f.projects[1].path.join(".factory/skills/alpha");
        fs::write(&conflict, "PRESERVE_CONFLICT\n").unwrap();
        let mut result = plan.execute();
        assert_eq!((result.changed(), result.failed()), (3, 1), "{result:?}");
        let successes = result.outcomes.iter().filter(|outcome| outcome.changed)
            .map(|outcome| (outcome.item.path.clone(), entry_identity(&outcome.item.path).unwrap())).collect::<Vec<_>>();
        let retained = f.base.join("retained-droid-conflict");
        fs::rename(&conflict, &retained).unwrap();
        let retry = result.retry_plan();
        assert_eq!(retry.items.len(), 1);
        result.merge_retry(retry.execute());
        assert_eq!((result.changed(), result.failed()), (4, 0), "{result:?}");
        for (path, identity) in successes { assert_eq!(entry_identity(&path).unwrap(), identity); }
        for project in &f.projects {
            let text = fs::read_to_string(project.path.join("AGENTS.md")).unwrap();
            assert!(text.starts_with("USER_INSTRUCTIONS\r\n"));
            assert!(text.contains("ADAPTATION.md"));
            assert!(!text.contains("PRIVATE_ADAPTATION"));
            for skill in &skills {
                assert!(text.contains(skill.name.as_str()));
                assert_eq!(fs::read_link(project.path.join(".factory/skills").join(&skill.name)).unwrap(), skill.source);
            }
        }
        let catalog = Catalog::scan(skills.clone(), f.projects.clone(), installed, None);
        let remove = catalog.plan(Action::Remove, &selected_skills, &selected_agents, &projects, &BTreeSet::new()).unwrap().execute();
        assert_eq!((remove.changed(), remove.failed()), (4, 0), "{remove:?}");
        for project in &f.projects {
            assert_eq!(fs::read(project.path.join("AGENTS.md")).unwrap(), b"USER_INSTRUCTIONS\r\n");
            for skill in &skills { assert!(project.path.join(".factory/skills").join(&skill.name).symlink_metadata().is_err()); }
        }
        assert_eq!(fs::read(retained).unwrap(), b"PRESERVE_CONFLICT\n");
        for skill in &skills { assert_eq!(fs::read(skill.source.join("local-data/ADAPTATION.md")).unwrap(), b"PRIVATE_ADAPTATION\n"); }
    }

    #[test]
    fn droid_batch_shared_directory_uses_droid_instruction_path() {
        for droid_is_alias in [false, true] {
            for ids in [vec!["cursor"], vec!["droid"], vec!["cursor", "droid"]] {
                let f = Fixture::new();
                fs::create_dir_all(f.source.join("local-data")).unwrap();
                fs::write(f.source.join("local-data/ADAPTATION.md"), "LOCAL_ADAPTATION\n").unwrap();
                for project in &f.projects {
                    let droid = project.path.join(".factory/skills");
                    let cursor = project.path.join(".cursor/skills");
                    let (physical, alias) = if droid_is_alias { (&cursor, &droid) } else { (&droid, &cursor) };
                    fs::create_dir_all(physical).unwrap();
                    fs::create_dir_all(alias.parent().unwrap()).unwrap();
                    symlink(physical, alias).unwrap();
                    fs::write(project.path.join("AGENTS.md"), "USER_INSTRUCTIONS\r\n").unwrap();
                }
                let installed = agents::AGENT_ICON_ORDER.iter().filter(|agent| ["cursor", "droid"].contains(&agent.id)).copied().collect::<Vec<_>>();
                let catalog = Catalog::scan(vec![f.skill()], f.projects.clone(), installed.clone(), None);
                let plan = f.plan(&catalog, Action::Install, &ids, &[]);
                assert_eq!(plan.count(Disposition::Change), 2, "{droid_is_alias} {ids:?}: {plan:?}");
                for item in &plan.items { assert!(item.instruction_paths.contains(&item.project.join("AGENTS.md"))); }
                let result = plan.execute();
                assert_eq!((result.changed(), result.failed()), (2, 0), "{result:?}");
                for project in &f.projects {
                    let text = fs::read_to_string(project.path.join("AGENTS.md")).unwrap();
                    assert!(text.starts_with("USER_INSTRUCTIONS\r\n"));
                    assert!(text.contains(".factory/skills/alpha"));
                    assert!(!text.contains(".cursor/skills/alpha"));
                }
                let catalog = Catalog::scan(vec![f.skill()], f.projects.clone(), installed, None);
                let result = f.plan(&catalog, Action::Remove, &ids, &[]).execute();
                assert_eq!((result.changed(), result.failed()), (2, 0), "{result:?}");
                for project in &f.projects {
                    assert_eq!(fs::read(project.path.join("AGENTS.md")).unwrap(), b"USER_INSTRUCTIONS\r\n");
                    assert!(project.path.join(".factory/skills/alpha").symlink_metadata().is_err());
                }
                assert_eq!(fs::read(f.source.join("local-data/ADAPTATION.md")).unwrap(), b"LOCAL_ADAPTATION\n");
            }
        }
    }

    #[test]
    fn copilot_batch_aliases_keep_one_native_instruction_file_per_project() {
        let f = Fixture::new();
        let mut beta = f.skill();
        beta.source = f.base.join("library/beta");
        fs::create_dir_all(&beta.source).unwrap();
        fs::write(beta.source.join("SKILL.md"), "# Beta\n").unwrap();
        beta.key = proma_batch::source_key(&beta.source.join("SKILL.md"));
        beta.name = "beta".into(); beta.install_name = Some("beta".into());
        let skills = vec![f.skill(), beta];
        for skill in &skills {
            fs::create_dir_all(skill.source.join("local-data")).unwrap();
            fs::write(skill.source.join("local-data/ADAPTATION.md"), "LOCAL_CANARY\n").unwrap();
        }
        for project in &f.projects {
            fs::create_dir_all(project.path.join(".agents/skills")).unwrap();
            fs::create_dir_all(project.path.join(".github")).unwrap();
            std::os::unix::fs::symlink(project.path.join(".agents/skills"), project.path.join(".github/skills")).unwrap();
            fs::write(project.path.join(".github/copilot-instructions.md"), "USER_INSTRUCTIONS\r\n").unwrap();
        }
        let selected_skills = skills.iter().map(|skill| skill.key.clone()).collect();
        let selected_agents = BTreeSet::from(["copilot".into()]);
        let projects = f.projects.iter().map(|project| project.path.clone()).collect();
        let installed = agents::AGENT_ICON_ORDER.iter().filter(|agent| agent.id == "copilot").copied().collect::<Vec<_>>();
        let catalog = Catalog::scan(skills.clone(), f.projects.clone(), installed.clone(), None);
        let plan = catalog.plan(Action::Install, &selected_skills, &selected_agents, &projects, &BTreeSet::new()).unwrap();
        assert_eq!(plan.count(Disposition::Change), 4);
        for item in &plan.items {
            assert_eq!(item.instruction_paths, vec![item.project.join(".github/copilot-instructions.md")]);
        }
        let result = plan.execute();
        assert_eq!((result.changed(), result.failed()), (4, 0));
        for project in &f.projects {
            let text = fs::read_to_string(project.path.join(".github/copilot-instructions.md")).unwrap();
            assert!(text.starts_with("USER_INSTRUCTIONS\r\n"));
            assert!(!text.contains("LOCAL_CANARY"));
            for skill in &skills {
                assert_eq!(fs::read_link(project.path.join(".agents/skills").join(&skill.name)).unwrap(), skill.source);
            }
        }
        let catalog = Catalog::scan(skills.clone(), f.projects.clone(), installed, None);
        let remove = catalog.plan(Action::Remove, &selected_skills, &selected_agents, &projects, &BTreeSet::new()).unwrap().execute();
        assert_eq!((remove.changed(), remove.failed()), (4, 0));
        for project in &f.projects {
            assert_eq!(fs::read(project.path.join(".github/copilot-instructions.md")).unwrap(), b"USER_INSTRUCTIONS\r\n");
            for skill in &skills { assert!(project.path.join(".agents/skills").join(&skill.name).symlink_metadata().is_err()); }
        }
        for skill in &skills { assert_eq!(fs::read(skill.source.join("local-data/ADAPTATION.md")).unwrap(), b"LOCAL_CANARY\n"); }
    }

    #[test]
    fn grok_batch_preview_and_public_execution_manage_native_and_shared_hooks() {
        let f = Fixture::new();
        fs::create_dir_all(f.source.join("local-data")).unwrap();
        fs::write(f.source.join("local-data/ADAPTATION.md"), "KEEP_LOCAL").unwrap();
        let installed = agents::AGENT_ICON_ORDER.iter()
            .filter(|agent| ["dimagent", "grok"].contains(&agent.id)).copied().collect::<Vec<_>>();
        let catalog = Catalog::scan(vec![f.skill()], f.projects.clone(), installed.clone(), None);
        let install = f.plan(&catalog, Action::Install, &["dimagent", "grok"], &[]);
        assert_eq!(install.count(Disposition::Change), 4);
        let paths = install.items.iter().flat_map(|item| {
            assert!(item.affected_agents.iter().any(|agent| agent.id == "grok"));
            assert!(item.instruction_paths.iter().any(|path| path.parent().unwrap().ends_with(".grok/hooks")));
            assert!(item.instruction_paths.iter().any(|path| path.parent().unwrap().ends_with(".grok/rules")));
            item.instruction_paths.iter().cloned()
        }).collect::<BTreeSet<_>>();
        assert_eq!(paths.len(), 8);
        assert!(paths.iter().all(|path| !path.exists()));
        let result = install.execute();
        assert_eq!((result.changed(), result.failed()), (4, 0));
        assert!(paths.iter().all(|path| path.is_file()));
        let before = paths.iter().map(|path| (path.clone(), entry_identity(path).unwrap())).collect::<Vec<_>>();
        let catalog = Catalog::scan(vec![f.skill()], f.projects.clone(), installed.clone(), None);
        let repeated = f.plan(&catalog, Action::Install, &["dimagent", "grok"], &[]);
        assert_eq!(repeated.count(Disposition::Unchanged), 4);
        assert_eq!(repeated.execute().failed(), 0);
        for (path, identity) in before { assert_eq!(entry_identity(&path).unwrap(), identity); }
        let catalog = Catalog::scan(vec![f.skill()], f.projects.clone(), installed, None);
        let remove = f.plan(&catalog, Action::Remove, &["dimagent", "grok"], &[]);
        assert_eq!(remove.items.iter().flat_map(|item| item.instruction_paths.iter().cloned()).collect::<BTreeSet<_>>(), paths);
        let result = remove.execute();
        assert_eq!((result.changed(), result.failed()), (4, 0));
        assert!(paths.iter().all(|path| !path.exists()));
        assert_eq!(fs::read(f.source.join("local-data/ADAPTATION.md")).unwrap(), b"KEEP_LOCAL");
    }

    #[test]
    fn grok_batch_only_failed_retry_preserves_user_hook_and_successes() {
        let f = Fixture::new();
        let installed = agents::AGENT_ICON_ORDER.iter().filter(|agent| agent.id == "grok").copied().collect();
        let catalog = Catalog::scan(vec![f.skill()], f.projects.clone(), installed, None);
        let install = f.plan(&catalog, Action::Install, &["grok"], &[]);
        assert_eq!(install.count(Disposition::Change), 2);
        let conflict = install.items[0].instruction_paths.iter()
            .find(|path| path.parent().unwrap().ends_with("hooks")).unwrap().clone();
        let conflict_path = conflict.clone();
        BEFORE_LINK_INSTRUCTIONS.with(|callback| *callback.borrow_mut() = Some(Box::new(move || {
            fs::create_dir_all(conflict_path.parent().unwrap()).unwrap();
            fs::write(conflict_path, "USER_HOOK").unwrap();
        })));
        let mut result = install.execute();
        assert_eq!((result.changed(), result.failed()), (1, 1));
        let success = result.outcomes.iter().find(|outcome| outcome.error.is_none()).unwrap();
        let success_link = (success.item.path.clone(), entry_identity(&success.item.path).unwrap());
        let success_files = success.item.instruction_paths.iter().map(|path|
            (path.clone(), entry_identity(path).unwrap(), fs::read(path).unwrap())).collect::<Vec<_>>();
        let retry = result.retry_plan(); assert_eq!(retry.items.len(), 1);
        assert_eq!(retry.execute().failed(), 1);
        assert_eq!(fs::read(&conflict).unwrap(), b"USER_HOOK");
        fs::rename(&conflict, f.base.join("retained-user-hook")).unwrap();
        result.merge_retry(result.retry_plan().execute());
        assert_eq!((result.changed(), result.failed()), (2, 0));
        assert!(result.retry_plan().items.is_empty());
        assert_eq!(entry_identity(&success_link.0).unwrap(), success_link.1);
        for (path, identity, bytes) in success_files {
            assert_eq!(entry_identity(&path).unwrap(), identity);
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
        assert_eq!(fs::read(f.base.join("retained-user-hook")).unwrap(), b"USER_HOOK");
    }

    #[test]
    fn batch_remove_recovery_preparation_failure_keeps_adapters_and_retries_only_failure() {
        let f = Fixture::new();
        fs::create_dir_all(f.source.join("local-data")).unwrap();
        fs::write(f.source.join("local-data/ADAPTATION.md"), b"KEEP_LOCAL\n").unwrap();
        let installed = agents::AGENT_ICON_ORDER.iter()
            .filter(|agent| ["dimagent", "grok"].contains(&agent.id)).copied().collect::<Vec<_>>();
        let catalog = Catalog::scan(vec![f.skill()], f.projects.clone(), installed.clone(), None);
        let initial = f.plan(&catalog, Action::Install, &["dimagent"], &[]).execute();
        assert_eq!((initial.changed(), initial.failed()), (2, 0));
        let original = initial.outcomes.iter().map(|outcome| {
            assert_eq!(outcome.item.instruction_paths.len(), 2);
            (outcome.item.path.clone(), (entry_identity(&outcome.item.path).unwrap(),
                outcome.item.instruction_paths.iter().map(|path|
                    (path.clone(), entry_identity(path).unwrap(), fs::read(path).unwrap())).collect::<Vec<_>>()))
        }).collect::<BTreeMap<_, _>>();
        let catalog = Catalog::scan(vec![f.skill()], f.projects.clone(), installed, None);
        let remove = f.plan(&catalog, Action::Remove, &["dimagent"], &[]);
        BEFORE_RECOVERY_NOTE.with(|hook| *hook.borrow_mut() = Some(Box::new(|backup| {
            // The opened recovery directory no longer owns its published name.
            // Preparing its note must fail before any adapter is detached.
            fs::rename(backup, backup.with_extension("retained")).unwrap();
            fs::create_dir(backup).unwrap();
        })));
        let mut result = remove.execute();
        assert_eq!((result.changed(), result.failed()), (1, 1));
        let failed = result.outcomes.iter().find(|outcome| outcome.error.is_some()).unwrap();
        let (link_identity, entries) = &original[&failed.item.path];
        assert_eq!(&entry_identity(&failed.item.path).unwrap(), link_identity);
        for (path, identity, bytes) in entries {
            assert_eq!(&entry_identity(path).unwrap(), identity);
            assert_eq!(&fs::read(path).unwrap(), bytes);
        }
        let succeeded = result.outcomes.iter().find(|outcome| outcome.error.is_none()).unwrap();
        assert!(succeeded.item.path.symlink_metadata().is_err());
        let successful_recovery = fs::read_dir(&succeeded.item.project).unwrap()
            .map(|entry| entry.unwrap().path()).find(|path| path.join("installation").is_symlink()).unwrap();
        let recovery_identity = entry_identity(&successful_recovery.join("installation")).unwrap();
        assert_eq!(result.retry_plan().items.len(), 1);
        result.merge_retry(result.retry_plan().execute());
        assert_eq!((result.changed(), result.failed()), (2, 0));
        assert_eq!(entry_identity(&successful_recovery.join("installation")).unwrap(), recovery_identity);
        assert_eq!(fs::read(f.source.join("local-data/ADAPTATION.md")).unwrap(), b"KEEP_LOCAL\n");
    }

    #[test]
    fn grok_batch_old_reader_detach_sync_failure_restores_and_retries_only_failure() {
        let f = Fixture::new();
        let installed = agents::AGENT_ICON_ORDER.iter().filter(|agent| agent.id == "grok").copied().collect::<Vec<_>>();
        let catalog = Catalog::scan(vec![f.skill()], f.projects.clone(), installed.clone(), None);
        let initial = f.plan(&catalog, Action::Install, &["grok"], &[]).execute();
        assert_eq!((initial.changed(), initial.failed()), (2, 0));
        let old = initial.outcomes.iter().map(|outcome| {
            let path = outcome.item.instruction_paths.iter().find(|path| path.parent().unwrap().ends_with("hooks")).unwrap().clone();
            let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            let reader = f.base.join("retired-reader");
            let handler = value.pointer_mut("/hooks/PostToolUse/0/hooks/0").unwrap();
            let id = handler["env"]["PLEAT_GROK_SOURCE_ID"].as_str().unwrap().to_owned();
            let installation = handler["env"]["PLEAT_GROK_INSTALLATION"].as_str().unwrap().to_owned();
            handler["env"]["PLEAT_GROK_READER"] = serde_json::json!(reader);
            handler["command"] = serde_json::json!(format!("'{}' --pleat-native-grok-adaptation-v1 --installation '{}' --source-identity '{}'", reader.display(), installation, id));
            value.sort_all_objects();
            let bytes = format!("{}\n", serde_json::to_string(&value).unwrap()).into_bytes();
            fs::write(&path, &bytes).unwrap(); (path, bytes)
        }).collect::<BTreeMap<_, _>>();
        let catalog = Catalog::scan(vec![f.skill()], f.projects.clone(), installed, None);
        let upgrade = f.plan(&catalog, Action::Install, &["grok"], &[]);
        assert_eq!(upgrade.count(Disposition::Change), 2);
        crate::host_instructions::BEFORE_DETACH_SYNC.with(|callback| *callback.borrow_mut() = Some(Box::new(|| {
            anyhow::bail!("injected batch detach durability failure")
        })));
        let mut result = upgrade.execute();
        assert_eq!((result.changed(), result.failed()), (1, 1));
        let failure = result.outcomes.iter().find(|outcome| outcome.error.is_some()).unwrap();
        let failed_path = failure.item.instruction_paths.iter().find(|path| path.parent().unwrap().ends_with("hooks")).unwrap();
        assert_eq!(fs::read(failed_path).unwrap(), old[failed_path]);
        let success = result.outcomes.iter().find(|outcome| outcome.error.is_none()).unwrap();
        let unchanged = success.item.instruction_paths.iter().map(|path|
            (path.clone(), entry_identity(path).unwrap(), fs::read(path).unwrap())).collect::<Vec<_>>();
        assert_eq!(result.retry_plan().items.len(), 1);
        result.merge_retry(result.retry_plan().execute());
        assert_eq!((result.changed(), result.failed()), (2, 0));
        for (path, identity, bytes) in unchanged {
            assert_eq!(entry_identity(&path).unwrap(), identity);
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
    }

    #[test]
    fn grok_batch_replaced_new_slot_is_preserved_without_hook_publication() {
        let f = Fixture::new();
        let installed = agents::AGENT_ICON_ORDER.iter().filter(|agent| agent.id == "grok").copied().collect();
        let catalog = Catalog::scan(vec![f.skill()], f.projects.clone(), installed, None);
        let install = f.plan(&catalog, Action::Install, &["grok"], &[]);
        let slot = install.items[0].path.clone(); let replacement = slot.clone();
        let source = f.source.clone(); let retained = f.base.join("retained-slot");
        BEFORE_LINK_INSTRUCTIONS.with(|callback| *callback.borrow_mut() = Some(Box::new(move || {
            fs::rename(&replacement, retained).unwrap(); symlink(source, replacement).unwrap();
        })));
        let result = install.execute();
        assert_eq!((result.changed(), result.failed()), (1, 1));
        let failed = result.outcomes.iter().find(|outcome| outcome.error.is_some()).unwrap();
        assert!(failed.item.instruction_paths.iter().all(|path| !path.exists()));
        assert_eq!(slot.canonicalize().unwrap(), f.source);
        assert_eq!(result.retry_plan().execute().failed(), 1);
    }

    #[test]
    fn grok_batch_source_replacement_after_preview_never_publishes_hook() {
        let f = Fixture::new();
        let installed = agents::AGENT_ICON_ORDER.iter().filter(|agent| agent.id == "grok").copied().collect();
        let catalog = Catalog::scan(vec![f.skill()], f.projects.clone(), installed, None);
        let install = f.plan(&catalog, Action::Install, &["grok"], &[]);
        fs::rename(&f.source, f.base.join("retained-source")).unwrap();
        fs::create_dir(&f.source).unwrap();
        fs::write(f.source.join("SKILL.md"), "OTHER_SOURCE").unwrap();
        let result = install.execute();
        assert_eq!(result.failed(), 2);
        assert!(install.items.iter().flat_map(|item| &item.instruction_paths).all(|path| !path.exists()));
        assert!(f.base.join("retained-source/SKILL.md").is_file());
    }

    #[test]
    fn workbuddy_batch_keeps_native_rule_scope_separate_from_skill_scope() {
        let f = Fixture::new();
        let mut beta = f.skill();
        beta.source = f.base.join("library/beta");
        fs::create_dir_all(&beta.source).unwrap();
        fs::write(beta.source.join("SKILL.md"), "# Beta\n").unwrap();
        beta.key = proma_batch::source_key(&beta.source.join("SKILL.md"));
        beta.name = "beta".into(); beta.install_name = Some("beta".into());
        let skills = vec![f.skill(), beta];
        for skill in &skills {
            fs::create_dir_all(skill.source.join("local-data")).unwrap();
            fs::write(skill.source.join("local-data/ADAPTATION.md"), "LOCAL_DATA_CANARY\n").unwrap();
        }
        let selected_skills = skills.iter().map(|skill| skill.key.clone()).collect();
        let selected_agents = BTreeSet::from(["workbuddy".into()]);
        let projects = f.projects.iter().map(|project| project.path.clone()).collect();
        let installed = agents::AGENT_ICON_ORDER.iter().filter(|agent| agent.id == "workbuddy")
            .copied().collect::<Vec<_>>();
        let user_rule = f.projects[0].path.join(".codebuddy/rules/user.mdc");
        fs::create_dir_all(user_rule.parent().unwrap()).unwrap();
        fs::write(&user_rule, "USER_RULE_CANARY\n").unwrap();
        let catalog = Catalog::scan(skills.clone(), f.projects.clone(), installed.clone(), None);
        let install = catalog.plan(Action::Install, &selected_skills, &selected_agents, &projects, &BTreeSet::new()).unwrap();
        assert_eq!(install.count(Disposition::Change), 4);
        let rule_paths = install.items.iter().map(|item| {
            assert_eq!(item.path, item.project.join(".workbuddy/skills").join(item.skill.name.as_str()));
            assert!(item.affected_agents.iter().any(|agent| agent.id == "workbuddy"));
            assert_eq!(item.instruction_paths.len(), 1);
            let rule = item.instruction_paths[0].clone();
            assert_eq!(rule.parent().unwrap(), item.project.join(".codebuddy/rules"));
            assert!(!rule.exists());
            rule
        }).collect::<BTreeSet<_>>();
        assert_eq!(rule_paths.len(), 4);
        let result = install.execute();
        assert_eq!((result.changed(), result.failed()), (4, 0));
        let links = result.outcomes.iter().map(|outcome| {
            let path = outcome.item.path.clone();
            assert_eq!(fs::read_link(&path).unwrap(), outcome.item.skill.source);
            (path.clone(), entry_identity(&path).unwrap())
        }).collect::<Vec<_>>();
        for rule in &rule_paths {
            let body = fs::read_to_string(rule).unwrap();
            assert!(body.starts_with("---\nenabled: true\nalwaysApply: true\n---\n"));
            assert!(body.contains(".workbuddy/skills/"));
            assert!(!body.contains("LOCAL_DATA_CANARY"));
        }
        for skill in &skills { fs::write(skill.source.join("SKILL.md"), "SOURCE_REVISION_B\n").unwrap(); }
        for (path, identity) in &links {
            assert_eq!(entry_identity(path).unwrap(), *identity);
            assert_eq!(fs::read(path.join("SKILL.md")).unwrap(), b"SOURCE_REVISION_B\n");
        }
        let catalog = Catalog::scan(skills.clone(), f.projects.clone(), installed, None);
        let remove = catalog.plan(Action::Remove, &selected_skills, &selected_agents, &projects, &BTreeSet::new()).unwrap();
        assert_eq!(remove.count(Disposition::Change), 4);
        assert_eq!(remove.items.iter().flat_map(|item| item.instruction_paths.iter().cloned()).collect::<BTreeSet<_>>(), rule_paths);
        let result = remove.execute();
        assert_eq!((result.changed(), result.failed()), (4, 0));
        assert!(rule_paths.iter().all(|path| !path.exists()));
        assert!(links.iter().all(|(path, _)| path.symlink_metadata().is_err()));
        assert_eq!(fs::read(user_rule).unwrap(), b"USER_RULE_CANARY\n");
        for skill in &skills {
            assert_eq!(fs::read(skill.source.join("SKILL.md")).unwrap(), b"SOURCE_REVISION_B\n");
            assert_eq!(fs::read(skill.source.join("local-data/ADAPTATION.md")).unwrap(), b"LOCAL_DATA_CANARY\n");
        }
    }

    #[test]
    fn antigravity_batch_preview_tracks_shared_and_native_rules() {
        let f = Fixture::new();
        let beta = f.base.join("library/beta");
        fs::create_dir_all(&beta).unwrap();
        fs::write(beta.join("SKILL.md"), "# Beta").unwrap();
        let mut second = f.skill();
        second.key = proma_batch::source_key(&beta.join("SKILL.md"));
        second.name = "beta".into(); second.install_name = Some("beta".into()); second.source = beta.clone();
        let skills = vec![f.skill(), second];
        let selected_skills = skills.iter().map(|skill| skill.key.clone()).collect();
        let selected_agents = BTreeSet::from(["dimagent".into(), "antigravity".into()]);
        let projects = f.projects.iter().map(|project| project.path.clone()).collect();
        let installed = agents::AGENT_ICON_ORDER.iter()
            .filter(|agent| ["dimagent", "antigravity", "trae"].contains(&agent.id)).copied().collect::<Vec<_>>();
        let catalog = Catalog::scan(skills.clone(), f.projects.clone(), installed.clone(), None);
        let install = catalog.plan(Action::Install, &selected_skills, &selected_agents, &projects, &BTreeSet::new()).unwrap();
        assert_eq!(install.count(Disposition::Change), 8);
        let mut rule_paths = BTreeSet::new();
        for item in &install.items {
            let rule = crate::host_instructions::RulePlan::for_file_host(
                &item.project, InstallTarget::Antigravity, &item.path).unwrap().path;
            assert!(item.instruction_paths.contains(&rule));
            assert!(item.affected_agents.iter().any(|agent| agent.id == "antigravity"));
            assert!(!rule.exists());
            rule_paths.extend(item.instruction_paths.iter().cloned());
            if item.path.parent().unwrap().ends_with(".agent/skills") {
                assert!(!item.affected_agents.iter().any(|agent| agent.id == "trae"));
                assert!(item.instruction_paths.iter().all(|path| !path.starts_with(item.project.join(".trae"))));
            }
        }
        assert_eq!(rule_paths.len(), 12); // Two shared host Rules and one native Rule per skill/project.
        let result = install.execute();
        assert_eq!((result.changed(), result.failed()), (8, 0));
        assert!(rule_paths.iter().all(|path| path.is_file()));
        let catalog = Catalog::scan(skills, f.projects.clone(), installed, None);
        let remove = catalog.plan(Action::Remove, &selected_skills, &selected_agents, &projects, &BTreeSet::new()).unwrap();
        assert_eq!(remove.count(Disposition::Change), 8);
        assert_eq!(remove.items.iter().flat_map(|item| item.instruction_paths.iter().cloned()).collect::<BTreeSet<_>>(), rule_paths);
        assert!(rule_paths.iter().all(|path| path.is_file()));
        let result = remove.execute();
        assert_eq!((result.changed(), result.failed()), (8, 0));
        assert!(rule_paths.iter().all(|path| !path.exists()));
        assert!(f.source.join("SKILL.md").is_file() && beta.join("SKILL.md").is_file());
    }

    #[test]
    fn antigravity_rules_are_not_planned_for_an_unaffected_codex_root() {
        let f = Fixture::new();
        let installed = agents::AGENT_ICON_ORDER.iter()
            .filter(|agent| ["codex", "antigravity", "trae"].contains(&agent.id)).copied().collect();
        let catalog = Catalog::scan(vec![f.skill()], f.projects.clone(), installed, None);
        let install = f.plan(&catalog, Action::Install, &["codex"], &[]);
        assert_eq!(install.count(Disposition::Change), 2);
        assert!(install.items.iter().all(|item| item.instruction_paths.is_empty()
            && !item.affected_agents.iter().any(|agent| ["antigravity", "trae"].contains(&agent.id))));
        let result = install.execute();
        assert_eq!((result.changed(), result.failed()), (2, 0));
    }

    #[test]
    fn trae_shared_and_native_installs_keep_separate_rules_and_preserve_sources() {
        let f = Fixture::new();
        fs::create_dir_all(f.source.join("local-data")).unwrap();
        fs::write(f.source.join("local-data/ADAPTATION.md"), "local adaptation").unwrap();
        let personal = f.projects[0].path.join(".trae/rules/personal.md");
        fs::create_dir_all(personal.parent().unwrap()).unwrap();
        fs::write(&personal, "personal rule").unwrap();
        let catalog = f.catalog();
        let install = f.plan(&catalog, Action::Install, &["dimagent", "trae"], &[]);
        assert_eq!(install.count(Disposition::Change), 4);
        assert!(install.items.iter().all(|item| item.instruction_paths.len() == 1));
        assert_eq!(install.items.iter().flat_map(|item| item.instruction_paths.iter())
            .collect::<BTreeSet<_>>().len(), 4);
        let result = install.execute();
        assert_eq!(result.changed(), 4);
        assert_eq!(result.failed(), 0);
        for project in &f.projects {
            let rules = fs::read_dir(project.path.join(".trae/rules")).unwrap()
                .map(|entry| entry.unwrap().path()).filter(|path|
                    path.file_name().unwrap().to_string_lossy().starts_with("pleat-adaptation-")).collect::<Vec<_>>();
            assert_eq!(rules.len(), 2);
            let content = rules.iter().map(|path| fs::read_to_string(path).unwrap()).collect::<Vec<_>>();
            assert!(content.iter().any(|text| text.contains(".agents/skills/alpha/local-data/ADAPTATION.md")));
            assert!(content.iter().any(|text| text.contains(".trae/skills/alpha/local-data/ADAPTATION.md")));
        }
        let remove = f.plan(&f.catalog(), Action::Remove, &["dimagent", "trae"], &[]).execute();
        assert_eq!(remove.changed(), 4);
        assert_eq!(remove.failed(), 0);
        assert_eq!(fs::read_to_string(personal).unwrap(), "personal rule");
        assert_eq!(fs::read_to_string(f.source.join("local-data/ADAPTATION.md")).unwrap(), "local adaptation");
        assert!(f.source.join("SKILL.md").is_file());
        for project in &f.projects {
            assert!(fs::read_dir(project.path.join(".trae/rules")).unwrap().all(|entry|
                !entry.unwrap().file_name().to_string_lossy().starts_with("pleat-adaptation-")));
        }
    }

    #[test]
    fn trae_rules_survive_cross_entry_install_and_removal() {
        for entry in 0..4 {
            let f = Fixture::new();
            let catalog = f.catalog();
            if entry < 3 {
                assert_eq!(f.plan(&catalog, Action::Install, &["dimagent"], &[]).execute().changed(), 2);
            } else {
                assert!(crate::project::install_batch(
                    &f.projects.iter().map(|p| p.path.clone()).collect::<Vec<_>>(),
                    &[("alpha".into(), f.source.clone())], &[InstallTarget::Universal]).failures.is_empty());
            }
            let source_before = fs::read(f.source.join("SKILL.md")).unwrap();
            for project in &f.projects {
                let rule = crate::host_instructions::RulePlan::for_trae(project.path.as_path(),
                    &project.path.join(".agents/skills/alpha")).unwrap().path;
                assert!(rule.is_file());
                match entry {
                    0 => {
                        let listed = crate::project::list(&project.path, f.source.parent().unwrap()).unwrap();
                        let item = listed.iter().flat_map(|skill| &skill.installations)
                            .find(|item| item.path.ends_with(".agents/skills/alpha")).unwrap();
                        crate::project::remove_project_skill(item).unwrap();
                    }
                    1 => crate::project::uninstall(&project.path, f.source.parent().unwrap(), "alpha", &[InstallTarget::Universal]).unwrap(),
                    2 => crate::project::uninstall_all_from_path(&project.path, "alpha", &f.source).unwrap(),
                    _ => {}
                }
                if entry < 3 {
                    assert!(!rule.exists());
                    assert!(!project.path.join(".agents/skills/alpha").exists());
                }
            }
            if entry == 3 {
                let result = f.plan(&f.catalog(), Action::Remove, &["dimagent"], &[]).execute();
                assert_eq!(result.changed(), 2);
                assert_eq!(result.failed(), 0);
                for project in &f.projects {
                    let rule = crate::host_instructions::RulePlan::for_trae(&project.path,
                        &project.path.join(".agents/skills/alpha")).unwrap().path;
                    assert!(!rule.exists());
                }
            }
            assert_eq!(fs::read(f.source.join("SKILL.md")).unwrap(), source_before);
        }
    }

    #[test]
    fn trae_alias_removal_preserves_a_different_source_with_the_same_name() {
        let f = Fixture::new();
        let project = &f.projects[0].path;
        let shared = project.join(".agents/skills");
        fs::create_dir_all(&shared).unwrap();
        fs::create_dir_all(project.join(".codex")).unwrap();
        symlink(&shared, project.join(".codex/skills")).unwrap();
        let other = f.base.join("other/alpha");
        fs::create_dir_all(other.join("local-data")).unwrap();
        fs::write(other.join("SKILL.md"), "other source").unwrap();
        fs::write(other.join("local-data/ADAPTATION.md"), "other local data").unwrap();
        let catalog = f.catalog();
        let install = catalog.plan(Action::Install, &BTreeSet::from([f.skill().key]),
            &BTreeSet::from(["codex".into()]), &BTreeSet::from([project.clone()]), &BTreeSet::new()).unwrap().execute();
        assert_eq!(install.changed(), 1);
        assert_eq!(install.failed(), 0);
        crate::project::install_from_path(project, &other, "alpha", &[InstallTarget::Trae]).unwrap();
        let alias_rule = crate::host_instructions::RulePlan::for_trae(project, &project.join(".codex/skills/alpha")).unwrap().path;
        let other_rule = crate::host_instructions::RulePlan::for_trae(project, &project.join(".trae/skills/alpha")).unwrap().path;
        assert!(alias_rule.exists() && other_rule.exists());
        crate::project::uninstall_all_from_path(project, "alpha", &f.source).unwrap();
        assert!(!alias_rule.exists());
        assert!(other_rule.is_file());
        assert_eq!(fs::canonicalize(project.join(".trae/skills/alpha")).unwrap(), other);
        assert_eq!(fs::read_to_string(other.join("local-data/ADAPTATION.md")).unwrap(), "other local data");
        // Reinstall via one alias, then remove through the deduplicated alias.
        crate::project::install_from_path(project, &f.source, "alpha", &[InstallTarget::Codex]).unwrap();
        let mut item = crate::project::list(project, f.source.parent().unwrap()).unwrap()
            .into_iter().flat_map(|skill| skill.installations).find(|item| item.path.ends_with(".codex/skills/alpha")).unwrap();
        item.path = shared.join("alpha"); item.target = InstallTarget::Universal;
        crate::project::remove_project_skill_at(project, &item).unwrap();
        assert!(!alias_rule.exists());
        assert!(other_rule.exists());
        assert!(f.source.join("SKILL.md").exists());
    }

    #[test]
    fn codex_catalog_workspace_installs_into_member_projects_and_keeps_current_context() {
        let f = Fixture::new();
        let home = f.base.join("codex-profile");
        fs::create_dir(&home).unwrap();
        let database = home.join("state_5.sqlite");
        let writer = rusqlite::Connection::open(&database).unwrap();
        writer.execute_batch(
            "CREATE TABLE projects(id TEXT PRIMARY KEY, name TEXT NOT NULL);
             CREATE TABLE project_roots(project_id TEXT, position INTEGER, path TEXT);
             INSERT INTO projects VALUES('native-project-id', 'Native project');",
        ).unwrap();
        for (position, project) in f.projects.iter().enumerate() {
            writer.execute("INSERT INTO project_roots VALUES(?1,?2,?3)",
                rusqlite::params!["native-project-id", position as i64, project.path.to_str().unwrap()]).unwrap();
        }
        drop(writer);
        let database_before = fs::read(&database).unwrap();
        let projects = crate::project_discovery::discover_codex_projects(&home).unwrap();
        let installed = agents::AGENT_ICON_ORDER.iter()
            .filter(|agent| ["codex", "cursor"].contains(&agent.id)).copied().collect::<Vec<_>>();
        let catalog = Catalog::scan(vec![f.skill()], projects.clone(), installed, None);
        let workspaces = catalog.native_workspaces();
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].source_id.as_deref(), Some("native-project-id"));
        let skills = BTreeSet::from([f.skill().key]);
        let agents = BTreeSet::from(["codex".into(), "cursor".into()]);
        let scoped = BTreeSet::from([workspaces[0].id()]);
        let workspace_plan = catalog.plan(Action::Install, &skills, &agents, &BTreeSet::new(), &scoped).unwrap();
        assert_eq!(workspace_plan.items.len(), 2);
        assert!(workspace_plan.items.iter().all(|item|
            projects.iter().any(|project| project.path == item.project)
                && item.path == item.project.join(".codex/skills/alpha")
                && item.native_workspace_paths == [database.clone()]));
        assert_eq!(workspace_plan.execute().changed(), 2);
        assert!(projects.iter().all(|project| !project.path.join(".cursor/skills/alpha").exists()));
        let current = catalog.plan(Action::Install, &skills, &agents,
            &BTreeSet::from([projects[0].path.clone()]), &BTreeSet::new()).unwrap();
        assert_eq!(current.items.len(), 2);
        assert!(current.items.iter().all(|item| item.project == projects[0].path
            && item.native_workspace_paths.is_empty()));
        assert_eq!(fs::read(database).unwrap(), database_before);
    }

    #[test]
    fn native_multiroot_selection_expands_only_its_agent_and_keeps_project_context() {
        let f = Fixture::new();
        let workspace = crate::project_discovery::NativeWorkspace {
            agent: crate::effective_skills::AgentKind::Cursor,
            source_id: None,
            display_name: None,
            archived: None,
            path: f.base.join("review.code-workspace"),
            project_roots: std::sync::Arc::new(f.projects.iter().map(|project| project.path.clone()).collect()),
        };
        fs::write(&workspace.path, "{}").unwrap();
        let mut projects = f.projects.clone();
        for project in &mut projects { project.native_workspaces.push(workspace.clone()); }
        let installed = agents::AGENT_ICON_ORDER.iter().filter(|agent| ["cursor", "codex"].contains(&agent.id)).copied().collect::<Vec<_>>();
        let catalog = Catalog::scan(vec![f.skill()], projects.clone(), installed.clone(), None);
        assert_eq!(catalog.native_workspaces().len(), 1);
        let skills = BTreeSet::from([f.skill().key]);
        let agents = BTreeSet::from(["cursor".into(), "codex".into()]);
        let scoped = BTreeSet::from([workspace.id()]);
        let plan = catalog.plan(Action::Install, &skills, &agents, &BTreeSet::new(), &scoped).unwrap();
        assert_eq!(plan.items.len(), 2);
        assert!(plan.items.iter().all(|item| item.native_workspace_paths == [workspace.path.clone()]
            && item.path.ends_with(".cursor/skills/alpha")));
        assert_eq!(plan.execute().changed(), 2);
        assert!(projects.iter().all(|project| !project.path.join(".codex/skills/alpha").exists()));
        let current = catalog.plan(Action::Install, &skills, &agents,
            &BTreeSet::from([projects[0].path.clone()]), &BTreeSet::new()).unwrap();
        assert!(current.items.iter().all(|item| item.project == projects[0].path && item.native_workspace_paths.is_empty()));
        let catalog = Catalog::scan(vec![f.skill()], projects.clone(), installed, None);
        let remove = catalog.plan(Action::Remove, &skills, &agents, &BTreeSet::new(), &scoped).unwrap().execute();
        assert_eq!(remove.changed(), 2);
        assert_eq!(remove.failed(), 0);
        assert!(f.source.join("SKILL.md").is_file());
        assert!(catalog.plan(Action::Install, &skills, &agents, &BTreeSet::new(),
            &BTreeSet::from(["native:cursor:stale".into()])).is_err());
    }

    #[test]
    fn multi_agent_install_remove_preserves_source_and_only_explicit_workspaces() {
        let f = Fixture::new(); let catalog = f.catalog();
        let install = f.plan(&catalog, Action::Install, &["codex","proma"], &["b"]);
        assert_eq!(install.count(Disposition::Change), 3);
        let first = install.execute(); assert_eq!((first.changed(), first.failed()), (3, 0));
        let repeated = install.execute(); assert_eq!((repeated.changed(), repeated.failed()), (0, 3));
    }

    #[test]
    fn shared_claude_links_keep_both_rule_plans_and_retry_partial_removal() {
        let f = Fixture::new(); let shared = f.base.join("shared-skills"); fs::create_dir(&shared).unwrap();
        for project in &f.projects {
            fs::create_dir(project.path.join(".claude")).unwrap();
            symlink(&shared, project.path.join(".claude/skills")).unwrap();
        }
        let catalog = || Catalog::scan(vec![f.skill()], f.projects.clone(), agents::AGENT_ICON_ORDER.iter().filter(|a| a.id == "claude-code").copied().collect(), None);
        let plan = f.plan(&catalog(), Action::Install, &["claude-code"], &[]);
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].instruction_paths.len(), 2);
        let installed = plan.execute(); assert_eq!((installed.changed(), installed.failed()), (1, 0));
        let paths = &plan.items[0].instruction_paths;
        assert!(paths.iter().all(|path| path.is_file()));
        let original = fs::read(&paths[1]).unwrap(); fs::write(&paths[1], "user changes").unwrap();
        let blocked = f.plan(&catalog(), Action::Remove, &["claude-code"], &[]);
        assert_eq!(blocked.count(Disposition::Protected), 1);
        assert_eq!(blocked.execute().changed(), 0);
        assert!(shared.join("alpha").is_symlink() && paths[0].is_file());
        fs::write(&paths[1], original).unwrap();
        let removal = f.plan(&catalog(), Action::Remove, &["claude-code"], &[]);
        BEFORE_RECOVERY_NOTE.with(|hook| *hook.borrow_mut() = Some(Box::new(|backup| { fs::write(backup.join("restore.json"), "injected failure").unwrap(); })));
        let result = removal.execute(); assert_eq!(result.failed(), 1);
        assert!(shared.join("alpha").is_symlink());
        assert!(paths.iter().all(|path| path.is_file()));
        let retried = result.retry_plan().execute();
        assert_eq!((retried.changed(), retried.failed()), (1, 0));
        assert!(!shared.join("alpha").exists());
        assert!(f.source.join("SKILL.md").is_file());
    }

    #[test]
    fn rule_failure_preserves_concurrent_user_link_and_reports_conflict() {
        let f = Fixture::new();
        let catalog = Catalog::scan(vec![f.skill()], vec![f.projects[0].clone()], agents::AGENT_ICON_ORDER.iter().filter(|a| a.id == "claude-code").copied().collect(), None);
        let plan = f.plan(&catalog, Action::Install, &["claude-code"], &[]);
        let link = plan.items[0].path.clone(); let rule = plan.items[0].instruction_paths[0].clone();
        let foreign = f.base.join("foreign"); fs::create_dir(&foreign).unwrap(); fs::write(foreign.join("sentinel"), "keep").unwrap();
        let foreign_clone = foreign.clone(); let link_clone = link.clone();
        BEFORE_LINK_INSTRUCTIONS.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            fs::remove_file(&link_clone).unwrap(); symlink(&foreign_clone, &link_clone).unwrap();
            fs::create_dir_all(rule.parent().unwrap()).unwrap(); fs::write(rule, "user rule").unwrap();
        })));
        let result = plan.execute(); assert_eq!(result.failed(), 1);
        assert!(result.outcomes[0].error.as_ref().unwrap().contains("归属已变化"));
        assert_eq!(link.canonicalize().unwrap(), foreign);
        assert_eq!(fs::read_to_string(link.join("sentinel")).unwrap(), "keep");
        assert_eq!(result.retry_plan().execute().failed(), 1);
        assert_eq!(link.canonicalize().unwrap(), foreign);
    }

    #[test]
    fn installation_roundtrip_counts_physical_entries_and_retains_sources() {
        let f = Fixture::new();
        let install = f.plan(&f.catalog(), Action::Install, &["codex","proma"], &["b"]);
        let result = install.execute(); assert_eq!((result.changed(), result.failed()), (3, 0));
        assert!(f.projects[0].path.join(".codex/skills/alpha").is_symlink());
        assert!(f.root.join("agent-workspaces/b/skills/alpha/SKILL.md").is_file());
        assert!(!f.root.join("agent-workspaces/a/skills/alpha").exists());
        let remove = f.plan(&f.catalog(), Action::Remove, &["codex","proma"], &["b"]);
        let result = remove.execute(); assert_eq!((result.changed(), result.failed()), (3, 0));
        assert!(f.source.join("SKILL.md").is_file());
        assert!(!f.projects[0].path.join(".codex/skills/alpha").exists());
        assert!(f.projects[0].path.read_dir().unwrap().any(|p| p.unwrap().file_name().to_string_lossy().starts_with(".kitter-removed-")));
    }

    #[test]
    fn native_and_shared_targets_disclose_only_actual_installed_readers() {
        let f = Fixture::new();
        let plan = f.plan(&f.catalog(), Action::Install, &["dimagent","trae"], &[]);
        assert_eq!(plan.count(Disposition::Change), 4);
        for item in &plan.items {
            let shared = item.path.parent().unwrap().ends_with(".agents/skills");
            assert_eq!(item.affected_agents.iter().any(|a| a.id == "codex"), shared);
            assert!(item.affected_agents.iter().any(|a| a.id == "trae"));
        }
        let result = plan.execute(); assert_eq!((result.changed(), result.failed()), (4, 0));
        let remove = f.plan(&f.catalog(), Action::Remove, &["codex","dimagent","trae"], &[]);
        assert_eq!(remove.count(Disposition::Change), 4);
        assert_eq!(remove.execute().changed(), 4);
    }

    #[test]
    fn mixed_agents_cannot_toggle_and_single_proma_does_not_install_missing() {
        let f = Fixture::new(); let catalog = f.catalog();
        assert!(!Action::Disable.supported(&BTreeSet::from(["codex".into(),"proma".into()])));
        assert!(catalog.plan(Action::Disable, &BTreeSet::new(), &BTreeSet::from(["codex".into(),"proma".into()]), &BTreeSet::new(), &BTreeSet::new()).is_err());
        let plan = f.plan(&catalog, Action::Disable, &["proma"], &["a","b"]);
        assert_eq!(plan.count(Disposition::Missing), 2);
        assert_eq!(plan.execute().changed(), 0);
    }

    #[test]
    fn entity_directories_and_same_name_other_sources_are_protected() {
        let f = Fixture::new();
        let path = f.projects[0].path.join(".codex/skills/alpha"); fs::create_dir_all(&path).unwrap();
        fs::write(path.join("SKILL.md"), "project source, keep").unwrap();
        let plan = f.plan(&f.catalog(), Action::Install, &["codex"], &[]);
        assert_eq!(plan.count(Disposition::Protected), 1);
        assert_eq!(plan.execute().changed(), 1);
        assert_eq!(fs::read_to_string(path.join("SKILL.md")).unwrap(), "project source, keep");
        let alias = f.base.join("other"); fs::create_dir(&alias).unwrap();
        let link = f.projects[0].path.join(".cursor/skills/alpha"); fs::create_dir_all(link.parent().unwrap()).unwrap(); symlink(alias, &link).unwrap();
        assert_eq!(f.plan(&f.catalog(), Action::Install, &["cursor"], &[]).count(Disposition::Protected), 1);
    }

    #[test]
    fn same_name_different_sources_are_not_merged_or_overwritten() {
        let f = Fixture::new(); let mut other = f.skill(); other.source = f.base.join("other/alpha");
        fs::create_dir_all(&other.source).unwrap(); fs::write(other.source.join("SKILL.md"), "other source").unwrap();
        other.key = proma_batch::source_key(&other.source.join("SKILL.md"));
        let mut catalog = f.catalog(); catalog.skills.push(other.clone());
        let plan = catalog.plan(Action::Install, &BTreeSet::from([f.skill().key, other.key]), &BTreeSet::from(["codex".into()]),
            &BTreeSet::from([f.projects[0].path.clone()]), &BTreeSet::new()).unwrap();
        assert_eq!(plan.count(Disposition::Protected), 2); assert_eq!(plan.execute().changed(), 0);
    }

    #[test]
    fn removal_refuses_replaced_link_and_retry_keeps_other_successes() {
        let f = Fixture::new(); f.plan(&f.catalog(), Action::Install, &["codex"], &[]).execute();
        let plan = f.plan(&f.catalog(), Action::Remove, &["codex"], &[]);
        let path = f.projects[0].path.join(".codex/skills/alpha");
        fs::remove_file(&path).unwrap(); fs::create_dir(&path).unwrap(); fs::write(path.join("keep"), "keep").unwrap();
        let mut result = plan.execute(); assert_eq!((result.changed(), result.failed()), (1, 1));
        assert_eq!(result.retry_plan().items.len(), 1); assert!(path.join("keep").is_file());
        result.merge_retry(result.retry_plan().execute());
        assert_eq!((result.changed(), result.failed()), (1, 1)); assert!(f.source.join("SKILL.md").is_file());
    }

    #[test]
    fn redirected_target_parent_and_source_are_rejected_after_preview() {
        let f = Fixture::new(); let plan = f.plan(&f.catalog(), Action::Install, &["codex"], &[]);
        let other = f.base.join("unrelated"); fs::create_dir(&other).unwrap();
        symlink(&other, f.projects[0].path.join(".codex")).unwrap();
        let result = plan.execute(); assert_eq!((result.changed(), result.failed()), (1, 1));
        assert!(!other.join("skills/alpha").exists());
        let second = Fixture::new(); let plan = second.plan(&second.catalog(), Action::Install, &["codex"], &[]);
        fs::rename(&second.source, second.base.join("original")).unwrap(); symlink(&other, &second.source).unwrap();
        let result = plan.execute(); assert_eq!(result.failed(), 2);
    }

    #[test]
    fn alias_directories_remove_once_and_disclose_compatibility_agents() {
        let f = Fixture::new(); let p = &f.projects[0].path;
        fs::create_dir_all(p.join(".agents/skills")).unwrap(); fs::create_dir(p.join(".codex")).unwrap();
        symlink(p.join(".agents/skills"), p.join(".codex/skills")).unwrap();
        symlink(&f.source, p.join(".agents/skills/alpha")).unwrap();
        let plan = f.plan(&f.catalog(), Action::Remove, &["codex","cursor","dimagent"], &[]);
        assert_eq!(plan.count(Disposition::Change), 1);
        let item = plan.items.iter().find(|i| i.disposition == Disposition::Change).unwrap();
        assert!(item.affected_agents.iter().any(|a| a.id == "dimagent"));
        assert_eq!(plan.execute().changed(), 1); assert!(f.source.join("SKILL.md").is_file());
    }

    #[test]
    fn proma_id_changed_after_catalog_is_not_adopted_by_install_plan() {
        let f = Fixture::new(); let catalog = f.catalog();
        let index = f.root.join("agent-workspaces.json");
        let mut json: serde_json::Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
        json["workspaces"][0]["id"] = "changed-id".into();
        fs::write(index, serde_json::to_vec(&json).unwrap()).unwrap();
        let plan = f.plan(&catalog, Action::Install, &["proma"], &["a"]);
        assert_eq!(plan.count(Disposition::Protected), 1); assert_eq!(plan.execute().changed(), 0);
        assert!(!f.root.join("agent-workspaces/a/skills/alpha").exists());
    }

    #[test]
    fn regular_agent_alias_into_proma_cannot_bypass_entity_transaction() {
        let f = Fixture::new(); let project = &f.projects[0].path;
        let native = f.root.join("agent-workspaces/a/skills"); fs::create_dir(&native).unwrap();
        fs::create_dir(project.join(".codex")).unwrap(); symlink(&native, project.join(".codex/skills")).unwrap();
        for ids in [vec!["codex"], vec!["codex","proma"]] {
            let plan = f.catalog().plan(Action::Install, &BTreeSet::from([f.skill().key]), &ids.into_iter().map(str::to_string).collect(),
                &BTreeSet::from([project.clone()]), &BTreeSet::from(["a".into()])).unwrap();
            assert_eq!(plan.count(Disposition::Change), 0); assert_eq!(plan.execute().changed(), 0);
            assert!(!native.join("alpha").exists());
        }
    }

    #[test]
    fn relative_link_removal_preserves_raw_target_and_source() {
        let f = Fixture::new(); let p = &f.projects[0].path;
        fs::create_dir_all(p.join(".codex/skills")).unwrap();
        symlink("../../../library/alpha", p.join(".codex/skills/alpha")).unwrap();
        let plan = f.plan(&f.catalog(), Action::Remove, &["codex"], &[]);
        assert_eq!(plan.count(Disposition::Change), 1);
        let result = plan.execute(); assert_eq!((result.changed(), result.failed()), (1, 0));
        assert!(!p.join(".codex/skills/alpha").exists()); assert!(f.source.join("SKILL.md").is_file());
    }

    #[test]
    fn recovery_note_failure_leaves_link_for_safe_retry() {
        let f = Fixture::new(); f.plan(&f.catalog(), Action::Install, &["codex"], &[]).execute();
        let plan = f.plan(&f.catalog(), Action::Remove, &["codex"], &[]);
        BEFORE_RECOVERY_NOTE.with(|hook| *hook.borrow_mut() = Some(Box::new(|backup| {
            fs::create_dir(backup.join("restore.json")).unwrap();
        })));
        let mut result = plan.execute(); assert_eq!((result.changed(), result.failed()), (1, 1));
        assert!(f.projects[0].path.join(".codex/skills/alpha").is_symlink());
        result.merge_retry(result.retry_plan().execute()); assert_eq!((result.changed(), result.failed()), (2, 0));
        assert!(f.source.join("SKILL.md").is_file());
    }

    #[test]
    fn recovery_project_swap_cannot_overwrite_sentinel_or_remove_external_install() {
        let f = Fixture::new();
        let project = f.projects[0].path.clone();
        let external = f.base.join("external-skills");
        fs::create_dir(&external).unwrap();
        fs::create_dir(project.join(".codex")).unwrap();
        symlink(&external, project.join(".codex/skills")).unwrap();
        f.plan(&f.catalog(), Action::Install, &["codex"], &[]).execute();
        let mut plan = f.plan(&f.catalog(), Action::Remove, &["codex"], &[]);
        plan.items.retain(|item| item.project == project);
        let sentinel = f.base.join("sentinel.txt");
        fs::write(&sentinel, "keep sentinel").unwrap();
        let retained = f.base.join("retained-project");
        let target = sentinel.clone();
        BEFORE_RECOVERY_NOTE.with(|hook| *hook.borrow_mut() = Some(Box::new(move |backup| {
            fs::rename(&project, &retained).unwrap();
            fs::create_dir_all(backup).unwrap();
            symlink(&target, backup.join("restore.json")).unwrap();
        })));
        let result = plan.execute();
        assert_eq!(result.failed(), 1);
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "keep sentinel");
        assert!(external.join("alpha").is_symlink());
        assert!(f.source.join("SKILL.md").is_file());
    }

    #[test]
    fn replaced_project_directory_at_same_path_invalidates_plan() {
        let f = Fixture::new(); let plan = f.plan(&f.catalog(), Action::Install, &["codex"], &[]);
        let p = &f.projects[0].path; fs::rename(p, f.base.join("retained-project")).unwrap(); fs::create_dir(p).unwrap();
        let result = plan.execute(); assert_eq!((result.changed(), result.failed()), (1, 1));
        assert!(!p.join(".codex/skills/alpha").exists());
    }

    #[test]
    fn proma_removal_refuses_changed_native_identity_or_marker_source() {
        for replace_id in [true, false] {
            let f = Fixture::new();
            let install = f.plan(&f.catalog(), Action::Install, &["proma"], &["a"]).execute(); assert_eq!(install.changed(), 1);
            let plan = f.plan(&f.catalog(), Action::Remove, &["proma"], &["a"]);
            let path = f.root.join("agent-workspaces/a/skills/alpha");
            let marker_path = path.join(".kitter-proma.json");
            let mut marker: serde_json::Value = serde_json::from_slice(&fs::read(&marker_path).unwrap()).unwrap();
            if replace_id {
                let index_path = f.root.join("agent-workspaces.json");
                let mut index: serde_json::Value = serde_json::from_slice(&fs::read(&index_path).unwrap()).unwrap();
                index["workspaces"][0]["id"] = "replacement".into(); marker["workspace_id"] = "replacement".into();
                fs::write(index_path, serde_json::to_vec(&index).unwrap()).unwrap();
            } else {
                let other = f.base.join("other-alpha"); fs::create_dir(&other).unwrap();
                fs::copy(f.source.join("SKILL.md"), other.join("SKILL.md")).unwrap(); marker["source"] = other.to_string_lossy().into_owned().into();
            }
            fs::write(marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();
            let result = plan.execute(); assert_eq!((result.changed(), result.failed()), (0, 1));
            assert!(path.join("SKILL.md").is_file()); assert!(f.source.join("SKILL.md").is_file());
        }
    }

    #[test]
    fn manual_project_rows_keep_native_archive_state_and_explicit_restore() {
        let mut f = Fixture::new(); let path = f.root.join("agent-workspaces.json");
        let mut index: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        index["workspaces"][2]["archived"] = true.into(); fs::write(path, serde_json::to_vec(&index).unwrap()).unwrap();
        let catalog = f.catalog();
        assert!(!catalog.projects.iter().find(|p| p.path == f.projects[1].path).unwrap().is_active());
        assert!(catalog.projects.iter().find(|p| p.path == f.projects[0].path).unwrap().is_active());
        f.projects[1].retained = true;
        assert!(f.catalog().projects.iter().find(|p| p.path == f.projects[1].path).unwrap().is_active());
    }

    #[test]
    fn lifecycle_user_copy_is_uninstalled_or_reinstalled_never_toggled() {
        let f = Fixture::new();
        assert_eq!(f.plan(&f.catalog(), Action::Install, &["proma"], &["a"]).execute().changed(), 1);
        let plan = f.plan(&f.catalog(), Action::Disable, &["proma"], &["a"]);
        assert_eq!(plan.count(Disposition::Protected), 1); assert_eq!(plan.execute().changed(), 0);
        let active = f.root.join("agent-workspaces/a/skills/alpha");
        let inactive = f.root.join("agent-workspaces/a/skills-inactive/alpha");
        fs::create_dir_all(inactive.parent().unwrap()).unwrap(); fs::rename(&active, &inactive).unwrap();
        let plan = f.plan(&f.catalog(), Action::Install, &["proma"], &["a"]);
        assert_eq!(plan.items[0].operation_kind(Action::Install), "reinstall_copy");
        let result = plan.execute(); assert_eq!((result.changed(), result.failed()), (1, 0));
        assert!(active.join("SKILL.md").is_file()); assert!(!inactive.exists());
        assert_eq!(f.plan(&f.catalog(), Action::Remove, &["proma"], &["a"]).execute().changed(), 1);
        assert!(!active.exists()); assert!(f.source.join("SKILL.md").is_file());
    }

    #[test]
    fn lifecycle_proma_legacy_link_converts_only_after_explicit_review() {
        let f = Fixture::new(); let link = f.root.join("agent-workspaces/a/skills/alpha");
        fs::create_dir(link.parent().unwrap()).unwrap(); symlink(&f.source, &link).unwrap();
        let install = f.plan(&f.catalog(), Action::Install, &["proma"], &["a"]);
        assert_eq!(install.count(Disposition::Protected), 1); assert!(link.is_symlink());
        let convert = f.plan(&f.catalog(), Action::Convert, &["proma"], &["a"]);
        assert_eq!(convert.count(Disposition::Change), 1);
        assert_eq!(convert.items[0].operation_kind(Action::Convert), "convert_link");
        assert!(link.is_symlink());
        let result = convert.execute(); assert_eq!((result.changed(), result.failed()), (1, 0));
        assert!(!link.is_symlink()); assert!(link.join(".kitter-proma.json").is_file());
        assert_eq!(fs::read_to_string(f.source.join("SKILL.md")).unwrap(), "---\nname: alpha\ndescription: Test\n---\nKeep source\n");
    }

    #[test]
    fn lifecycle_unknown_and_broken_proma_links_can_be_removed_without_sources() {
        let f = Fixture::new(); let directory = f.root.join("agent-workspaces/a/skills"); fs::create_dir(&directory).unwrap();
        let outside = f.base.join("outside/other"); fs::create_dir_all(&outside).unwrap(); fs::write(outside.join("SKILL.md"), "keep outside source").unwrap();
        symlink(&outside, directory.join("unknown")).unwrap(); symlink(f.base.join("missing"), directory.join("broken")).unwrap();
        let catalog = f.catalog();
        let keys = catalog.skills.iter().filter(|s| ["unknown", "broken"].contains(&s.name.as_str())).map(|s| s.key.clone()).collect();
        let remove = catalog.plan(Action::Remove, &keys, &BTreeSet::from(["proma".into()]), &BTreeSet::new(), &BTreeSet::from(["a".into()])).unwrap();
        assert_eq!(remove.count(Disposition::Change), 2);
        assert!(remove.items.iter().all(|item| item.operation_kind(Action::Remove) == "remove_link"));
        let result = remove.execute(); assert_eq!((result.changed(), result.failed()), (2, 0));
        assert!(directory.join("unknown").symlink_metadata().is_err()); assert!(directory.join("broken").symlink_metadata().is_err());
        assert_eq!(fs::read_to_string(outside.join("SKILL.md")).unwrap(), "keep outside source");
    }

    #[test]
    fn lifecycle_link_conversion_requires_registered_same_source_and_frozen_target() {
        let f = Fixture::new(); let link = f.root.join("agent-workspaces/a/skills/alpha"); fs::create_dir(link.parent().unwrap()).unwrap(); symlink(&f.source, &link).unwrap();
        let convert = f.plan(&f.catalog(), Action::Convert, &["proma"], &["a"]);
        let outside = f.base.join("outside"); fs::create_dir(&outside).unwrap(); fs::write(outside.join("SKILL.md"), "keep outside").unwrap();
        fs::remove_file(&link).unwrap(); symlink(&outside, &link).unwrap();
        let result = convert.execute(); assert_eq!((result.changed(), result.failed()), (0, 1));
        assert!(link.is_symlink()); assert_eq!(fs::read_to_string(outside.join("SKILL.md")).unwrap(), "keep outside");
        let catalog = f.catalog(); let unknown = catalog.skills.iter().find(|s| s.key == proma_batch::source_key(&outside.join("SKILL.md"))).unwrap();
        assert!(unknown.install_name.is_none());
        let plan = catalog.plan(Action::Convert, &BTreeSet::from([unknown.key.clone()]), &BTreeSet::from(["proma".into()]), &BTreeSet::new(), &BTreeSet::from(["a".into()])).unwrap();
        assert_eq!(plan.count(Disposition::Protected), 1); assert!(link.is_symlink());
    }

    #[test]
    fn lifecycle_same_name_builtin_and_library_user_skill_have_separate_actions() {
        let f = Fixture::new(); let template = f.root.join("fixture-app/default-skills/alpha"); let preset = f.root.join("agent-workspaces/a/skills/alpha");
        for p in [&template, &preset] { fs::create_dir_all(p).unwrap(); fs::write(p.join("SKILL.md"), "---\nname: alpha\ndescription: Built-in\n---\nPreset\n").unwrap(); }
        proma::register_test_bundled_templates(&f.root, template.parent().unwrap());
        let catalog = f.catalog(); assert_eq!(catalog.skills.iter().filter(|s| s.name == "alpha").count(), 2);
        let keys = BTreeSet::from(["proma:builtin:alpha".into()]);
        for action in [Action::Install, Action::Remove, Action::Convert] {
            let plan = catalog.plan(action, &keys, &BTreeSet::from(["proma".into()]), &BTreeSet::new(), &BTreeSet::from(["a".into()])).unwrap();
            assert_eq!(plan.count(Disposition::Protected), 1);
        }
        let plan = catalog.plan(Action::Disable, &keys, &BTreeSet::from(["proma".into()]), &BTreeSet::new(), &BTreeSet::from(["a".into()])).unwrap();
        assert_eq!(plan.execute().changed(), 1);
        assert!(!preset.exists()); assert!(f.root.join("agent-workspaces/a/skills-inactive/alpha/SKILL.md").is_file());
        assert!(f.source.join("SKILL.md").is_file());
    }

    #[test]
    fn lifecycle_scanned_links_cannot_adopt_new_targets_before_preview() {
        for broken in [false, true] {
            let f = Fixture::new(); let link = f.root.join("agent-workspaces/a/skills/alpha");
            fs::create_dir(link.parent().unwrap()).unwrap();
            let first = if broken { f.base.join("missing-first") } else { f.source.clone() };
            symlink(&first, &link).unwrap();
            let catalog = f.catalog();
            let key = catalog.proma.as_ref().unwrap().workspaces.iter().find(|w| w.workspace.id == "a").unwrap().identities[&link].clone();
            let replacement = f.base.join("replacement");
            if !broken { fs::create_dir(&replacement).unwrap(); fs::write(replacement.join("SKILL.md"), "keep replacement").unwrap(); }
            fs::remove_file(&link).unwrap(); symlink(&replacement, &link).unwrap();
            let plan = catalog.plan(Action::Remove, &BTreeSet::from([key]), &BTreeSet::from(["proma".into()]),
                &BTreeSet::new(), &BTreeSet::from(["a".into()])).unwrap();
            assert_eq!(plan.count(Disposition::Protected), 1);
            assert_eq!(plan.execute().changed(), 0); assert_eq!(fs::read_link(&link).unwrap(), replacement);
            assert!(f.source.join("SKILL.md").is_file());
        }
    }

    #[test]
    fn lifecycle_uninstall_refuses_same_source_objects_replaced_after_preview() {
        for replace in ["copy", "parent", "workspace", "update"] {
            let f = Fixture::new(); f.plan(&f.catalog(), Action::Install, &["proma"], &["a"]).execute();
            let plan = f.plan(&f.catalog(), Action::Remove, &["proma"], &["a"]);
            let workspace = f.root.join("agent-workspaces/a"); let parent = workspace.join("skills"); let copy = parent.join("alpha");
            let replaced = match replace { "workspace" => &workspace, "parent" => &parent, _ => &copy };
            if replace == "update" {
                fs::write(f.source.join("SKILL.md"), "---\nname: alpha\ndescription: Updated\n---\nNew source version\n").unwrap();
                proma::apply_plan(&proma::plan_install(&f.root, &workspace, &f.source, "alpha").unwrap()).unwrap();
            } else {
                let original = f.base.join("preserved-original"); fs::rename(replaced, &original).unwrap();
                fs::create_dir_all(&copy).unwrap();
                let old_copy = match replace { "workspace" => original.join("skills/alpha"), "parent" => original.join("alpha"), _ => original.clone() };
                for name in ["SKILL.md", ".kitter-proma.json"] { fs::copy(old_copy.join(name), copy.join(name)).unwrap(); }
            }
            let result = plan.execute(); assert_eq!((result.changed(), result.failed()), (0, 1), "{replace}");
            assert!(copy.join("SKILL.md").is_file()); assert!(f.source.join("SKILL.md").is_file());
        }
    }
}
