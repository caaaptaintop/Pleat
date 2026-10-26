use super::*;
use gpui_base::Disableable;
use crate::cross_project::{Action, BatchResult, Catalog, Disposition, Plan, Skill};

pub(super) struct CrossProjectState {
    catalog: Option<Arc<Catalog>>,
    plan: Option<Plan>,
    result: Option<BatchResult>,
    skills: BTreeSet<String>,
    agents: BTreeSet<String>,
    projects: BTreeSet<PathBuf>,
    workspaces: BTreeSet<String>,
    initial_project: Option<PathBuf>,
    conversion_paths: Option<BTreeSet<PathBuf>>,
    collapsed: BTreeSet<PathBuf>,
    skill_search: Entity<InputState>,
    project_search: Entity<InputState>,
    skill_scroll: ScrollHandle,
    project_scroll: ScrollHandle,
    action: Action,
    preset_mode: bool,
    loading: bool,
    planning: bool,
    busy: bool,
    reviewed: bool,
    generation: u64,
    plan_generation: u64,
    error: Option<String>,
}

fn toggle_visible<T: Ord + Clone>(selected: &mut BTreeSet<T>, visible: &[T]) {
    if !visible.is_empty() && visible.iter().all(|key| selected.contains(key)) {
        selected.retain(|key| !visible.contains(key));
    } else { selected.extend(visible.iter().cloned()); }
}

fn skill_matches_action(catalog: &Catalog, skill: &Skill, preset_mode: bool, action: Action) -> bool {
    if skill.is_preset() != preset_mode { return false; }
    if skill.provider == "library" && skill.install_name.is_none() { return false; }
    if action == Action::Install { return skill.install_name.is_some(); }
    if action != Action::Convert { return true; }
    catalog.proma.iter().flat_map(|p| &p.workspaces).any(|w| w.installations.iter().any(|copy| {
        matches!(copy.status.as_str(), "external_link" | "broken_link") && w.identities.get(&copy.path) == Some(&skill.key)
    }))
}

impl CrossProjectState {
    pub(super) fn new(english: bool, window: &mut Window, cx: &mut Context<KitterApp>) -> Self {
        let skill_search = cx.new(|cx| InputState::new(window, cx).placeholder(if english { "Search skill names" } else { "搜索技能名称" }));
        let project_search = cx.new(|cx| InputState::new(window, cx).placeholder(if english { "Search projects or workspaces" } else { "搜索项目或工作区" }));
        for search in [&skill_search, &project_search] {
            cx.subscribe_in(search, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.cross_project.skill_scroll.set_offset(point(px(0.), px(0.)));
                    this.cross_project.project_scroll.set_offset(point(px(0.), px(0.)));
                    this.notify_dialog(cx);
                }
            }).detach();
        }
        Self { catalog: None, plan: None, result: None, skills: BTreeSet::new(), agents: BTreeSet::new(), projects: BTreeSet::new(),
            workspaces: BTreeSet::new(), initial_project: None, conversion_paths: None, collapsed: BTreeSet::new(), skill_search, project_search,
            skill_scroll: ScrollHandle::new(), project_scroll: ScrollHandle::new(), action: Action::Install, preset_mode: false,
            loading: false, planning: false, busy: false, reviewed: false, generation: 0, plan_generation: 0, error: None }
    }

    fn select_initial_destinations(&mut self, catalog: &Catalog) {
        if self.agents.contains("proma") && let Some(project) = self.initial_project.take().filter(|p| self.projects.contains(p)) {
            let archived = catalog.projects.iter().any(|p| p.path == project && p.archived);
            self.workspaces.extend(catalog.proma.iter().flat_map(|c| &c.workspaces)
                .filter(|w| w.workspace.project_root_path.as_ref().unwrap_or(&w.workspace.path) == &project)
                .filter(|w| !w.workspace.archived || archived)
                .filter(|w| w.error.is_none())
                .map(|w| w.workspace.id.clone()));
        }
        if let Some(paths) = &self.conversion_paths {
            self.workspaces = catalog.proma.iter().flat_map(|c| &c.workspaces)
                .filter(|w| paths.iter().any(|path| path.parent().and_then(Path::parent) == Some(w.workspace.path.as_path())))
                .map(|w| w.workspace.id.clone()).collect();
        }
    }
}

impl KitterApp {
    pub(super) fn cross_project_busy(&self) -> bool { self.cross_project.busy }

    pub(super) fn cross_project_agents_changed(&mut self) {
        if self.cross_project.busy { return; }
        let available = self.available_project_agents();
        self.cross_project.agents.retain(|id| available.iter().any(|a| a.id == id && !a.global_only));
        if let Some(catalog) = &mut self.cross_project.catalog { Arc::make_mut(catalog).installed_agents = self.model.installed_agents.clone(); }
        self.cross_project.plan_generation = self.cross_project.plan_generation.wrapping_add(1);
        self.cross_project.plan = None;
        self.cross_project.planning = false;
        self.cross_project.reviewed = self.cross_project.conversion_paths.is_some();
        self.cross_project.error = Some(self.tr("Agent 安装状态已变化，请刷新后重新预览", "Agent availability changed; refresh to review again").into());
    }

    pub(super) fn cross_project_explanation(&self) -> String {
        self.tr("用户技能安装到项目，或从项目卸载；Proma 内置预设单独启停。旧软链接可移除，或转换为工作区副本。",
            "Install or uninstall user skills across projects. Manage Proma built-in presets separately; remove legacy links or convert them to workspace copies.").to_string()
    }

    pub(super) fn open_library_cross_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.selected_skill_keys();
        let keys = self.model.skills.iter().filter(|s| selected.iter().any(|key| key == skill_storage_name(s)))
            .map(|s| crate::proma_batch::source_key(&s.path.join("SKILL.md"))).collect();
        self.open_cross_project(keys, None, Action::Install, None, window, cx);
    }

    pub(super) fn open_cross_project(&mut self, keys: Vec<String>, project: Option<PathBuf>, action: Action, agent: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.cross_project.busy { return; }
        self.cross_project.skill_search.update(cx, |input, cx| input.set_value("", window, cx));
        self.cross_project.project_search.update(cx, |input, cx| input.set_value("", window, cx));
        self.cross_project.skill_scroll.set_offset(point(px(0.), px(0.)));
        self.cross_project.project_scroll.set_offset(point(px(0.), px(0.)));
        self.cross_project.skills = keys.into_iter().collect();
        self.cross_project.agents = if action.is_state() || action == Action::Convert { BTreeSet::from(["proma".into()]) }
            else { agent.into_iter().collect() };
        if self.cross_project.agents.is_empty() && let Some(path) = &project {
            if let Some(selected) = self.projects_view.selected_project_agent {
                self.cross_project.agents.insert(selected.id().to_string());
            } else if let Some(entry) = self.all_project_entries().iter().find(|entry| &entry.path == path) {
                self.cross_project.agents.extend(entry.agents.iter().map(|a| a.id().to_string()));
            }
        }
        self.cross_project.projects = project.clone().into_iter().collect();
        self.cross_project.workspaces.clear();
        self.cross_project.initial_project = project;
        self.cross_project.conversion_paths = None;
        self.cross_project.collapsed.clear();
        self.cross_project.action = action;
        self.cross_project.preset_mode = action.is_state();
        self.cross_project.reviewed = false;
        self.cross_project.result = None;
        self.open_dialog(DialogKind::CrossProject, cx);
        self.load_cross_project(cx);
    }

    pub(super) fn open_proma_conversion(&mut self, keys: Vec<String>, project: PathBuf, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if self.cross_project.busy { return; }
        self.open_cross_project(keys, Some(project), Action::Convert, Some("proma".into()), window, cx);
        self.cross_project.conversion_paths = Some(paths.into_iter().collect());
        self.cross_project.reviewed = true;
    }

    fn load_cross_project(&mut self, cx: &mut Context<Self>) {
        if self.cross_project.busy { return; }
        self.cross_project.generation = self.cross_project.generation.wrapping_add(1);
        self.cross_project.plan_generation = self.cross_project.plan_generation.wrapping_add(1);
        let generation = self.cross_project.generation;
        self.cross_project.loading = true;
        self.cross_project.planning = false;
        self.cross_project.result = None;
        self.cross_project.error = None;
        self.cross_project.plan = None;
        self.cross_project.reviewed = self.cross_project.conversion_paths.is_some();
        let projects = self.all_project_entries();
        let installed = self.model.installed_agents.clone();
        let root = crate::proma::home();
        let skills = self.model.skills.iter().map(|s| Skill {
            key: crate::proma_batch::source_key(&s.path.join("SKILL.md")), name: s.record.name.clone(), source: s.path.clone(), provider: "library",
            install_name: (!s.record.origin.is_builtin()).then(|| skill_storage_name(s).to_string()),
        }).collect();
        let preview = cfg!(debug_assertions) && std::env::var("KITTER_PREVIEW_CROSS_PROJECT").as_deref() == Ok("1") && self.cross_project.catalog.is_none();
        let task = cx.background_executor().spawn(async move { Catalog::scan(skills, projects, installed, root.as_deref()) });
        cx.spawn(async move |this, cx| {
            let catalog = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.cross_project.generation != generation { return; }
                this.cross_project.loading = false;
                if let Some(proma) = &catalog.proma {
                    for copy in proma.workspaces.iter().flat_map(|w| &w.installations) {
                        if this.cross_project.skills.contains(&crate::proma_batch::source_key(&copy.path.join("SKILL.md"))) {
                            this.cross_project.skills.insert(crate::proma_batch::installation_key(copy));
                        }
                    }
                }
                let preset_mode = this.cross_project.preset_mode;
                let action = this.cross_project.action;
                let available = catalog.available_agents();
                this.cross_project.skills.retain(|key| catalog.skills.iter().any(|s| &s.key == key && skill_matches_action(&catalog, s, preset_mode, action)));
                this.cross_project.agents.retain(|id| available.iter().any(|a| a.id == id && !a.global_only));
                this.cross_project.projects.retain(|path| catalog.projects.iter().any(|p| &p.path == path));
                this.cross_project.workspaces.retain(|id| catalog.proma.as_ref().is_some_and(|p| p.workspaces.iter().any(|w| &w.workspace.id == id))
                    || catalog.native_workspaces().iter().any(|workspace| workspace.id() == *id));
                this.cross_project.select_initial_destinations(&catalog);
                if preview {
                    this.cross_project.agents = ["codex", "proma"].into_iter().filter(|id| available.iter().any(|a| a.id == *id)).map(str::to_string).collect();
                    this.cross_project.projects.extend(catalog.projects.iter().filter(|p| p.is_active()).take(2).map(|p| p.path.clone()));
                    this.cross_project.workspaces.extend(catalog.proma.iter().flat_map(|p| &p.workspaces).filter(|w| !w.workspace.archived).take(3).map(|w| w.workspace.id.clone()));
                }
                this.cross_project.catalog = Some(Arc::new(catalog));
                this.update_cross_project_plan(cx);
            });
        }).detach();
        self.notify_dialog(cx);
    }

    fn update_cross_project_plan(&mut self, cx: &mut Context<Self>) {
        let Some(catalog) = self.cross_project.catalog.clone() else { return; };
        if self.cross_project.busy || self.cross_project.loading { return; }
        let state = &mut self.cross_project;
        state.select_initial_destinations(&catalog);
        state.plan_generation = state.plan_generation.wrapping_add(1);
        let generation = state.plan_generation;
        let (action, skills, agents, projects, workspaces, conversion_paths) = (state.action, state.skills.clone(), state.agents.clone(), state.projects.clone(), state.workspaces.clone(), state.conversion_paths.clone());
        state.planning = true; state.plan = None; state.error = None; state.reviewed = state.conversion_paths.is_some();
        let task = cx.background_executor().spawn(async move {
            catalog.plan(action, &skills, &agents, &projects, &workspaces).map(|mut plan| {
                if let Some(paths) = conversion_paths { plan.items.retain(|item| paths.contains(&item.path)); }
                plan
            }).map_err(|e| format!("{e:#}"))
        });
        cx.spawn(async move |this, cx| {
            let plan = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.cross_project.plan_generation != generation { return; }
                this.cross_project.planning = false;
                match plan { Ok(plan) => this.cross_project.plan = Some(plan), Err(error) => this.cross_project.error = Some(error) }
                this.notify_dialog(cx);
            });
        }).detach();
        self.notify_dialog(cx);
    }

    fn execute_cross_project(&mut self, retry: bool, cx: &mut Context<Self>) {
        let state = &mut self.cross_project;
        if state.busy || state.loading || state.planning || !state.reviewed { return; }
        let plan = if retry { state.result.as_ref().map(BatchResult::retry_plan) }
            else if state.result.is_none() { state.plan.clone() } else { None };
        let Some(plan) = plan.filter(|p| p.count(Disposition::Change) > 0) else { return; };
        state.busy = true;
        let task = cx.background_executor().spawn(async move { plan.execute() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.cross_project.busy = false;
                if let Some(previous) = &mut this.cross_project.result { previous.merge_retry(result); }
                else { this.cross_project.result = Some(result); }
                this.refresh(cx); this.notify_dialog(cx);
            });
        }).detach();
        self.notify_dialog(cx);
    }

    fn cross_project_verb(&self, action: Action) -> &'static str {
        match action { Action::Install => self.tr("安装到项目", "Install to projects"), Action::Remove => self.tr("从项目卸载", "Uninstall from projects"),
            Action::Convert => self.tr("转换为副本", "Convert to copies"),
            Action::Enable => self.tr("启用预设", "Enable presets"), Action::Disable => self.tr("停用预设", "Disable presets") }
    }

    fn cross_project_action_hint(&self, action: Action) -> &'static str {
        match action {
            Action::Install => self.tr("从 Pleat 安装所选用户技能。Proma 新副本安装后默认可用，旧停用副本会明确列为重新安装。", "Install user skills from Pleat. New Proma copies are active; legacy inactive copies are reviewed as reinstalls."),
            Action::Remove => self.tr("移除所选项目的用户技能安装。实体副本会卸载；软链接仅移除链接，Pleat 技能源及本机配置保留。", "Uninstall selected project copies, or remove only their links. Pleat sources and local settings are retained."),
            Action::Convert => self.tr("仅用于 Proma 旧软链接。来源已在 Pleat 收录且匹配时，替换为工作区副本；未收录或失效链接可先移除。", "For legacy Proma links only. Replace links to matching Pleat sources with workspace copies; unregistered or broken links can be removed."),
            Action::Enable => self.tr("仅启用已核验的 Proma 内置预设。用户技能请在“用户技能”中安装；集成项由提供方维护。", "Enable verified Proma built-in presets. Install user skills in User skills; integrations remain provider-managed."),
            Action::Disable => self.tr("仅停用 Proma 内置预设，保留文件以便恢复。用户技能请使用卸载。", "Disable Proma built-in presets while retaining their files. Uninstall user skills instead."),
        }
    }

    fn cross_project_operation_label(&self, item: &crate::cross_project::Item, action: Action) -> &'static str {
        match item.operation_kind(action) {
            "remove_link" => self.tr("仅移除链接", "Remove link only"),
            "remove_copy" => self.tr("卸载工作区副本", "Uninstall workspace copy"),
            "convert_link" => self.tr("旧链接转换为副本", "Convert legacy link to copy"),
            "reinstall_copy" => self.tr("重新安装旧停用副本", "Reinstall legacy inactive copy"),
            "install_copy" => self.tr("安装工作区副本", "Install workspace copy"),
            "install_link" => self.tr("安装项目链接", "Install project link"),
            "enable_preset" => self.tr("启用内置预设", "Enable built-in preset"),
            "disable_preset" => self.tr("停用内置预设", "Disable built-in preset"),
            _ => self.cross_project_verb(action),
        }
    }

    fn cross_project_summary(&self, plan: &Plan) -> String {
        let mut parts = vec![if self.uses_english() { format!("{} to change", plan.count(Disposition::Change)) }
            else { format!("待处理 {} 处", plan.count(Disposition::Change)) }];
        for (kind, label) in [(Disposition::Unchanged, self.tr("无需变更", "unchanged")),
            (Disposition::Missing, self.tr("无对应安装", "no installation")), (Disposition::Protected, self.tr("受保护", "protected")),
            (Disposition::Unavailable, self.tr("无法扫描", "unavailable"))] {
            let count = plan.count(kind); if count > 0 { parts.push(format!("{label} {count}")); }
        }
        parts.join(" · ")
    }

    fn cross_project_status(&self, disposition: Disposition, action: Action) -> String {
        match disposition {
            Disposition::Change => if self.uses_english() { format!("Will {}", self.cross_project_verb(action)) } else { format!("将{}", self.cross_project_verb(action)) },
            Disposition::Unchanged => self.tr("无需变更", "Unchanged").into(),
            Disposition::Missing => self.tr("未安装", "Not installed").into(),
            Disposition::Protected => self.tr("受保护", "Protected").into(),
            Disposition::Unavailable => self.tr("无法扫描", "Unavailable").into(),
        }
    }

    fn cross_project_check(&self, checked: bool) -> Div {
        let p = self.palette();
        div().size(px(15.)).flex_none().rounded(px(4.)).border_1().border_color(if checked { p.accent } else { p.border_strong })
            .bg(if checked { p.accent } else { p.surface }).flex().items_center().justify_center()
            .when(checked, |d| d.child(Self::icon("icons/check.svg", 11., rgb(0xffffff))))
    }

    fn cross_project_skill_keys(&self, cx: &App) -> Vec<String> {
        let state = &self.cross_project;
        let query = state.skill_search.read(cx).value().trim().to_lowercase();
        let mut skills = state.catalog.iter().flat_map(|c| &c.skills)
            .filter(|s| state.catalog.as_ref().is_some_and(|c| skill_matches_action(c, s, state.preset_mode, state.action)))
            .filter(|s| query.is_empty() || s.name.to_lowercase().contains(&query)).collect::<Vec<_>>();
        skills.sort_by_key(|s| (!state.skills.contains(&s.key), s.name.to_lowercase(), s.key.clone()));
        skills.into_iter().map(|s| s.key.clone()).collect()
    }

    fn cross_project_visible_projects(&self, cx: &App) -> Vec<PathBuf> {
        let query = self.cross_project.project_search.read(cx).value().trim().to_lowercase();
        let mut projects = self.cross_project.catalog.iter().flat_map(|c| &c.projects).filter(|p| {
            query.is_empty() || p.name.to_lowercase().contains(&query) || p.path.to_string_lossy().to_lowercase().contains(&query)
                || p.native_workspaces.iter().any(|workspace| format!("{} {}", workspace.agent.id(), workspace.path.display()).to_lowercase().contains(&query))
                || self.cross_project.catalog.as_ref().and_then(|c| c.proma.as_ref()).is_some_and(|c| c.workspaces.iter().any(|w| {
                    w.workspace.project_root_path.as_ref().unwrap_or(&w.workspace.path) == &p.path
                        && format!("{} {} {}", w.workspace.name, w.workspace.slug, w.workspace.path.display()).to_lowercase().contains(&query)
                }))
        }).map(|p| p.path.clone()).collect::<Vec<_>>();
        projects.sort_by_key(|path| !(self.cross_project.projects.contains(path) || self.cross_project.catalog.as_ref()
            .is_some_and(|c| c.workspace_ids(path).iter().any(|id| self.cross_project.workspaces.contains(id)))));
        projects
    }

    fn cross_project_skills_picker(&self, height: f32, cx: &mut Context<Self>) -> Div {
        let p = self.palette(); let state = &self.cross_project;
        let visible = self.cross_project_skill_keys(cx);
        let mut list = div().id("cross-project-skills-list").h(px(height)).overflow_y_scroll().track_scroll(&state.skill_scroll);
        if let Some(catalog) = &state.catalog {
            for (index, key) in visible.iter().enumerate() {
                let Some(skill) = catalog.skills.iter().find(|s| &s.key == key) else { continue; };
                let checked = state.skills.contains(key); let key = key.clone();
                let provider = match skill.provider { "library" | "managed" => self.tr("技能库", "Library"), "builtin" => self.tr("Proma 内置预设", "Proma built-in"),
                    "integration" => self.tr("集成维护", "Provider-managed"), "link" => self.tr("未纳管链接", "Unregistered link"),
                    "broken-link" => self.tr("失效链接", "Broken link"), _ => self.tr("项目来源", "Project source") };
                let source = format!("{provider} · {}", display_path(&skill.source));
                let tip = source.clone();
                list = list.child(gpui_base::Button::new(ElementId::Name(format!("cross-project-skill-{index}").into()))
                    .accessibility_label(format!("{} · {}", skill.name, if checked { self.tr("已选", "Selected") } else { self.tr("未选", "Not selected") }))
                    .w_full().border_0().px(px(10.)).py(px(8.)).rounded(px(6.)).flex().items_center().gap(px(9.))
                    .bg(if checked { p.selected } else { rgba(0x00000000) }).hover(move |d| d.bg(p.hover)).disabled(state.busy || state.loading)
                    .child(self.cross_project_check(checked))
                    .child(div().flex_1().min_w_0().text_color(p.text).text_size(px(12.))
                        .child(div().truncate().child(skill.name.clone()))
                        .child(div().mt(px(3.)).text_size(px(10.)).text_color(p.muted).truncate().child(source)))
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.cross_project.skills.remove(&key) { this.cross_project.skills.insert(key.clone()); }
                        this.update_cross_project_plan(cx);
                    })));
            }
        }
        if visible.is_empty() { list = list.child(div().p(px(12.)).text_size(px(12.)).text_color(p.muted).child(self.tr("没有匹配的技能", "No matching skills"))); }
        let all = !visible.is_empty() && visible.iter().all(|key| state.skills.contains(key));
        div().flex_1().min_w_0().rounded(px(10.)).border_1().border_color(p.border).overflow_hidden()
            .child(div().p(px(10.)).child(div().flex().items_center()
                .child(div().flex_1().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child(self.tr("选择技能", "Skills")))
                .child(div().text_size(px(11.)).text_color(p.muted).child(format!("{} / {}", state.skills.len(), visible.len()))))
                .child(Input::new(&state.skill_search).small().w_full().mt(px(8.)).prefix(Self::icon("icons/search.svg", 14., p.muted)))
                .child(self.toolbar_text_button("cross-project-select-skills", if all { self.tr("取消当前结果", "Deselect visible") } else { self.tr("全选当前结果", "Select visible") })
                    .debug_selector(|| "cross-project-select-skills".into()).mt(px(5.)).disabled(state.busy || state.loading || visible.is_empty())
                    .on_click(cx.listener(move |this, _, _, cx| { toggle_visible(&mut this.cross_project.skills, &visible); this.update_cross_project_plan(cx); }))))
            .child(list)
    }

    fn cross_project_projects_picker(&self, height: f32, cx: &mut Context<Self>) -> Div {
        let p = self.palette(); let state = &self.cross_project;
        let visible = self.cross_project_visible_projects(cx);
        let ordinary = state.agents.iter().any(|id| id != "proma"); let proma = state.agents.contains("proma");
        let mut selectable_projects = Vec::new(); let mut selectable_workspaces = Vec::new();
        let mut listed_native = BTreeSet::new();
        let mut list = div().id("cross-project-projects-list").h(px(height)).overflow_y_scroll().track_scroll(&state.project_scroll);
        if let Some(catalog) = &state.catalog {
            for (index, path) in visible.iter().enumerate() {
                let Some(project) = catalog.projects.iter().find(|p| &p.path == path) else { continue; };
                let query = state.project_search.read(cx).value().trim().to_lowercase();
                let project_matches = query.is_empty() || project.name.to_lowercase().contains(&query) || path.to_string_lossy().to_lowercase().contains(&query);
                let ids = catalog.proma_workspace_ids(path).into_iter().filter(|id| project_matches || catalog.proma.as_ref().is_some_and(|p| p.workspaces.iter().any(|w| {
                    &w.workspace.id == id && format!("{} {} {}", w.workspace.name, w.workspace.slug, w.workspace.path.display()).to_lowercase().contains(&query)
                }))).collect::<Vec<_>>();
                let native = project.native_workspaces.iter().filter(|workspace| state.agents.contains(workspace.agent.id()))
                    .filter(|workspace| project_matches || format!("{} {}", workspace.agent.id(), workspace.path.display()).to_lowercase().contains(&query))
                    .filter(|workspace| listed_native.insert(workspace.id()))
                    .collect::<Vec<_>>();
                let checked = state.projects.contains(path) && ordinary; let unavailable = !path.is_dir();
                if ordinary && !unavailable { selectable_projects.push(path.clone()); }
                if proma { selectable_workspaces.extend(ids.iter().cloned()); }
                selectable_workspaces.extend(native.iter().map(|workspace| workspace.id()));
                let project_path = path.clone(); let expanded = !state.collapsed.contains(path);
                let mut heading = div().flex().items_center().gap(px(5.)).px(px(5.));
                if (proma && !ids.is_empty()) || !native.is_empty() {
                    let expand_path = path.clone();
                    heading = heading.child(self.toolbar_text_button(ElementId::Name(format!("cross-project-expand-{index}").into()), if expanded { "⌄" } else { "›" })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.cross_project.collapsed.remove(&expand_path) { this.cross_project.collapsed.insert(expand_path.clone()); }
                            this.notify_dialog(cx);
                        })));
                }
                heading = heading.child(gpui_base::Button::new(ElementId::Name(format!("cross-project-project-{index}").into()))
                    .accessibility_label(format!("{} · {}", project.name, if checked { self.tr("已选项目", "Selected project") } else { self.tr("项目", "Project") }))
                    .w_full().min_w_0().border_0().px(px(5.)).py(px(8.)).rounded(px(6.)).flex().items_center().gap(px(9.))
                    .bg(if checked { p.selected } else { rgba(0x00000000) }).hover(move |d| d.bg(p.hover))
                    .disabled(state.busy || state.loading || unavailable || !ordinary)
                    .when(ordinary, |d| d.child(self.cross_project_check(checked)))
                    .child(div().flex_1().min_w_0().text_color(if unavailable { p.muted } else { p.text }).text_size(px(12.))
                        .child(div().truncate().child(format!("{}{}", project.name, if !project.is_active() { self.tr(" · 已归档/隐藏", " · Archived/hidden") } else { "" })))
                        .child(div().mt(px(3.)).text_size(px(10.)).text_color(p.muted).truncate().child(display_path(path))))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.cross_project.projects.remove(&project_path) { this.cross_project.projects.insert(project_path.clone()); }
                        this.update_cross_project_plan(cx);
                    })));
                list = list.child(heading);
                if proma && expanded {
                    if ids.is_empty() {
                        list = list.child(div().pl(px(36.)).pb(px(6.)).text_size(px(10.)).text_color(p.muted).child(self.tr("未关联 Proma 工作区", "No linked Proma workspace")));
                    }
                    for id in ids {
                        let Some(entry) = catalog.proma.as_ref().and_then(|c| c.workspaces.iter().find(|w| w.workspace.id == id)) else { continue; };
                        let checked = state.workspaces.contains(&id); let has_error = entry.error.is_some(); let key = id.clone();
                        list = list.child(gpui_base::Button::new(ElementId::Name(format!("cross-project-workspace-{id}").into()))
                            .debug_selector(move || format!("cross-project-workspace-{key}"))
                            .accessibility_label(format!("Proma · {} · {}", entry.workspace.name, if checked { self.tr("已选工作区", "Selected workspace") } else { self.tr("工作区", "Workspace") }))
                            .w_full().border_0().pl(px(35.)).pr(px(10.)).py(px(6.)).flex().items_center().gap(px(9.))
                            .bg(if checked { p.selected } else { rgba(0x00000000) }).hover(move |d| d.bg(p.hover)).disabled(state.busy || state.loading || has_error)
                            .child(self.cross_project_check(checked))
                            .child(div().flex_1().min_w_0().text_size(px(11.)).text_color(if has_error { p.muted } else { p.secondary })
                                .child(div().truncate().child(format!("Proma · {}{}", entry.workspace.name, if entry.workspace.archived { self.tr(" · 已归档", " · Archived") } else { "" })))
                                .child(div().text_size(px(10.)).mt(px(2.)).text_color(p.muted).truncate().child(entry.error.clone().unwrap_or_else(|| entry.workspace.slug.clone()))))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.cross_project.workspaces.remove(&id) { this.cross_project.workspaces.insert(id.clone()); }
                                this.update_cross_project_plan(cx);
                            })));
                    }
                }
                if expanded {
                    for workspace in native {
                        let id = workspace.id(); let checked = state.workspaces.contains(&id);
                        let label = format!("{} · {}{}", workspace.agent.id(), workspace.display_name.clone().or_else(|| workspace.source_id.clone())
                            .unwrap_or_else(|| workspace.path.file_name().unwrap_or_default().to_string_lossy().into_owned()),
                            if workspace.archived == Some(true) { self.tr(" · 宿主已归档", " · Host archived") } else { "" });
                        let detail = format!("{} · {}", self.tr("包含项目", "Projects"), workspace.project_roots.len());
                        let mut tip = format!("{}\n{}", display_path(&workspace.path), workspace.project_roots.iter().take(12).map(|root| display_path(root)).collect::<Vec<_>>().join("\n"));
                        if workspace.project_roots.len() > 12 { tip.push_str(&format!("\n+{}", workspace.project_roots.len() - 12)); }
                        list = list.child(gpui_base::Button::new(ElementId::Name(format!("cross-project-native-{index}-{id}").into()))
                            .accessibility_label(format!("{label} · {detail} · {}", if checked { self.tr("已选工作区", "Selected workspace") } else { self.tr("工作区", "Workspace") }))
                            .w_full().border_0().pl(px(35.)).pr(px(10.)).py(px(6.)).flex().items_center().gap(px(9.))
                            .bg(if checked { p.selected } else { rgba(0x00000000) }).hover(move |d| d.bg(p.hover)).disabled(state.busy || state.loading)
                            .child(self.cross_project_check(checked))
                            .child(div().flex_1().min_w_0().text_size(px(11.)).text_color(p.secondary)
                                .child(div().truncate().child(label))
                                .child(div().text_size(px(10.)).mt(px(2.)).text_color(p.muted).truncate().child(detail)))
                            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.cross_project.workspaces.remove(&id) { this.cross_project.workspaces.insert(id.clone()); }
                                this.update_cross_project_plan(cx);
                            })));
                    }
                }
            }
        }
        if visible.is_empty() { list = list.child(div().p(px(12.)).text_size(px(12.)).text_color(p.muted).child(self.tr("没有匹配的项目", "No matching projects"))); }
        let all = (!selectable_projects.is_empty() || !selectable_workspaces.is_empty())
            && selectable_projects.iter().all(|p| state.projects.contains(p)) && selectable_workspaces.iter().all(|id| state.workspaces.contains(id));
        div().flex_1().min_w_0().rounded(px(10.)).border_1().border_color(p.border).overflow_hidden()
            .child(div().p(px(10.)).child(div().flex().items_center()
                .child(div().flex_1().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child(self.tr("选择项目 / 工作区", "Projects / workspaces")))
                .child(div().text_size(px(11.)).text_color(p.muted).child(if self.uses_english() { format!("{} projects · {} workspaces", if ordinary { state.projects.len() } else { 0 }, selectable_workspaces.iter().filter(|id| state.workspaces.contains(*id)).collect::<BTreeSet<_>>().len()) }
                    else { format!("{} 项目 · {} 工作区", if ordinary { state.projects.len() } else { 0 }, selectable_workspaces.iter().filter(|id| state.workspaces.contains(*id)).collect::<BTreeSet<_>>().len()) })))
                .child(Input::new(&state.project_search).small().w_full().mt(px(8.)).prefix(Self::icon("icons/search.svg", 14., p.muted)))
                .child(self.toolbar_text_button("cross-project-select-projects", if all { self.tr("取消当前结果", "Deselect visible") } else { self.tr("全选当前结果", "Select visible") })
                    .debug_selector(|| "cross-project-select-projects".into()).mt(px(5.)).disabled(state.busy || state.loading || (!ordinary && !proma))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if all { this.cross_project.projects.retain(|p| !selectable_projects.contains(p)); this.cross_project.workspaces.retain(|id| !selectable_workspaces.contains(id)); }
                        else { this.cross_project.projects.extend(selectable_projects.iter().cloned()); this.cross_project.workspaces.extend(selectable_workspaces.iter().cloned()); }
                        this.update_cross_project_plan(cx);
                    }))))
            .child(list)
    }

    fn cross_project_items(&self, plan: &Plan, result: Option<&BatchResult>) -> impl IntoElement {
        let p = self.palette();
        let mut list = div().id("cross-project-impact-list").max_h(px(340.)).overflow_y_scroll();
        let mut items = plan.items.iter().enumerate().collect::<Vec<_>>();
        items.sort_by_key(|(_, item)| item.disposition != Disposition::Change);
        for (index, item) in items {
            let outcome = result.and_then(|r| r.outcomes.get(index));
            let status = outcome.map(|o| if o.error.is_some() { self.tr("失败", "Failed").into() } else if o.changed { self.tr("成功", "Succeeded").into() }
                else { self.cross_project_status(if item.disposition == Disposition::Change { Disposition::Unchanged } else { item.disposition }, plan.action) }).unwrap_or_else(|| self.cross_project_status(item.disposition, plan.action));
            let agents = item.affected_agents.iter().map(|a| a.name).collect::<Vec<_>>().join("、");
            let other_agents = item.affected_agents.iter().any(|a| !self.cross_project.agents.contains(a.id));
            let reason = outcome.and_then(|o| o.error.clone()).or(item.reason.clone());
            let operation = self.cross_project_operation_label(item, plan.action);
            let path_tip = display_path(&item.path);
            list = list.child(div().py(px(8.)).border_b_1().border_color(p.border)
                .child(div().flex().items_center().gap(px(8.)).text_size(px(12.))
                    .child(div().min_w_0().flex_1().truncate().child(format!("{} · {}", item.skill.name, item.location)))
                    .child(div().text_color(if outcome.is_some_and(|o| o.error.is_some()) { p.danger } else { p.secondary }).child(status)))
                .child(div().mt(px(3.)).text_size(px(11.)).text_color(if other_agents { p.warning } else { p.secondary })
                    .child(format!("{}：{agents}{}", self.tr("影响 Agent", "Affected agents"), if other_agents { self.tr("（包含共享目录使用者）", " (includes shared readers)") } else { "" })))
                .when(item.disposition == Disposition::Change, |d| d.child(div().mt(px(3.)).text_size(px(11.)).font_weight(FontWeight::MEDIUM).text_color(p.text).child(operation)))
                .child(div().id(ElementId::Name(format!("cross-project-path-{index}").into())).mt(px(3.)).truncate().font_family(MONO).text_size(px(10.)).text_color(p.muted)
                    .child(display_path(&item.path)).tooltip(move |window, cx| Tooltip::new(path_tip.clone()).build(window, cx)))
                .children(item.instruction_paths.iter().map(|path| div().mt(px(3.)).text_size(px(10.)).text_color(p.muted)
                    .child(format!("{}：{}", self.tr("适配加载入口", "Adaptation entry"), display_path(path)))))
                .children(item.native_workspace_paths.iter().map(|path| div().mt(px(3.)).text_size(px(10.)).text_color(p.muted)
                    .child(format!("{}：{}", self.tr("工作区", "Workspace"), display_path(path)))))
                .when(reason.is_some(), |d| d.child(div().mt(px(3.)).text_size(px(11.)).text_color(p.muted).child(reason.unwrap_or_default()))));
        }
        list
    }

    pub(super) fn cross_project_modal(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let p = self.palette(); let state = &self.cross_project;
        let mut background = p.elevated; background.a = 1.;
        let busy = state.loading || state.busy;
        let mut content = div().id("cross-project-content").px(px(20.)).pb(px(12.))
            .max_h(px((f32::from(window.bounds().size.height) - 180.).max(200.))).overflow_y_scroll();
        if let Some(result) = &state.result {
            content = content.child(div().mb(px(8.)).text_size(px(13.)).text_color(if result.failed() > 0 { p.warning } else { p.success })
                .child(if self.uses_english() { format!("{} changed · {} failed · {} skipped", result.changed(), result.failed(), result.outcomes.len() - result.changed() - result.failed()) }
                    else { format!("成功 {} 份 · 失败 {} 份 · 跳过 {} 项", result.changed(), result.failed(), result.outcomes.len() - result.changed() - result.failed()) }));
            let result_plan = Plan { action: result.action, items: result.outcomes.iter().map(|o| o.item.clone()).collect() };
            content = content.child(self.cross_project_items(&result_plan, Some(result)));
        } else if state.reviewed {
            if let Some(plan) = &state.plan {
                content = content.child(div().mb(px(10.)).p(px(10.)).rounded(px(10.)).bg(p.raised).text_size(px(12.))
                    .child(self.cross_project_summary(plan)))
                    .child(div().mb(px(6.)).text_size(px(12.)).font_weight(FontWeight::MEDIUM).child(self.tr("实际影响清单", "Actual impact")))
                    .child(self.cross_project_items(plan, None));
            }
            if let Some(error) = &state.error {
                content = content.child(div().mt(px(8.)).text_size(px(12.)).text_color(p.warning).child(error.clone()));
            }
            if state.conversion_paths.is_some() && state.plan.as_ref().is_some_and(|plan| plan.items.is_empty()) {
                content = content.child(div().mt(px(8.)).text_size(px(12.)).text_color(p.warning)
                    .child(self.tr("所选链接已变化，请关闭后重新检查。", "The selected links changed. Close this dialog and inspect them again.")));
            }
        } else {
            let mut modes = div().flex().items_center().gap(px(5.)).mb(px(8.));
            for (preset, id, label) in [(false, "cross-project-users", self.tr("用户技能", "User skills")), (true, "cross-project-presets", self.tr("Proma 预设", "Proma presets"))] {
                let tip = if preset { self.tr("内置预设通过启用、停用管理；集成项保留提供方管理", "Enable or disable built-in presets; integrations remain provider-managed") }
                    else { self.tr("用户技能通过安装、卸载管理；旧软链接可以移除或转换为副本", "Install or uninstall user skills; remove legacy links or convert them to copies") };
                modes = modes.child(self.toolbar_text_button(id, label).debug_selector(move || id.into())
                    .bg(if state.preset_mode == preset { p.selected } else { rgba(0x00000000) }).disabled(busy)
                    .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.cross_project.preset_mode == preset { return; }
                        this.cross_project.preset_mode = preset;
                        this.cross_project.skills.clear(); this.cross_project.result = None;
                        this.cross_project.action = if preset { Action::Enable } else { Action::Install };
                        if preset { this.cross_project.agents = BTreeSet::from(["proma".into()]); }
                        this.cross_project.skill_scroll.set_offset(point(px(0.), px(0.)));
                        this.update_cross_project_plan(cx);
                    })));
            }
            let mut actions = div().flex().items_center().gap(px(5.));
            let available_actions = if state.preset_mode { vec![Action::Enable, Action::Disable] }
                else if state.agents.contains("proma") { vec![Action::Install, Action::Remove, Action::Convert] }
                else { vec![Action::Install, Action::Remove] };
            for action in available_actions {
                let id = match action { Action::Install => "cross-project-install", Action::Remove => "cross-project-remove", Action::Convert => "cross-project-convert", Action::Enable => "cross-project-enable", Action::Disable => "cross-project-disable" };
                let supported = !(action.is_state() || action == Action::Convert) || action.supported(&state.agents);
                let hint = self.cross_project_action_hint(action);
                actions = actions.child(self.toolbar_text_button(id, self.cross_project_verb(action))
                    .debug_selector(move || id.into())
                    .bg(if state.action == action { p.selected } else { rgba(0x00000000) }).disabled(busy || !supported)
                    .tooltip(move |window, cx| Tooltip::new(hint).build(window, cx))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.cross_project.action = action;
                        if let Some(catalog) = &this.cross_project.catalog {
                            let preset = this.cross_project.preset_mode;
                            this.cross_project.skills.retain(|key| catalog.skills.iter().any(|s| &s.key == key && skill_matches_action(catalog, s, preset, action)));
                        }
                        this.update_cross_project_plan(cx);
                    })));
            }
            content = content.child(modes).child(actions)
                .child(div().mt(px(5.)).text_size(px(11.)).text_color(p.secondary).child(self.cross_project_action_hint(state.action)))
                .child(div().mt(px(8.)).text_size(px(11.)).text_color(p.muted)
                    .child(self.tr("Agent · 可管理的在前，灰色项不可选", "Agents · available first, grey items unavailable")));
            let mut agents = crate::agents::AGENT_ICON_ORDER.to_vec();
            let available_agents = self.available_project_agents();
            if state.preset_mode { agents.retain(|a| a.id == "proma"); }
            agents.sort_by_key(|a| crate::agents::display_sort_key(a.name, available_agents.contains(a)));
            let mut agent_picker = div().mt(px(5.)).flex().flex_wrap().gap(px(4.));
            for agent in agents {
                let installed = self.model.installed_agents.contains(&agent);
                let available = available_agents.contains(&agent) && !agent.global_only;
                let checked = state.agents.contains(agent.id);
                let label = if agent.global_only { self.tr("仅支持全局", "Global only") }
                    else if !installed && available { self.tr("有关联项目", "Project associated") }
                    else if !installed { self.tr("未检测到", "Not detected") } else { "" };
                let tip = format!("{}{}", agent.name, if label.is_empty() { String::new() } else { format!(" · {label}") });
                agent_picker = agent_picker.child(gpui_base::Button::new(ElementId::Name(format!("cross-project-agent-{}", agent.id).into()))
                    .debug_selector(move || format!("cross-project-agent-{}", agent.id)).accessibility_label(tip.clone())
                    .h(px(CONTROL_HEIGHT)).px(px(8.)).border_0().rounded(px(RADIUS_CONTROL)).flex().items_center().gap(px(6.))
                    .bg(if checked { p.selected } else { rgba(0x00000000) }).text_color(if available { p.text } else { p.muted })
                    .text_size(px(12.)).font_family(FONT_UI).font_weight(FontWeight::NORMAL).hover(move |d| d.bg(p.hover)).disabled(busy || (!available && !checked))
                    .child(self.cross_project_check(checked)).child(self.agent_status_icon(agent.icon_path, 14., available))
                    .child(agent.name).tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.cross_project.agents.remove(agent.id) { this.cross_project.agents.insert(agent.id.into()); }
                        this.update_cross_project_plan(cx);
                    })));
            }
            content = content.child(agent_picker)
                .child(div().mt(px(8.)).mb(px(8.)).text_size(px(11.)).text_color(p.secondary)
                    .child(self.tr("项目勾选作用于普通 Agent；Proma 请在项目下单独勾选工作区。全局继承项在全局页面管理。", "Project selections apply to other agents; select Proma workspace children separately. Manage inherited global installs in the global view.")))
                .child(div().flex().gap(px(12.))
                    .child(self.cross_project_skills_picker((f32::from(window.bounds().size.height) - 565.).clamp(110., 240.), cx))
                    .child(self.cross_project_projects_picker((f32::from(window.bounds().size.height) - 565.).clamp(110., 240.), cx)));
            if let Some(error) = &state.error {
                content = content.child(div().mt(px(8.)).text_size(px(12.)).text_color(p.warning)
                    .child(if state.agents.is_empty() { self.tr("请选择至少一个可管理的 Agent", "Select at least one available agent").to_string() } else { error.clone() }));
            }
            if let Some(plan) = &state.plan {
                content = content.child(div().mt(px(10.)).p(px(10.)).rounded(px(10.)).bg(p.raised).text_size(px(12.))
                    .child(self.cross_project_summary(plan)));
            }
        }
        let retry = state.result.as_ref().is_some_and(|r| r.failed() > 0);
        let count = if retry { state.result.as_ref().map(BatchResult::failed).unwrap_or(0) }
            else if state.result.is_some() { 0 } else { state.plan.as_ref().map(|p| p.count(Disposition::Change)).unwrap_or(0) };
        let label = if state.busy { self.tr("正在执行…", "Applying…").to_string() }
            else if state.loading || state.planning { self.tr("正在计算…", "Calculating…").to_string() }
            else if retry { if self.uses_english() { format!("Retry {count} failed") } else { format!("仅重试失败 {count} 份") } }
            else if state.result.is_some() { self.tr("已完成", "Done").into() }
            else if !state.reviewed {
                let action = match state.action { Action::Install => self.tr("预览安装", "Review installation"), Action::Remove => self.tr("预览卸载", "Review uninstall"),
                    Action::Convert => self.tr("预览转换", "Review conversion"), Action::Enable => self.tr("预览启用", "Review enabling"), Action::Disable => self.tr("预览停用", "Review disabling") };
                if self.uses_english() { format!("{action} · {count}") } else { format!("{action} · {count} 处") }
            }
            else if self.uses_english() { format!("{} {count} installations", self.cross_project_verb(state.action)) }
            else { format!("确认{} {count} 份", self.cross_project_verb(state.action)) };
        div().bg(background).text_color(p.text).font_family(FONT_UI)
            .child(div().px(px(20.)).pt(px(16.)).pb(px(10.))
                .child(div().flex().items_center().gap(px(6.))
                    .child(div().flex_1().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child(if state.conversion_paths.is_some() { self.tr("转换为副本", "Convert to copies") } else { self.tr("跨项目管理", "Manage across projects") }))
                    .child(self.toolbar_text_button("cross-project-refresh", self.tr("刷新", "Refresh")).disabled(busy).on_click(cx.listener(|this, _, _, cx| this.load_cross_project(cx))))
                    .child(self.toolbar_text_button("cross-project-close", self.tr("关闭", "Close")).disabled(state.busy).on_click(cx.listener(|this, _, _, cx| this.close_dialog(cx)))))
                .child(div().mt(px(7.)).text_size(px(12.)).text_color(p.secondary).child(if state.conversion_paths.is_some() {
                    self.tr("仅转换下列工作区中的所选软链接，Pleat 技能源与本机资料保留。确认后执行。", "Convert only the selected links in the workspaces below. Keep Pleat sources and local data; apply after confirmation.").to_string()
                } else { self.cross_project_explanation() })))
            .child(content)
            .child(div().px(px(20.)).py(px(12.)).border_t_1().border_color(p.border).flex().items_center().gap(px(7.))
                .child(div().min_w_0().flex_1().text_size(px(11.)).text_color(p.muted).child(if self.uses_english() { format!("{} skills · {} agents", state.skills.len(), state.agents.len()) }
                    else { format!("已选 {} 个技能 · {} 个 Agent", state.skills.len(), state.agents.len()) }))
                .when(state.conversion_paths.is_none() && (state.result.is_some() || state.reviewed), |d| d.child(self.toolbar_text_button("cross-project-back", self.tr("返回选择", "Back to selection")).disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.cross_project.result.is_some() { this.load_cross_project(cx); }
                        else { this.cross_project.reviewed = false; this.notify_dialog(cx); }
                    }))))
                .child(Button::new("cross-project-execute").debug_selector(|| "cross-project-execute".into()).small().primary()
                    .h(px(CONTROL_HEIGHT)).font_family(FONT_UI).font_weight(FontWeight::NORMAL).text_size(px(12.)).label(label)
                    .disabled(busy || state.planning || if !state.reviewed && state.result.is_none() { state.plan.as_ref().is_none_or(|p| p.items.is_empty()) } else { count == 0 }).on_click(cx.listener(move |this, _, _, cx| {
                        if !this.cross_project.reviewed { this.cross_project.reviewed = true; this.notify_dialog(cx); }
                        else { this.execute_cross_project(retry, cx); }
                    }))))
    }
}

#[cfg(test)]
mod tests {
    use super::toggle_visible;
    use std::collections::BTreeSet;
    #[test]
    fn select_visible_preserves_hidden_selection() {
        let mut selected = BTreeSet::from(["hidden".to_string()]);
        let visible = vec!["first".to_string(), "second".to_string()];
        toggle_visible(&mut selected, &visible); assert_eq!(selected.len(), 3);
        toggle_visible(&mut selected, &visible); assert_eq!(selected, BTreeSet::from(["hidden".to_string()]));
    }

    #[cfg(all(feature = "ui-test", unix))]
    #[test]
    fn management_context_follows_agent_changes_without_reselecting_workspaces() {
        use super::*;
        use std::fs;
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let project = base.join("Project A");
        let other = base.join("Project B");
        let proma = base.join("proma");
        fs::create_dir(&project).unwrap(); fs::create_dir(&other).unwrap();
        let mut workspaces = Vec::new();
        for (id, root, archived) in [("a", &project, false), ("review", &project, false),
            ("archive", &project, true), ("b", &other, false)] {
            fs::create_dir_all(proma.join("agent-workspaces").join(id).join("skills")).unwrap();
            workspaces.push(serde_json::json!({"id":id,"slug":id,"name":id,"projectRootPath":root,"archived":archived}));
        }
        fs::write(proma.join("agent-workspaces.json"), serde_json::to_vec(&serde_json::json!({"version":2,"workspaces":workspaces})).unwrap()).unwrap();
        let installed = crate::agents::AGENT_ICON_ORDER.iter().filter(|a| ["codex", "proma"].contains(&a.id)).copied().collect::<Vec<_>>();
        let catalog = Arc::new(Catalog::scan(vec![], vec![], installed.clone(), Some(&proma)));
        let mut test = TestAppContext::single(); test.update(gpui_component::init);
        let (app, cx) = test.add_window_view({ let data = base.join("data"); move |window, cx| KitterApp::new_in(data, window, cx) });
        app.update_in(cx, |app, _, cx| {
            app.model.installed_agents = installed;
            let state = &mut app.cross_project;
            state.catalog = Some(catalog.clone());
            state.agents = BTreeSet::from(["codex".into()]);
            state.projects = BTreeSet::from([project.clone()]);
            state.initial_project = Some(project.clone());
            state.select_initial_destinations(&catalog);
            assert!(state.workspaces.is_empty(), "Codex must not select Proma destinations");
            app.open_dialog(DialogKind::CrossProject, cx); app.update_cross_project_plan(cx);
        });
        cx.run_until_parked(); cx.refresh().unwrap();
        let bounds = cx.debug_bounds("cross-project-agent-proma").unwrap();
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked(); cx.refresh().unwrap();
        cx.read_entity(&app, |app, _| assert_eq!(app.cross_project.workspaces,
            BTreeSet::from(["a".into(), "review".into()]), "adding Proma must select this project's active workspaces"));
        let bounds = cx.debug_bounds("cross-project-workspace-a").unwrap();
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked(); cx.refresh().unwrap();
        for _ in 0..2 {
            let bounds = cx.debug_bounds("cross-project-agent-proma").unwrap();
            cx.simulate_click(bounds.center(), Modifiers::none());
            cx.run_until_parked(); cx.refresh().unwrap();
        }
        cx.read_entity(&app, |app, _| assert_eq!(app.cross_project.workspaces, BTreeSet::from(["review".into()]), "explicit deselection must survive toggling agents"));
        app.update_in(cx, |app, _, cx| {
            app.cross_project.agents = BTreeSet::from(["codex".into()]);
            app.cross_project.workspaces.clear(); app.cross_project.initial_project = Some(project.clone());
            app.update_cross_project_plan(cx);
        });
        cx.run_until_parked(); cx.refresh().unwrap();
        let bounds = cx.debug_bounds("cross-project-presets").unwrap();
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked(); cx.refresh().unwrap();
        cx.read_entity(&app, |app, _| assert_eq!(app.cross_project.workspaces, BTreeSet::from(["a".into(), "review".into()]), "preset management must inherit the same current project"));
        app.update_in(cx, |app, _, cx| {
            app.cross_project.initial_project = None; app.cross_project.projects.clear();
            app.cross_project.workspaces.clear(); app.cross_project.preset_mode = false;
            app.update_cross_project_plan(cx);
        });
        cx.run_until_parked(); cx.refresh().unwrap();
        let bounds = cx.debug_bounds("cross-project-presets").unwrap();
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();
        cx.read_entity(&app, |app, _| assert!(app.cross_project.workspaces.is_empty(), "library entry without project context must not select any workspace"));
        app.update_in(cx, |app, _, cx| {
            app.cross_project.agents = BTreeSet::from(["codex".into()]);
            app.cross_project.initial_project = Some(project.clone());
            app.update_cross_project_plan(cx);
        });
        cx.run_until_parked(); cx.refresh().unwrap();
        let bounds = cx.debug_bounds("cross-project-agent-proma").unwrap();
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();
        cx.read_entity(&app, |app, _| assert!(app.cross_project.workspaces.is_empty(), "explicitly deselected projects must not acquire workspace selections"));
    }

    #[cfg(feature = "ui-test")]
    #[test]
    fn management_install_dialog_uses_only_the_visible_project_context() {
        use super::*;
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("Project A"); let other = temp.path().join("Project B");
        std::fs::create_dir(&project).unwrap(); std::fs::create_dir(&other).unwrap();
        let mut test = TestAppContext::single(); test.update(gpui_component::init);
        let (app, cx) = test.add_window_view({ let data = temp.path().join("data"); move |window, cx| KitterApp::new_in(data, window, cx) });
        app.update_in(cx, |app, window, cx| {
            app.shell.page = Page::Projects; app.projects_view.global_project_view = false;
            app.projects_view.open_project = Some(project.clone());
            app.open_install_dialog(window, cx);
            assert!(!app.install_flow.global);
            assert_eq!(app.install_flow.selected_projects, BTreeSet::from([project.clone()]));
            app.shell.page = Page::Skills;
            app.open_install_dialog(window, cx);
            assert!(app.install_flow.global, "library install must not inherit the previously viewed project");
            assert!(app.install_flow.selected_projects.is_empty());
            app.shell.page = Page::Projects; app.projects_view.open_project = Some(other.clone());
            app.open_install_dialog(window, cx);
            assert_eq!(app.install_flow.selected_projects, BTreeSet::from([other.clone()]));
            app.projects_view.global_project_view = true;
            app.open_install_dialog(window, cx);
            assert!(app.install_flow.global);
            assert!(app.install_flow.selected_projects.is_empty(), "global management must not retain a hidden project selection");
        });
    }

    #[cfg(all(feature = "ui-test", unix))]
    #[test]
    fn project_context_is_selected_once_and_direct_conversion_keeps_its_exact_scope() {
        use super::*;
        use std::{fs, os::unix::fs::symlink};
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let source = base.join("source/alpha");
        fs::create_dir_all(source.join("local-data")).unwrap();
        fs::write(source.join("SKILL.md"), "---\nname: alpha\ndescription: Test\n---\nBody\n").unwrap();
        fs::write(source.join("local-data/keep"), "keep local data").unwrap();
        let proma = base.join("proma");
        let project = base.join("Project A");
        let other = base.join("Project B");
        fs::create_dir(&project).unwrap(); fs::create_dir(&other).unwrap();
        let mut workspaces = Vec::new();
        for (id, root, archived) in [("default", None, false), ("a", Some(&project), false),
            ("review", Some(&project), false), ("archive", Some(&project), true), ("b", Some(&other), false)] {
            let skills = proma.join("agent-workspaces").join(id).join("skills");
            fs::create_dir_all(&skills).unwrap();
            symlink(&source, skills.join("alpha")).unwrap();
            workspaces.push(serde_json::json!({"id":id,"slug":id,"name":id,"projectRootPath":root,"archived":archived}));
        }
        fs::write(proma.join("agent-workspaces.json"), serde_json::to_vec(&serde_json::json!({"version":2,"workspaces":workspaces})).unwrap()).unwrap();
        let installed = crate::agents::AGENT_ICON_ORDER.iter().filter(|a| ["codex", "proma"].contains(&a.id)).copied().collect::<Vec<_>>();
        let skill = Skill { key: crate::proma_batch::source_key(&source.join("SKILL.md")), name: "alpha".into(), source: source.clone(), provider: "library", install_name: Some("alpha".into()) };
        let catalog = Arc::new(Catalog::scan(vec![skill.clone()], vec![], installed.clone(), Some(&proma)));
        let native = proma.join("agent-workspaces/default");
        let link = proma.join("agent-workspaces/a/skills/alpha");
        let mut test = TestAppContext::single(); test.update(gpui_component::init);
        let (app, cx) = test.add_window_view({ let data = base.join("data"); move |window, cx| KitterApp::new_in(data, window, cx) });
        app.update_in(cx, |app, _, cx| {
            let state = &mut app.cross_project;
            state.catalog = Some(catalog.clone());
            state.agents = BTreeSet::from(["codex".into(), "proma".into()]);
            state.projects = BTreeSet::from([project.clone()]);
            state.initial_project = Some(project.clone());
            state.select_initial_destinations(&catalog);
            assert_eq!(state.workspaces, BTreeSet::from(["a".into(), "review".into()]));
            state.workspaces.remove("review");
            state.select_initial_destinations(&catalog);
            assert_eq!(state.workspaces, BTreeSet::from(["a".into()]), "refresh must preserve explicit deselection");
            state.workspaces.clear();
            state.projects = BTreeSet::from([native.clone()]);
            state.initial_project = Some(native.clone());
            state.select_initial_destinations(&catalog);
            assert_eq!(state.workspaces, BTreeSet::from(["default".into()]));
            assert_eq!(app.cross_project_visible_projects(cx).first(), Some(&native));
            let state = &mut app.cross_project;
            state.workspaces.clear(); state.initial_project = None;
            state.select_initial_destinations(&catalog);
            assert!(state.workspaces.is_empty(), "library entry must not select unrelated workspaces");
            state.projects = BTreeSet::from([project.clone()]);
            state.initial_project = Some(project.clone());
            state.conversion_paths = Some(BTreeSet::from([link.clone()]));
            state.skills = BTreeSet::from([skill.key.clone()]);
            state.agents = BTreeSet::from(["proma".into()]);
            state.action = Action::Convert;
            state.select_initial_destinations(&catalog);
            assert_eq!(state.workspaces, BTreeSet::from(["a".into()]));
            app.model.installed_agents = installed;
            app.open_dialog(DialogKind::CrossProject, cx);
            app.update_cross_project_plan(cx);
        });
        cx.run_until_parked(); cx.refresh().unwrap();
        cx.read_entity(&app, |app, _| {
            let plan = app.cross_project.plan.as_ref().unwrap();
            assert!(app.cross_project.reviewed);
            assert_eq!(plan.count(Disposition::Change), 1);
            assert_eq!(plan.items[0].path, link);
        });
        assert!(cx.debug_bounds("cross-project-select-projects").is_none());
        assert!(link.is_symlink(), "opening conversion must not execute it");
        app.update_in(cx, |app, _, cx| {
            app.cross_project_agents_changed();
            assert!(app.cross_project.reviewed, "availability checks must not reopen destination selection");
            app.update_cross_project_plan(cx);
        });
        cx.run_until_parked(); cx.refresh().unwrap();
        let confirm = cx.debug_bounds("cross-project-execute").unwrap();
        cx.simulate_click(confirm.center(), Modifiers::none());
        cx.run_until_parked(); cx.refresh().unwrap();
        cx.read_entity(&app, |app, _| {
            let result = app.cross_project.result.as_ref().unwrap();
            assert_eq!((result.changed(), result.failed()), (1, 0));
        });
        assert!(link.is_dir() && !link.is_symlink());
        for id in ["default", "review", "archive", "b"] {
            assert!(proma.join("agent-workspaces").join(id).join("skills/alpha").is_symlink());
        }
        assert_eq!(fs::read_to_string(source.join("local-data/keep")).unwrap(), "keep local data");
        assert!(source.join("SKILL.md").is_file());
    }

    #[cfg(feature = "ui-test")]
    #[test]
    fn mixed_agents_require_review_and_proma_workspaces_are_explicit() {
        use super::*;
        use std::fs;
        let temp = tempfile::tempdir().unwrap(); let base = temp.path().canonicalize().unwrap();
        let source = base.join("source/alpha"); let project = base.join("Project A");
        fs::create_dir_all(&source).unwrap(); fs::create_dir(&project).unwrap();
        fs::write(source.join("SKILL.md"), "---\nname: alpha\ndescription: Test\n---\nBody\n").unwrap();
        let proma = base.join("proma"); fs::create_dir_all(proma.join("agent-workspaces/a")).unwrap();
        fs::write(proma.join("agent-workspaces.json"), serde_json::to_vec(&serde_json::json!({"version":2,"workspaces":[
            {"id":"a","slug":"a","name":"Workspace A","projectRootPath":project}
        ]})).unwrap()).unwrap();
        crate::proma::apply_plan(&crate::proma::plan_install(&proma, &proma.join("agent-workspaces/a"), &source, "alpha").unwrap()).unwrap();
        let template = proma.join("fixture-app/default-skills/preset");
        let preset = proma.join("agent-workspaces/a/skills/preset");
        for path in [&template, &preset] { fs::create_dir_all(path).unwrap(); fs::write(path.join("SKILL.md"), "---\nname: preset\ndescription: Built-in\n---\nPreset\n").unwrap(); }
        crate::proma::register_test_bundled_templates(&proma, template.parent().unwrap());
        let installed = crate::agents::AGENT_ICON_ORDER.iter().filter(|a| ["codex","proma"].contains(&a.id)).copied().collect::<Vec<_>>();
        let skill = Skill { key: crate::proma_batch::source_key(&source.join("SKILL.md")), name:"alpha".into(), source:source.clone(), provider:"library", install_name:Some("alpha".into()) };
        let entry = crate::project_discovery::ProjectEntry { path:project.clone(), name:"Project A".into(), agents:vec![], proma_workspaces:vec![], native_workspaces: Vec::new(), archive_states: Vec::new(), archived:false, hidden:false, retained:false };
        let catalog = Catalog::scan(vec![skill.clone()], vec![entry], installed.clone(), Some(&proma));
        let mut test = TestAppContext::single(); test.update(gpui_component::init);
        let (app, cx) = test.add_window_view({ let data = base.join("data"); move |window, cx| KitterApp::new_in(data, window, cx) });
        app.update_in(cx, |app, _, cx| {
            app.model.installed_agents = installed;
            app.cross_project.catalog = Some(Arc::new(catalog));
            app.cross_project.skills.insert(skill.key);
            app.cross_project.agents = BTreeSet::from(["codex".into(),"proma".into()]);
            app.cross_project.projects.insert(project.clone());
            app.open_dialog(DialogKind::CrossProject, cx); app.update_cross_project_plan(cx);
        });
        cx.run_until_parked(); cx.refresh().unwrap();
        cx.read_entity(&app, |app, _| {
            assert_eq!(app.cross_project.plan.as_ref().unwrap().count(Disposition::Change), 1);
            assert!(app.cross_project.workspaces.is_empty());
            assert!(!app.cross_project.reviewed);
        });
        // User installations have no state toggle, even when Proma is selected.
        assert!(cx.debug_bounds("cross-project-enable").is_none());
        assert!(cx.debug_bounds("cross-project-disable").is_none());
        cx.read_entity(&app, |app, _| assert_eq!(app.cross_project.action, Action::Install));
        // First click only reveals the frozen impact list.
        let button = cx.debug_bounds("cross-project-execute").unwrap();
        cx.simulate_click(button.center(), Modifiers::none());
        cx.run_until_parked(); cx.refresh().unwrap();
        assert!(!project.join(".codex/skills/alpha").exists());
        cx.read_entity(&app, |app, _| assert!(app.cross_project.reviewed));
        let button = cx.debug_bounds("cross-project-execute").unwrap();
        cx.simulate_click(button.center(), Modifiers::none());
        cx.run_until_parked(); cx.refresh().unwrap();
        cx.read_entity(&app, |app, _| { let r = app.cross_project.result.as_ref().unwrap(); assert_eq!((r.changed(),r.failed()),(1,0)); });
        assert!(project.join(".codex/skills/alpha").is_symlink());
        assert!(proma.join("agent-workspaces/a/skills/alpha").exists());
        assert!(source.join("SKILL.md").is_file());
        app.update_in(cx, |app, _, cx| { app.cross_project.result = None; app.cross_project.reviewed = false; app.notify_dialog(cx); });
        cx.run_until_parked(); cx.refresh().unwrap();
        let tab = cx.debug_bounds("cross-project-presets").unwrap(); cx.simulate_click(tab.center(), Modifiers::none());
        cx.run_until_parked();
        cx.read_entity(&app, |app, _| {
            assert!(app.cross_project.preset_mode); assert_eq!(app.cross_project.agents, BTreeSet::from(["proma".into()]));
            assert!(app.cross_project.skills.is_empty());
        });
        assert!(cx.debug_bounds("cross-project-install").is_none());
        assert!(cx.debug_bounds("cross-project-enable").is_some());
        app.update_in(cx, |app, _, cx| {
            app.cross_project.skills = BTreeSet::from(["proma:builtin:preset".into()]);
            app.cross_project.workspaces = BTreeSet::from(["a".into()]);
            app.cross_project.action = Action::Disable;
            app.update_cross_project_plan(cx);
        });
        cx.run_until_parked(); cx.refresh().unwrap();
        let button = cx.debug_bounds("cross-project-execute").unwrap(); cx.simulate_click(button.center(), Modifiers::none());
        cx.run_until_parked(); cx.refresh().unwrap();
        let button = cx.debug_bounds("cross-project-execute").unwrap(); cx.simulate_click(button.center(), Modifiers::none());
        cx.run_until_parked(); cx.refresh().unwrap();
        cx.read_entity(&app, |app, _| { let r=app.cross_project.result.as_ref().unwrap(); assert_eq!((r.changed(),r.failed()),(1,0)); });
        assert!(proma.join("agent-workspaces/a/skills-inactive/preset/SKILL.md").is_file());
        assert!(proma.join("agent-workspaces/a/skills/alpha/SKILL.md").is_file());
    }
}
