use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_base::TextSelectionHandle;
use gpui_component::{
    ActiveTheme as _, Icon, IconName, IconNamed as _, IndexPath, Root, Sizable as _,
    Theme as ComponentTheme, ThemeMode as ComponentThemeMode,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    checkbox::Checkbox,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt, PopupMenuItem},
    popover::Popover,
    select::{SelectEvent, SelectItem, SelectState},
    tooltip::Tooltip,
};

use crate::{
    InstallTarget, ProjectSkill, ProjectSkillInstallation, SkillGroup, SkillLibrary, SkillSummary,
    TriggerMode, adoption,
    config::{Language, Theme},
    effective_skills::{self, AgentContextEstimate, AgentKind},
    project, source,
    tags::{TagId, TagState, load_tag_states_from, save_tag_states_to},
};

actions!(kitter, [Quit]);

fn quit(_: &Quit, cx: &mut App) {
    cx.quit();
}

mod add_actions;
mod add_flow;
mod adoption_list;
mod badges;
mod components;
mod effective_view;
mod flows;
mod install_flow;
mod layout;
mod messages;
mod motion;
mod organize_flows;
mod overlay_actions;
mod project_actions;
mod proma_sync;
mod cross_project_flow;
mod projects_page;
mod selectable_text;
mod selection_actions;
mod settings_actions;
mod settings_page;
mod skill_selection;
mod skills_page;
mod state;
mod tags_groups_actions;
mod theme;

use crate::text::counted;
use effective_view::{EffectivePluginGroup, EffectiveSkillRow, SkillOrigin, SkillSourceGroup, SkillRowsCache, SkillRowsCacheKey, skill_source_groups, same_file};
use gpui_component::resizable::ResizableState;
use skill_selection::SkillSelection;
use state::*;
use theme::*;

static CODEX_COLOR_IMAGE: OnceLock<Arc<RenderImage>> = OnceLock::new();
static CLAUDE_COLOR_IMAGE: OnceLock<Arc<RenderImage>> = OnceLock::new();
static OPENCLAW_COLOR_IMAGE: OnceLock<Arc<RenderImage>> = OnceLock::new();
static AMP_COLOR_IMAGE: OnceLock<Arc<RenderImage>> = OnceLock::new();
static ANTIGRAVITY_COLOR_IMAGE: OnceLock<Arc<RenderImage>> = OnceLock::new();
static TRAE_COLOR_IMAGE: OnceLock<Arc<RenderImage>> = OnceLock::new();
static OMP_COLOR_IMAGE: OnceLock<Arc<RenderImage>> = OnceLock::new();
static DIMAGENT_COLOR_IMAGE: OnceLock<Arc<RenderImage>> = OnceLock::new();
static WORKBUDDY_COLOR_IMAGE: OnceLock<Arc<RenderImage>> = OnceLock::new();

fn display_path(path: &Path) -> String {
    let Some(home) = dirs::home_dir() else {
        return path.display().to_string();
    };
    if path == home {
        return "~".into();
    }
    path.strip_prefix(&home)
        .map(|relative| format!("~/{}", relative.display()))
        .unwrap_or_else(|_| path.display().to_string())
}

fn display_effective_root(path: &Path, project: &Path) -> String {
    path.strip_prefix(project)
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(|relative| relative.display().to_string())
        .unwrap_or_else(|| display_path(path))
}

fn project_tag_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn unique_installation_paths(installations: &[crate::ProjectSkillInstallation]) -> Vec<PathBuf> {
    let mut keys = HashSet::new();
    installations
        .iter()
        .filter_map(|installation| {
            keys.insert(project::installation_key(&installation.path))
                .then(|| installation.path.clone())
        })
        .collect()
}

fn unique_external_sources(kinds: &[project::RemovalKind]) -> Vec<String> {
    let mut seen = HashSet::new();
    kinds
        .iter()
        .filter_map(|kind| match kind {
            project::RemovalKind::ExternalLink { source } if seen.insert(source.clone()) => {
                Some(display_path(source))
            }
            _ => None,
        })
        .collect()
}

fn effective_plugin_groups(
    estimates: &[AgentContextEstimate],
    selected_agent: Option<AgentKind>,
) -> Vec<EffectivePluginGroup> {
    effective_view::plugin_groups(estimates, selected_agent)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Skills,
    Projects,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TagScope {
    Skills,
    Projects,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DetailTab {
    Installs,
    Content,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProjectSkillsTab {
    Skills,
    Plugins,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProjectBatchAction {
    Remove,
    DisableProma,
    EnableProma,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AddKind {
    Local,
    Npx,
    Claude,
    Existing,
}

#[derive(Clone, PartialEq, Eq)]
struct SourceChoice {
    kind: AddKind,
    label: SharedString,
}

impl SelectItem for SourceChoice {
    type Value = AddKind;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn display_title(&self) -> Option<AnyElement> {
        Some(
            div()
                .font_family(FONT_UI)
                .text_size(px(13.))
                .child(self.label.clone())
                .into_any_element(),
        )
    }

    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .font_family(FONT_UI)
            .text_size(px(13.))
            .child(self.label.clone())
    }

    fn value(&self) -> &Self::Value {
        &self.kind
    }
}

#[derive(Clone, Copy)]
enum DialogKind {
    Add,
    Install,
    Delete,
    Tags,
    AssignTags,
    Groups,
    DeleteGroup,
    MoveGroup,
    CrossProject,
    Storage,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TagEdit {
    CreateRoot,
    CreateChild(TagId),
    Rename(TagId),
}

#[derive(Clone, PartialEq, Eq)]
enum GroupEdit {
    Create,
    Rename(String),
}

#[derive(Clone)]
struct TagAssignmentTarget {
    scope: TagScope,
    keys: Vec<String>,
    label: String,
}

#[derive(Clone)]
struct SkillDrag {
    name: String,
}

impl Render for SkillDrag {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(10.))
            .py(px(6.))
            .rounded(px(RADIUS_CONTROL))
            .bg(rgb(0xeeeeee))
            .font_family(MONO)
            .text_size(px(12.))
            .child(self.name.clone())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GroupDragScope {
    Management,
    List,
}

#[derive(Clone)]
struct GroupDrag {
    scope: GroupDragScope,
    id: String,
    name: String,
    background: Rgba,
    foreground: Rgba,
}

impl Render for GroupDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(10.))
            .py(px(6.))
            .rounded(px(RADIUS_CONTROL))
            .bg(self.background)
            .text_color(self.foreground)
            .font_family(MONO)
            .text_size(px(12.))
            .child(self.name.clone())
    }
}

#[derive(Clone)]
struct TagDrag {
    scope: TagScope,
    parent: Option<TagId>,
    tag_id: TagId,
    name: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TagDropPosition {
    Before,
    After,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct TagDropTarget {
    scope: TagScope,
    tag_id: TagId,
    position: TagDropPosition,
}

impl Render for TagDrag {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(10.))
            .py(px(6.))
            .rounded(px(RADIUS_CONTROL))
            .bg(rgb(0xeeeeee))
            .font_family(MONO)
            .text_size(px(12.))
            .child(format!("#{}", self.name))
    }
}

struct DialogBody {
    app: Entity<KitterApp>,
    kind: DialogKind,
}

struct PageBody {
    app: WeakEntity<KitterApp>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AddTask {
    Scanning,
    Importing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProjectChoice {
    path: PathBuf,
}

impl SelectItem for ProjectChoice {
    type Value = PathBuf;

    fn title(&self) -> SharedString {
        self.path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
            .into()
    }

    fn display_title(&self) -> Option<AnyElement> {
        Some(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .justify_center()
                .child(
                    div()
                        .font_family(FONT_UI)
                        .text_size(px(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .child(self.title()),
                )
                .child(
                    div()
                        .mt(px(2.))
                        .font_family(MONO)
                        .text_size(px(12.))
                        .text_color(rgb(0x858590))
                        .truncate()
                        .child(display_path(&self.path)),
                )
                .into_any_element(),
        )
    }

    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .min_w_0()
            .py(px(2.))
            .flex()
            .flex_col()
            .justify_center()
            .child(
                div()
                    .font_family(FONT_UI)
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .child(self.title()),
            )
            .child(
                div()
                    .mt(px(2.))
                    .font_family(MONO)
                    .text_size(px(12.))
                    .text_color(rgb(0x858590))
                    .truncate()
                    .child(display_path(&self.path)),
            )
    }

    fn value(&self) -> &Self::Value {
        &self.path
    }
}

#[derive(Clone)]
enum DeleteConfirmation {
    LibrarySkills {
        skills: Vec<(String, PathBuf)>,
    },
    ProjectSkills {
        project: PathBuf,
        skills: Vec<crate::ProjectSkill>,
        batch: bool,
    },
}

#[derive(Clone, PartialEq, Eq)]
enum ProjectSkillFilter {
    Group(String),
    Tag(TagId),
}

#[derive(Clone)]
struct ContentSnapshot {
    skill: String,
    file: PathBuf,
    files: Vec<PathBuf>,
    content: SharedString,
}

fn skill_storage_name(skill: &SkillSummary) -> &str {
    if skill.record.storage_name.is_empty() {
        &skill.record.name
    } else {
        &skill.record.storage_name
    }
}

const CONTEXT_ESTIMATE_CACHE_TTL: Duration = Duration::from_secs(2 * 60 * 60);

#[derive(Clone)]
struct ContextEstimateCache {
    scanned_at: Instant,
    estimates: Vec<AgentContextEstimate>,
}

struct SpinnerView {
    color: Rgba,
    size: f32,
}

impl Render for SpinnerView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        motion::spin(KitterApp::icon(
            "icons/rotate-cw.svg",
            self.size,
            self.color,
        ))
    }
}

pub struct KitterApp {
    model: AppModel,
    shell: ShellState,
    skills_view: SkillsState,
    projects_view: ProjectsState,
    add_flow: AddFlowState,
    install_flow: InstallFlowState,
    delete_flow: DeleteFlowState,
    tags_flow: TagsFlowState,
    groups_flow: GroupsFlowState,
    cross_project: cross_project_flow::CrossProjectState,
    settings_storage: settings_actions::StorageState,
}

impl KitterApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        if std::env::var_os("KITTER_HOME").is_none() {
            crate::config::validate_app_data_roots(
                &crate::config::preferred_app_data_dir(),
                &crate::config::legacy_app_data_dir(),
            ).unwrap_or_else(|error| panic!("无法打开 Pleat：{error:#}"));
        }
        let mut app = Self::new_in(crate::config::app_data_dir(), window, cx);
        {
            app.projects_view.discovered_projects =
                crate::project_discovery::discover_agent_projects().unwrap_or_default();
            app.normalize_open_project();
            app.sync_project_select(window, cx);
            app.projects_view.discovery_task = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(15)).await;
                    let Ok(previous) = this.read_with(cx, |this, _| this.projects_view.discovered_projects.clone()) else { break; };
                    let (agents, discovered) = cx.background_executor().spawn(async move {
                        let agents = crate::agents::installed_agents();
                        let discovered = crate::project_discovery::refresh_agent_projects(&previous);
                        (agents, discovered)
                    }).await;
                    if this.update(cx, |this, cx| {
                        if this.set_installed_agents(agents) { this.notify_dialog(cx); }
                        if let Ok(discovered) = discovered {
                            if this.projects_view.discovered_projects != discovered {
                                this.projects_view.discovered_projects = discovered;
                                this.normalize_open_project();
                                this.projects_view.scan_generation = this.projects_view.scan_generation.wrapping_add(1);
                                this.projects_view.project_snapshots.borrow_mut().clear();
                                this.projects_view.project_snapshot_tasks.borrow_mut().clear();
                                this.projects_view.context_estimates.borrow_mut().clear();
                                this.projects_view.context_estimate_tasks.borrow_mut().clear();
                                cx.notify();
                            }
                        }
                    }).is_err() { break; }
                }
            }));
        }
        app.start_local_proma_sync(cx);
        app
    }

    fn new_in(data_dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let library = SkillLibrary::open_in(&data_dir)
            .unwrap_or_else(|error| panic!("无法打开 Kitter：{error:#}"));
        let system_dark = matches!(
            cx.window_appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let component_mode = match library.config.theme {
            Theme::Light => ComponentThemeMode::Light,
            Theme::Dark => ComponentThemeMode::Dark,
            Theme::System if system_dark => ComponentThemeMode::Dark,
            Theme::System => ComponentThemeMode::Light,
        };
        ComponentTheme::change(component_mode, Some(window), cx);
        let skills = library.list().unwrap_or_default();
        let known_groups = library
            .groups()
            .into_iter()
            .map(|group| group.id)
            .collect::<HashSet<_>>();
        let selected = skills
            .first()
            .map(|skill| skill_storage_name(skill).to_string());
        let english = match library.config.language {
            Language::En => true,
            Language::ZhCn => false,
            Language::System => Language::system() == Language::En,
        };
        let skill_search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(if english {
                "Search skills"
            } else {
                "搜索技能"
            })
        });
        let project_search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(if english {
                "Search projects"
            } else {
                "搜索项目"
            })
        });
        let install_project_search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(if english {
                "Search project name or path"
            } else {
                "搜索项目名称或路径"
            })
        });
        let effective_skill_search = cx.new(|cx| InputState::new(window, cx).placeholder(if english {
            "Search skill names or descriptions"
        } else { "搜索技能名称或描述" }));
        let tag_name_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(if english {
                "Tag name"
            } else {
                "标签名称"
            })
        });
        let group_name_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(if english {
                "Group name"
            } else {
                "分组名称"
            })
        });
        let add_primary_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(if english {
                "Paste a skills.sh/GitHub URL or npx skills add command"
            } else {
                "粘贴 skills.sh、GitHub 地址或 npx skills add 命令"
            })
        });
        let add_source_select = cx.new(|cx| {
            SelectState::new(
                vec![
                    SourceChoice {
                        kind: AddKind::Npx,
                        label: "skills.sh / github".into(),
                    },
                    SourceChoice {
                        kind: AddKind::Claude,
                        label: if english {
                            "Claude plugin"
                        } else {
                            "Claude 插件"
                        }
                        .into(),
                    },
                    SourceChoice {
                        kind: AddKind::Local,
                        label: if english {
                            "Local folder"
                        } else {
                            "本地文件夹"
                        }
                        .into(),
                    },
                    SourceChoice {
                        kind: AddKind::Existing,
                        label: if english {
                            "Existing installations"
                        } else {
                            "已有安装"
                        }
                        .into(),
                    },
                ],
                Some(IndexPath::default()),
                window,
                cx,
            )
        });
        let shell_layout = cx.new(|_| ResizableState::default());
        let content_layout = cx.new(|_| ResizableState::default());
        // Panel bounds notify during prepaint too. Only invalidate the page
        // when widths actually change, not on every layout measurement.
        for state in [&shell_layout, &content_layout] {
            let mut previous = Vec::new();
            cx.observe(state, move |_, state, cx| {
                let sizes = state.read(cx).sizes();
                if *sizes != previous {
                    previous = sizes.clone();
                    cx.notify();
                }
            })
            .detach();
        }
        let project_choices = library
            .config
            .project_paths()
            .into_iter()
            .map(|path| ProjectChoice { path })
            .collect::<Vec<_>>();
        let project_select = cx.new(|cx| {
            SelectState::new(
                project_choices,
                (!library.config.project_paths().is_empty()).then(IndexPath::default),
                window,
                cx,
            )
        });
        let initial_palette = match component_mode {
            ComponentThemeMode::Dark => Palette::dark(),
            _ => Palette::light(),
        };
        let spinner_accent = cx.new(|_| SpinnerView {
            color: initial_palette.accent,
            size: 14.,
        });
        let spinner_on_accent = cx.new(|_| SpinnerView {
            color: initial_palette.on_accent,
            size: 12.,
        });
        let cross_project = cross_project_flow::CrossProjectState::new(english, window, cx);
        let app = cx.entity();
        let weak_app = app.downgrade();
        let page_body = cx.new(|cx| {
            cx.observe(&app, |_, _, cx| cx.notify()).detach();
            PageBody { app: weak_app }
        });
        let skill_page_body = page_body.clone();
        cx.subscribe_in(
            &skill_search,
            window,
            move |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.sync_skill_search_selection(cx);
                    skill_page_body.update(cx, |_, cx| cx.notify());
                }
            },
        )
        .detach();
        let project_page_body = page_body.clone();
        let effective_page_body = page_body.clone();
        cx.subscribe_in(&effective_skill_search, window, move |_, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) { effective_page_body.update(cx, |_, cx| cx.notify()); }
        }).detach();
        cx.subscribe_in(
            &install_project_search,
            window,
            |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.install_flow.project_scroll.set_offset(point(px(0.), px(0.)));
                    this.notify_dialog(cx);
                }
            },
        ).detach();
        cx.subscribe_in(
            &project_search,
            window,
            move |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    project_page_body.update(cx, |_, cx| cx.notify());
                }
            },
        )
        .detach();
        cx.subscribe_in(
            &tag_name_input,
            window,
            |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::Change => {
                    this.tags_flow.error = None;
                    this.notify_dialog(cx);
                }
                InputEvent::PressEnter { .. } => this.commit_tag_edit(cx),
                InputEvent::Focus | InputEvent::Blur => {}
            },
        )
        .detach();
        cx.subscribe_in(
            &group_name_input,
            window,
            |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::Change => {
                    this.tags_flow.error = None;
                    this.notify_dialog(cx);
                }
                InputEvent::PressEnter { .. } => this.commit_group_edit(cx),
                InputEvent::Focus | InputEvent::Blur => {}
            },
        )
        .detach();
        cx.subscribe_in(
            &project_select,
            window,
            |this, _, event: &SelectEvent<Vec<ProjectChoice>>, _, cx| {
                let SelectEvent::Confirm(Some(path)) = event else {
                    return;
                };
                this.projects_view.open_project = Some(path.clone());
                if !this.can_install_proma() {
                    this.install_flow.selected_targets.remove(&InstallTarget::Proma);
                }
                this.notify_dialog(cx);
            },
        )
        .detach();
        cx.subscribe_in(
            &add_primary_input,
            window,
            |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.notify_dialog(cx);
                }
            },
        )
        .detach();
        cx.subscribe_in(
            &add_source_select,
            window,
            |this, _, event: &SelectEvent<Vec<SourceChoice>>, window, cx| {
                let SelectEvent::Confirm(Some(kind)) = event else {
                    return;
                };
                if this.add_flow.task.is_some() {
                    return;
                }
                this.add_flow.kind = *kind;
                let english = this.uses_english();
                let placeholder = match this.add_flow.kind {
                    AddKind::Claude => {
                        if english {
                            "Plugin name or claude plugin install command"
                        } else {
                            "插件名称或 claude plugin install 命令"
                        }
                    }
                    AddKind::Npx => {
                        if english {
                            "Paste a skills.sh/GitHub URL or npx skills add command"
                        } else {
                            "粘贴 skills.sh、GitHub 地址或 npx skills add 命令"
                        }
                    }
                    AddKind::Local | AddKind::Existing => "",
                };
                this.add_flow.primary_input.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                    input.set_placeholder(placeholder, window, cx);
                });
                this.add_flow.scan = None;
                this.add_flow.adoption_scan = None;
                this.add_flow.adoption_expanded.clear();
                this.add_flow.selected.clear();
                this.add_flow.error = None;
                this.add_flow.group_enabled = true;
                this.add_flow.group_name = None;
                this.notify_dialog(cx);
            },
        )
        .detach();
        let (tags, project_tags) = load_tag_states_from(&data_dir);
        let collapsed_groups = if library.config.collapsed_skill_groups_initialized {
            library
                .config
                .collapsed_skill_groups
                .iter()
                .cloned()
                .collect()
        } else {
            known_groups.clone()
        };
        let initial_project = std::env::var_os("KITTER_OPEN_PROJECT")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute() && path.is_dir());
        // Seed the prototype's initial view when desktop input is unavailable.
        // Release builds always use the normal initial agent and group state.
        let preview_proma = cfg!(debug_assertions)
            && std::env::var("KITTER_PREVIEW_PROMA").as_deref() == Ok("1");
        let mut app = Self {
            model: AppModel {
                installed_agents: crate::agents::installed_agents(),
                library,
                skills,
                update_check: None,
                updating_skill: None,
                proma_sync_task: None,
                proma_sync_warnings: Vec::new(),
                proma_sync_running: false,
            },
            shell: ShellState {
                page: if initial_project.is_some() { Page::Projects } else { Page::Skills },
                focus_handle: cx.focus_handle(),
                header_drag_armed: false,
                shell_layout,
                content_layout,
                spinner_accent,
                spinner_on_accent,
                notice: None,
                notice_generation: 0,
                system_dark,
                dialog_body: None,
                page_body,
            },
            skills_view: SkillsState {
                expanded_description: None,
                tab: DetailTab::Installs,
                selection: SkillSelection::new(selected),
                selected_file: PathBuf::from("SKILL.md"),
                skill_search,
                search_descriptions: false,
                content_snapshot: RefCell::new(None),
                content_scroll: ScrollHandle::new(),
                selectable_text_handles: RefCell::new(BTreeMap::new()),
                known_groups,
                collapsed_groups,
                collapsed_content_directories: HashSet::new(),
            },
            cross_project,
            settings_storage: Default::default(),
            projects_view: ProjectsState {
                proma_directory_menu: None,
                selected_skill_source: None,
                discovered_projects: Vec::new(),
                discovery_task: None,
                open_project: initial_project.clone(),
                global_project_view: initial_project.is_none(),
                batch_project: None,
                batch_selected: HashSet::new(),
                batch_filter: None,
                batch_action: ProjectBatchAction::Remove,
                project_skills_tab: ProjectSkillsTab::Skills,
                selected_project_agent: preview_proma.then_some(AgentKind::Proma),
                project_agents_expanded: false,
                expanded_project_plugins: HashSet::new(),
                project_search,
                effective_skill_search,
                project_agent_filter: ProjectAgentFilter::default(),
                list_view: ProjectListView::default(),
                project_select,
                project_snapshots: RefCell::new(BTreeMap::new()),
                skill_rows_cache: RefCell::new(SkillRowsCache::default()),
                skill_list: ListState::new(0, ListAlignment::Top, px(120.)),
                skill_list_items: RefCell::new((None, None, None, 0)),
                project_snapshot_tasks: RefCell::new(BTreeMap::new()),
                context_estimates: RefCell::new(BTreeMap::new()),
                context_estimate_tasks: RefCell::new(BTreeMap::new()),
                scan_generation: 0,
            },
            add_flow: AddFlowState {
                kind: AddKind::Npx,
                task: None,
                scan: None,
                adoption_scan: None,
                adoption_rows: Vec::new(),
                adoption_list: ListState::new(0, ListAlignment::Top, px(80.)),
                adoption_cancel: None,
                adoption_expanded: HashSet::new(),
                adoption_root: None,
                selected: HashSet::new(),
                error: None,
                group_enabled: true,
                group_name: None,
                primary_input: add_primary_input,
                source_select: add_source_select,
            },
            install_flow: InstallFlowState {
                modal: false,
                global: true,
                selected_projects: BTreeSet::new(),
                project_picker_open: false,
                project_search: install_project_search,
                project_scroll: ScrollHandle::new(),
                selected_targets: HashSet::new(),
            },
            delete_flow: DeleteFlowState {
                confirmation: None,
                selected: HashSet::new(),
                busy: false,
            },
            tags_flow: TagsFlowState {
                name_input: tag_name_input,
                skills: tags,
                projects: project_tags,
                selected_skill_filter: None,
                selected_project_filter: None,
                scope: TagScope::Skills,
                drop_target: None,
                edit: None,
                delete_pending: None,
                error: None,
                assignment_keys: Vec::new(),
                assignment_label: None,
                return_to_assignment: None,
            },
            groups_flow: GroupsFlowState {
                drop_target: None,
                name_input: group_name_input,
                edit: None,
                delete_pending: None,
                delete_skills: false,
                move_skills: Vec::new(),
            },
        };
        if cfg!(debug_assertions)
            && std::env::var("KITTER_PREVIEW_MULTI_INSTALL").as_deref() == Ok("1") {
            app.shell.page = Page::Skills;
            let keys = app.model.skills.iter().filter(|skill| !skill.record.origin.is_builtin()).take(2)
                .map(|skill| skill_storage_name(skill).to_string()).collect::<Vec<_>>();
            app.skills_view.selection.replace(&keys);
            app.install_flow.global = false;
            app.install_flow.modal = true;
            app.install_flow.selected_projects = app.model.library.config.project_paths()
                .into_iter().take(2).collect();
            app.install_flow.selected_targets = HashSet::from([InstallTarget::Codex, InstallTarget::Proma]);
            app.open_dialog(DialogKind::Install, cx);
        }
        if cfg!(debug_assertions) && std::env::var("KITTER_PREVIEW_PROMA_BATCH").as_deref() == Ok("1") {
            let keys = app.model.skills.iter().filter(|skill| matches!(skill.record.name.as_str(), "humanizer-zh" | "pdf" | "writing-plans"))
                .map(|skill| crate::proma_batch::source_key(&skill.path.join("SKILL.md"))).collect();
            app.open_cross_project(keys, None, crate::cross_project::Action::Disable, Some("proma".into()), window, cx);
        }
        if cfg!(debug_assertions) && std::env::var("KITTER_PREVIEW_CROSS_PROJECT").as_deref() == Ok("1") {
            let keys = app.model.skills.iter().filter(|s| matches!(s.record.name.as_str(), "humanizer-zh" | "pdf" | "writing-plans"))
                .map(|s| crate::proma_batch::source_key(&s.path.join("SKILL.md"))).collect();
            app.open_cross_project(keys, None, crate::cross_project::Action::Install, None, window, cx);
        }
        app
    }

    fn uses_dark_theme(&self) -> bool {
        match self.model.library.config.theme {
            Theme::Light => false,
            Theme::Dark => true,
            Theme::System => self.shell.system_dark,
        }
    }

    fn palette(&self) -> Palette {
        if self.uses_dark_theme() {
            Palette::dark()
        } else {
            Palette::light()
        }
    }

    fn content_pane_width(&self, window: &Window, cx: &App) -> f32 {
        let sidebar = self
            .shell
            .shell_layout
            .read(cx)
            .sizes()
            .first()
            .copied()
            .unwrap_or(px(layout::SIDEBAR_WIDTH));
        f32::from(window.bounds().size.width) - f32::from(sidebar).clamp(160., 260.)
    }

    fn tr(&self, zh: &'static str, en: &'static str) -> &'static str {
        if self.uses_english() { en } else { zh }
    }

    fn selectable_text(
        &self,
        id: impl Into<String>,
        document_order: u64,
        text: impl Into<SharedString>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> selectable_text::SelectableText {
        let id = id.into();
        let text = text.into();
        let focus_handle = self.shell.focus_handle.clone();
        let selection = {
            let mut handles = self.skills_view.selectable_text_handles.borrow_mut();
            handles
                .entry(id)
                .or_insert_with(|| {
                    let selection = TextSelectionHandle::new(text.to_string(), cx);
                    selection.refresh_window_on_change(window, cx).detach();
                    selection.focus_with(
                        move |window, cx| {
                            focus_handle.focus(window, cx);
                        },
                        cx,
                    );
                    selection
                })
                .clone()
        };
        selection.set_fallback_copy_text(text.to_string(), cx);
        selectable_text::SelectableText::new(selection, document_order, text)
    }

    fn uses_english(&self) -> bool {
        match self.model.library.config.language {
            Language::En => true,
            Language::ZhCn => false,
            Language::System => Language::system() == Language::En,
        }
    }

    fn tags_for(&self, scope: TagScope) -> &TagState {
        match scope {
            TagScope::Skills => &self.tags_flow.skills,
            TagScope::Projects => &self.tags_flow.projects,
        }
    }

    fn tags_for_mut(&mut self, scope: TagScope) -> &mut TagState {
        match scope {
            TagScope::Skills => &mut self.tags_flow.skills,
            TagScope::Projects => &mut self.tags_flow.projects,
        }
    }

    fn persist_tags(&self) {
        let _ = save_tag_states_to(
            self.model.library.data_dir(),
            &self.tags_flow.skills,
            &self.tags_flow.projects,
        );
    }

    fn persist_collapsed_groups(&mut self) {
        self.model.library.config.collapsed_skill_groups =
            self.skills_view.collapsed_groups.iter().cloned().collect();
        self.model.library.config.collapsed_skill_groups_initialized = true;
        let _ = self.model.library.save();
    }

    fn tag_filter_for(&self, scope: TagScope) -> Option<TagId> {
        match scope {
            TagScope::Skills => self.tags_flow.selected_skill_filter,
            TagScope::Projects => self.tags_flow.selected_project_filter,
        }
    }

    fn set_tag_filter(&mut self, scope: TagScope, filter: Option<TagId>) {
        match scope {
            TagScope::Skills => self.tags_flow.selected_skill_filter = filter,
            TagScope::Projects => self.tags_flow.selected_project_filter = filter,
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.model.skills = self.model.library.list().unwrap_or_default();
        let previous_collapsed_groups = self.skills_view.collapsed_groups.clone();
        let current_groups = self
            .model
            .library
            .groups()
            .into_iter()
            .map(|group| group.id)
            .collect::<HashSet<_>>();
        self.skills_view.collapsed_groups.extend(
            current_groups
                .difference(&self.skills_view.known_groups)
                .cloned(),
        );
        self.skills_view
            .collapsed_groups
            .retain(|id| current_groups.contains(id));
        self.skills_view.known_groups = current_groups;
        if self.skills_view.collapsed_groups != previous_collapsed_groups {
            self.persist_collapsed_groups();
        }
        self.projects_view.scan_generation = self.projects_view.scan_generation.wrapping_add(1);
        self.projects_view.project_snapshots.borrow_mut().clear();
        self.projects_view
            .project_snapshot_tasks
            .borrow_mut()
            .clear();
        self.projects_view.context_estimates.borrow_mut().clear();
        self.projects_view
            .context_estimate_tasks
            .borrow_mut()
            .clear();
        *self.skills_view.content_snapshot.borrow_mut() = None;
        let order = self
            .model
            .skills
            .iter()
            .map(|skill| skill_storage_name(skill).to_string())
            .collect::<Vec<_>>();
        self.skills_view.selection.reconcile(&order);
        self.sync_skill_search_selection(cx);
        cx.notify();
    }

    fn sync_spinner_palette(&self, cx: &mut Context<Self>) {
        let p = self.palette();
        self.shell.spinner_accent.update(cx, |spinner, cx| {
            spinner.color = p.accent;
            cx.notify();
        });
        self.shell.spinner_on_accent.update(cx, |spinner, cx| {
            spinner.color = p.on_accent;
            cx.notify();
        });
    }

    fn project_snapshot(
        &self,
        path: &PathBuf,
        cx: &mut Context<Self>,
    ) -> Option<Vec<ProjectSkill>> {
        if let Some(snapshot) = self.projects_view.project_snapshots.borrow().get(path) {
            return Some(snapshot.clone());
        }

        self.request_project_snapshots(std::slice::from_ref(path), cx);
        None
    }

    fn request_project_snapshots(&self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let generation = self.projects_view.scan_generation;
        let mut pending = Vec::new();
        {
            let snapshots = self.projects_view.project_snapshots.borrow();
            let tasks = self.projects_view.project_snapshot_tasks.borrow();
            for path in paths {
                if !snapshots.contains_key(path)
                    && !tasks
                        .get(path)
                        .is_some_and(|running| *running == generation)
                {
                    pending.push(path.clone());
                }
            }
        }
        if pending.is_empty() {
            return;
        }
        {
            let mut tasks = self.projects_view.project_snapshot_tasks.borrow_mut();
            for path in &pending {
                tasks.insert(path.clone(), generation);
            }
        }

        let library_dir = self.model.library.config.library_dir.clone();
        cx.spawn(async move |this, cx| {
            let snapshots = cx
                .background_executor()
                .spawn(async move {
                    pending
                        .into_iter()
                        .map(|path| {
                            let snapshot = project::list(&path, &library_dir).unwrap_or_default();
                            (path, snapshot)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let mut changed = false;
                for (path, snapshot) in snapshots {
                    let is_current = this.projects_view.scan_generation == generation
                        && this
                            .projects_view
                            .project_snapshot_tasks
                            .borrow()
                            .get(&path)
                            .is_some_and(|running| *running == generation);
                    if !is_current {
                        continue;
                    }
                    this.projects_view
                        .project_snapshot_tasks
                        .borrow_mut()
                        .remove(&path);
                    this.projects_view
                        .project_snapshots
                        .borrow_mut()
                        .insert(path, snapshot);
                    changed = true;
                }
                if changed {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn context_estimate_snapshot(
        &self,
        path: &PathBuf,
        cx: &mut Context<Self>,
    ) -> Option<Vec<AgentContextEstimate>> {
        let cached = self
            .projects_view
            .context_estimates
            .borrow()
            .get(path)
            .cloned();
        if cached
            .as_ref()
            .is_some_and(|cache| cache.scanned_at.elapsed() < CONTEXT_ESTIMATE_CACHE_TTL)
        {
            return cached.map(|cache| cache.estimates);
        }

        let generation = self.projects_view.scan_generation;
        if !self
            .projects_view
            .context_estimate_tasks
            .borrow()
            .get(path)
            .is_some_and(|running| *running == generation)
        {
            self.projects_view
                .context_estimate_tasks
                .borrow_mut()
                .insert(path.clone(), generation);
            let path = path.clone();
            let agents = self.available_project_agents();
            let discovered = self.projects_view.discovered_projects.clone();
            cx.spawn(async move |this, cx| {
                let worker_path = path.clone();
                let estimates = cx
                    .background_executor()
                    .spawn(async move { effective_skills::estimate_project_with_catalog(&worker_path, &agents, &discovered) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    let is_current = this.projects_view.scan_generation == generation
                        && this
                            .projects_view
                            .context_estimate_tasks
                            .borrow()
                            .get(&path)
                            .is_some_and(|running| *running == generation);
                    if !is_current {
                        return;
                    }
                    this.projects_view
                        .context_estimate_tasks
                        .borrow_mut()
                        .remove(&path);
                    if this.projects_view.open_project.as_ref() == Some(&path) {
                        this.projects_view.selected_project_agent = this.projects_view.selected_project_agent
                            .filter(|agent| estimates.iter().any(|estimate| estimate.agent == *agent));
                    }
                    this.projects_view.context_estimates.borrow_mut().insert(
                        path,
                        ContextEstimateCache {
                            scanned_at: Instant::now(),
                            estimates,
                        },
                    );
                    cx.notify();
                });
            })
            .detach();
        }

        cached.map(|cache| cache.estimates)
    }

    fn refresh_context_estimate(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.refresh_installed_agents();
        self.projects_view.scan_generation = self.projects_view.scan_generation.wrapping_add(1);
        // External Agent updates can change installation state and origin too.
        self.projects_view.project_snapshots.borrow_mut().remove(&path);
        self.projects_view.project_snapshot_tasks.borrow_mut().clear();
        self.projects_view
            .context_estimate_tasks
            .borrow_mut()
            .clear();
        self.projects_view
            .context_estimates
            .borrow_mut()
            .remove(&path);
        cx.notify();
    }

    fn refresh_installed_agents(&mut self) {
        self.set_installed_agents(crate::agents::installed_agents());
    }

    pub(super) fn available_project_agents(&self) -> Vec<crate::agents::AgentIconInfo> {
        crate::agents::project_agents(&self.model.installed_agents, &self.all_project_entries())
    }

    fn set_installed_agents(&mut self, agents: Vec<crate::agents::AgentIconInfo>) -> bool {
        if agents == self.model.installed_agents { return false; }
        self.model.installed_agents = agents;
        let projects = self.all_project_entries();
        let available = |kind: &AgentKind| ProjectAgentFilter::agent_available(*kind, &self.model.installed_agents, &projects);
        self.projects_view.selected_project_agent = self.projects_view.selected_project_agent.filter(available);
        self.projects_view.project_agent_filter.retain_available(&self.model.installed_agents, &projects);
        let manageable = crate::agents::project_agents(&self.model.installed_agents, &projects);
        self.install_flow.selected_targets.retain(|target| *target == InstallTarget::Universal
            || manageable.iter().any(|agent| agent.owns_target(*target)));
        self.cross_project_agents_changed();
        self.projects_view.scan_generation = self.projects_view.scan_generation.wrapping_add(1);
        self.projects_view.context_estimate_tasks.borrow_mut().clear();
        self.projects_view.context_estimates.borrow_mut().clear();
        true
    }

    fn content_snapshot(&self, skill: &SkillSummary) -> ContentSnapshot {
        let storage_name = skill_storage_name(skill);
        if let Some(snapshot) = self.skills_view.content_snapshot.borrow().as_ref()
            && snapshot.skill == storage_name
            && snapshot.file == self.skills_view.selected_file
        {
            return snapshot.clone();
        }
        let files = self
            .model
            .library
            .files_by_storage(storage_name)
            .unwrap_or_default();
        let content = self
            .model
            .library
            .read_file_by_storage(storage_name, &self.skills_view.selected_file)
            .unwrap_or_else(|error| self.error_message(error));
        let snapshot = ContentSnapshot {
            skill: storage_name.to_string(),
            file: self.skills_view.selected_file.clone(),
            files,
            content: content.into(),
        };
        *self.skills_view.content_snapshot.borrow_mut() = Some(snapshot.clone());
        snapshot
    }

    fn set_language(&mut self, language: Language, window: &mut Window, cx: &mut Context<Self>) {
        self.model.library.config.language = language;
        let english = self.uses_english();
        self.skills_view.skill_search.update(cx, |input, cx| {
            input.set_placeholder(
                if english {
                    "Search skills"
                } else {
                    "搜索技能"
                },
                window,
                cx,
            )
        });
        self.projects_view.project_search.update(cx, |input, cx| {
            input.set_placeholder(
                if english {
                    "Search projects"
                } else {
                    "搜索项目"
                },
                window,
                cx,
            )
        });
        self.install_flow.project_search.update(cx, |input, cx| {
            input.set_placeholder(
                if english { "Search project name or path" } else { "搜索项目名称或路径" },
                window, cx,
            )
        });
        self.tags_flow.name_input.update(cx, |input, cx| {
            input.set_placeholder(if english { "Tag name" } else { "标签名称" }, window, cx)
        });
        self.groups_flow.name_input.update(cx, |input, cx| {
            input.set_placeholder(
                if english {
                    "Group name"
                } else {
                    "分组名称"
                },
                window,
                cx,
            )
        });
        let placeholder = match self.add_flow.kind {
            AddKind::Npx => self.tr(
                "粘贴 skills.sh、GitHub 地址或 npx skills add 命令",
                "Paste a skills.sh/GitHub URL or npx skills add command",
            ),
            AddKind::Claude => self.tr(
                "插件名称或 claude plugin install 命令",
                "Plugin name or claude plugin install command",
            ),
            AddKind::Local | AddKind::Existing => "",
        };
        self.add_flow.primary_input.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx)
        });
        self.add_flow.source_select.update(cx, |select, cx| {
            select.set_items(
                vec![
                    SourceChoice {
                        kind: AddKind::Npx,
                        label: "skills.sh / github".into(),
                    },
                    SourceChoice {
                        kind: AddKind::Claude,
                        label: if english {
                            "Claude plugin"
                        } else {
                            "Claude 插件"
                        }
                        .into(),
                    },
                    SourceChoice {
                        kind: AddKind::Local,
                        label: if english {
                            "Local folder"
                        } else {
                            "本地文件夹"
                        }
                        .into(),
                    },
                    SourceChoice {
                        kind: AddKind::Existing,
                        label: if english {
                            "Existing installations"
                        } else {
                            "已有安装"
                        }
                        .into(),
                    },
                ],
                window,
                cx,
            )
        });
        let _ = self.model.library.save();
        cx.notify();
    }

    fn sync_project_select(&self, window: &mut Window, cx: &mut Context<Self>) {
        let choices = self.project_entries()
            .into_iter()
            .map(|entry| ProjectChoice { path: entry.path })
            .collect::<Vec<_>>();
        let selected = self
            .projects_view
            .open_project
            .as_ref()
            .and_then(|path| choices.iter().position(|choice| &choice.path == path))
            .or_else(|| (!choices.is_empty()).then_some(0));
        self.projects_view.project_select.update(cx, |select, cx| {
            select.set_items(choices, window, cx);
            select.set_selected_index(
                selected.map(|row| IndexPath::default().row(row)),
                window,
                cx,
            );
        });
    }
}

impl Render for DialogBody {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.app.update(cx, |app, app_cx| match self.kind {
            DialogKind::Add => app.add_skill_modal(window, app_cx),
            DialogKind::Install => app.install_modal(window, app_cx),
            DialogKind::Delete => app.delete_confirmation_modal(window, app_cx),
            DialogKind::Tags => app.tag_management_modal(window, app_cx),
            DialogKind::AssignTags => app.tag_assignment_modal(window, app_cx),
            DialogKind::Groups => app.group_management_modal(window, app_cx),
            DialogKind::DeleteGroup => app.group_delete_modal(app_cx),
            DialogKind::MoveGroup => app.move_group_modal(window, app_cx),
            DialogKind::CrossProject => app.cross_project_modal(window, app_cx),
            DialogKind::Storage => app.storage_modal(app_cx),
        })
    }
}

impl Render for PageBody {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.app
            .update(cx, |app, app_cx| match app.shell.page {
                Page::Skills => app.skills_page(window, app_cx),
                Page::Projects => app.projects_page(window, app_cx),
                Page::Settings => app.settings_page(window, app_cx),
            })
            .unwrap_or_else(|_| div())
    }
}

impl Render for KitterApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette();
        let content = self.shell.page_body.clone();
        let mut root = div()
            .size_full()
            .relative()
            .track_focus(&self.shell.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key.eq_ignore_ascii_case("escape")
                    && this.shell.dialog_body.as_ref().is_some_and(|body| matches!(body.read(cx).kind, DialogKind::CrossProject | DialogKind::Storage))
                {
                    this.close_dialog(cx);
                    cx.stop_propagation();
                    return;
                }
                if event.keystroke.key.eq_ignore_ascii_case("escape")
                    && this.skills_view.selection.is_multiple()
                {
                    this.finish_skill_selection_mode(cx);
                    cx.stop_propagation();
                }
            }))
            .font_family(FONT_UI)
            .text_color(p.text)
            .text_size(px(14.))
            .bg(p.window)
            .flex();
        let main = div()
            .min_w_0()
            .flex_1()
            .min_h_0()
            .h_full()
            .bg(p.base)
            .flex()
            .flex_col();
        root = root.child(layout::shell(
            &self.shell.shell_layout,
            self.sidebar(cx),
            main.child(content),
        ));
        // Linux uses a borderless GPUI window, so it still needs a client-side
        // outline. Windows and macOS already provide their own window frame.
        if cfg!(target_os = "linux") {
            root = root.child(
                div()
                    .absolute()
                    .top(px(0.))
                    .right(px(0.))
                    .bottom(px(0.))
                    .left(px(0.))
                    .rounded(px(14.))
                    .border_1()
                    .border_color(p.window_border),
            );
        }
        if let Some(notice) = self.shell.notice.clone() {
            let progress = self.model.update_check.clone();
            let mut card = div()
                .id("app-notice")
                .debug_selector(|| "app-notice".into())
                .absolute()
                .right(px(18.))
                .bottom(px(18.))
                .max_w(px(420.))
                .when(progress.is_some(), |card| card.min_w(px(240.)))
                .px(px(13.))
                .py(px(9.))
                .rounded(px(RADIUS_MENU))
                .border_1()
                .border_color(p.border_strong)
                .bg(p.surface)
                .shadow_md()
                .flex()
                .flex_col()
                .gap(px(6.))
                .text_size(px(13.))
                .child(notice);
            if let Some(progress) = progress {
                if let Some(current) = progress.current.filter(|name| !name.is_empty()) {
                    card = card.child(
                        div()
                            .text_size(px(12.))
                            .text_color(p.secondary)
                            .truncate()
                            .child(current),
                    );
                }
                if progress.total > 0 {
                    let ratio = (progress.scanned as f32 / progress.total as f32).clamp(0., 1.);
                    card = card.child(
                        div()
                            .relative()
                            .w_full()
                            .h(px(4.))
                            .rounded(px(2.))
                            .bg(p.raised)
                            .overflow_hidden()
                            .child(
                                div()
                                    .h_full()
                                    .rounded(px(2.))
                                    .bg(p.accent)
                                    .w(relative(ratio)),
                            ),
                    );
                }
            }
            root = root.child(card);
        }
        if let Some((position, menu)) = &self.projects_view.proma_directory_menu {
            root = root.child(deferred(anchored()
                .position(*position)
                .snap_to_window_with_margin(px(8.))
                .child(menu.clone()))
                .with_priority(gpui_base::POPUP_PRIORITY));
        }
        if let Some(body) = self.shell.dialog_body.clone() {
            let width = match body.read(cx).kind {
                DialogKind::Add => px(520.),
                DialogKind::Install => px(600.),
                DialogKind::Delete => px(420.),
                DialogKind::Tags | DialogKind::AssignTags => px(420.),
                DialogKind::Groups => px(420.),
                DialogKind::DeleteGroup => px(460.),
                DialogKind::MoveGroup => px(420.),
                DialogKind::Storage => px(620.),
                DialogKind::CrossProject => px((f32::from(window.bounds().size.width) - 40.).clamp(300., 980.)),
            };
            root = root.child(
                div()
                    .absolute()
                    .top(px(0.))
                    .right(px(0.))
                    .bottom(px(0.))
                    .left(px(0.))
                    .bg(p.overlay)
                    .flex()
                    .items_center()
                    .justify_center()
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .w(width)
                            .max_h(if matches!(body.read(cx).kind, DialogKind::Install | DialogKind::CrossProject) {
                                px((f32::from(window.bounds().size.height) - 40.).max(300.))
                            } else { px(680.) })
                            .rounded(px(RADIUS_MODAL))
                            .border_1()
                            .border_color(p.border)
                            .overflow_hidden()
                            .shadow_lg()
                            .child(body),
                    ),
            );
        }
        root
    }
}

#[cfg(test)]
mod window_tests {
    use super::{fit_initial_window_size, fit_minimum_window_size};
    use gpui::{px, size};

    #[test]
    fn initial_window_fits_inside_a_scaled_laptop_display() {
        let fitted = fit_initial_window_size(Some(size(px(1200.), px(720.))));

        assert_eq!(fitted.width, px(1020.));
        assert_eq!(fitted.height, px(576.));
        let minimum = fit_minimum_window_size(Some(size(px(1200.), px(720.))));
        assert_eq!(minimum.width, px(900.));
        assert_eq!(minimum.height, px(540.));
    }

    #[test]
    fn initial_window_keeps_its_designed_size_on_a_large_display() {
        let fitted = fit_initial_window_size(Some(size(px(2560.), px(1440.))));

        assert_eq!(fitted.width, px(1180.));
        assert_eq!(fitted.height, px(760.));
        let minimum = fit_minimum_window_size(Some(size(px(2560.), px(1440.))));
        assert_eq!(minimum.width, px(940.));
        assert_eq!(minimum.height, px(620.));
    }
}

#[cfg(all(test, feature = "ui-test"))]
mod e2e_tests {
    use super::{AddKind, KitterApp, Page, effective_view};
    use gpui::{AppContext, Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext, point, px};
    use std::fs;

    fn init(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
    }

    #[test]
    fn skill_search_hides_groups_and_keeps_detail_within_results() {
        let mut cx = TestAppContext::single();
        init(&mut cx);
        let temp = tempfile::tempdir().unwrap();
        let (app, cx) = cx.add_window_view({
            let data = temp.path().join("data");
            move |window, cx| KitterApp::new_in(data, window, cx)
        });
        let group_id = app.update_in(cx, |app, window, cx| {
            let group = app.model.library.create_group("Documents").unwrap().id;
            for (name, description) in [("pdf", "PDF editor"), ("kdocs", "Create PDF documents"), ("grill-me", "Interview plans")] {
                let source = temp.path().join(name);
                fs::create_dir_all(&source).unwrap();
                fs::write(source.join("SKILL.md"), format!("---\nname: {name}\ndescription: {description}\n---\n")).unwrap();
                crate::source::scan_local(&source).unwrap().import_selected(
                    &mut app.model.library, &std::collections::HashSet::from([name.into()]), None,
                ).unwrap();
                app.model.library.assign_group(name, Some(&group)).unwrap();
            }
            app.model.library.create_group("Empty").unwrap();
            app.refresh(cx);
            app.skills_view.selection.select_one("grill-me".into());
            app.skills_view.skill_search.update(cx, |input, cx| input.focus(window, cx));
            group
        });
        cx.refresh().unwrap();
        cx.simulate_input("pdf");
        cx.run_until_parked();
        cx.refresh().unwrap();
        let group_selector: &'static str = format!("sort-group-{group_id}").leak();
        assert!(cx.debug_bounds(group_selector).is_none());
        assert!(cx.debug_bounds("skill-pdf").is_some());
        assert!(cx.debug_bounds("skill-kdocs").is_none());
        cx.read_entity(&app, |app, cx| {
            assert_eq!(app.skills_view.selection.primary(), Some("pdf"));
            assert_eq!(app.visible_library_skills(cx).len(), 1);
        });
        app.update(cx, |app, cx| {
            app.skills_view.search_descriptions = true;
            assert_eq!(app.visible_library_skills(cx).len(), 2);
            app.skills_view.selection.select_one("kdocs".into());
            app.skills_view.search_descriptions = false;
            app.sync_skill_search_selection(cx);
            assert_eq!(app.skills_view.selection.primary(), Some("pdf"));
        });
        app.update_in(cx, |app, window, cx| {
            app.skills_view.skill_search.update(cx, |input, cx| {
                input.set_value("no-match-fixture", window, cx);
                cx.emit(super::InputEvent::Change);
            });
        });
        cx.run_until_parked();
        cx.read_entity(&app, |app, _| assert!(app.skills_view.selection.primary().is_none()));
        app.update_in(cx, |app, window, cx| {
            app.skills_view.skill_search.update(cx, |input, cx| {
                input.set_value("", window, cx);
                cx.emit(super::InputEvent::Change);
            });
        });
        cx.run_until_parked();
        cx.refresh().unwrap();
        assert!(cx.debug_bounds(group_selector).is_some());
    }

    #[test]
    fn removed_agent_stays_visible_but_cannot_be_selected() {
        use super::{AgentKind, InstallTarget};
        let mut cx = TestAppContext::single();
        init(&mut cx);
        let temp = tempfile::tempdir().unwrap();
        let (app, cx) = cx.add_window_view({
            let data = temp.path().join("data");
            move |window, cx| KitterApp::new_in(data, window, cx)
        });
        app.update_in(cx, |app, window, cx| {
            app.open_install_dialog(window, cx);
            app.model.installed_agents = crate::agents::AGENT_ICON_ORDER.iter().copied()
                .filter(|agent| matches!(agent.id, "codex" | "cursor")).collect();
            app.projects_view.selected_project_agent = Some(AgentKind::Cursor);
            app.projects_view.project_agent_filter.agents = vec![AgentKind::Cursor, AgentKind::Codex];
            app.install_flow.selected_targets = std::collections::HashSet::from([
                InstallTarget::Cursor, InstallTarget::Codex, InstallTarget::Universal,
            ]);
            let codex = app.model.installed_agents.iter().copied().find(|agent| agent.id == "codex").unwrap();
            assert!(app.set_installed_agents(vec![codex]));
            assert!(app.projects_view.selected_project_agent.is_none());
            assert_eq!(app.projects_view.project_agent_filter.agents, [AgentKind::Codex]);
            assert!(!app.install_flow.selected_targets.contains(&InstallTarget::Cursor));
            assert!(app.install_flow.selected_targets.contains(&InstallTarget::Codex));
            app.toggle_install_target(InstallTarget::OpenCode);
            assert!(!app.install_flow.selected_targets.contains(&InstallTarget::OpenCode));
            app.notify_dialog(cx);
        });
        cx.refresh().unwrap();
        assert!(cx.debug_bounds("target-codex").is_some());
        let cursor = cx.debug_bounds("target-cursor").unwrap();
        cx.simulate_click(cursor.center(), Modifiers::none());
        cx.read_entity(&app, |app, _| assert!(!app.install_flow.selected_targets.contains(&InstallTarget::Cursor)));
        assert!(cx.debug_bounds("target-opencode").is_some());
    }

    #[test]
    fn multi_project_dialog_installs_only_checked_projects() {
        use super::InstallTarget;
        let mut cx = TestAppContext::single();
        init(&mut cx);
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let projects = [root.join("000-project-a"), root.join("001-project-b"), root.join("002-project-c")];
        for project in &projects { fs::create_dir_all(project).unwrap(); }
        let source = root.join("source/demo");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("SKILL.md"), "---\nname: demo\ndescription: UI install fixture\n---\n").unwrap();
        let (app, cx) = cx.add_window_view({
            let data_dir = root.join("data");
            move |window, cx| KitterApp::new_in(data_dir, window, cx)
        });
        app.update_in(cx, |app, window, cx| {
            crate::source::scan_local(&source).unwrap()
                .import_selected(&mut app.model.library, &std::collections::HashSet::from(["demo".into()]), None).unwrap();
            for project in &projects { app.model.library.config.remember_project(project.clone()); }
            app.refresh(cx);
            app.skills_view.selection.replace(&["demo".into()]);
            app.projects_view.open_project = Some(projects[2].clone());
            app.projects_view.global_project_view = false;
            app.open_install_dialog(window, cx);
        });
        cx.refresh().unwrap();
        let scope = cx.debug_bounds("install-scope-projects").unwrap();
        cx.simulate_click(scope.center(), Modifiers::none());
        cx.refresh().unwrap();
        let picker = cx.debug_bounds("install-project-select").unwrap();
        cx.simulate_click(picker.center(), Modifiers::none());
        cx.run_until_parked();
        cx.refresh().unwrap();
        let clear = cx.debug_bounds("install-projects-clear").unwrap();
        cx.simulate_click(clear.center(), Modifiers::none());
        for project in &projects[..2] {
            let index = cx.read_entity(&app, |app, _| app.project_entries().iter().position(|entry| &entry.path == project).unwrap());
            let id = Box::leak(format!("install-project-option-{index}").into_boxed_str());
            let row = cx.debug_bounds(id).unwrap();
            cx.simulate_click(row.center(), Modifiers::none());
        }
        cx.read_entity(&app, |app, _| {
            assert_eq!(app.install_flow.selected_projects.len(), 2);
            assert_eq!(app.projects_view.open_project.as_ref(), Some(&projects[2]));
        });
        let done = cx.debug_bounds("install-projects-done").unwrap();
        cx.simulate_click(done.center(), Modifiers::none());
        cx.run_until_parked();
        cx.refresh().unwrap();
        let target = cx.debug_bounds("target-codex").unwrap();
        cx.simulate_click(target.center(), Modifiers::none());
        cx.read_entity(&app, |app, _| assert!(app.install_flow.selected_targets.contains(&InstallTarget::Codex)));
        cx.run_until_parked();
        cx.refresh().unwrap();
        let confirm = cx.debug_bounds("confirm-install").unwrap();
        cx.simulate_click(confirm.center(), Modifiers::none());
        cx.run_until_parked();
        for project in &projects[..2] { assert!(project.join(".codex/skills/demo/SKILL.md").is_file()); }
        assert!(!projects[2].join(".codex").exists());
        cx.read_entity(&app, |app, _| assert!(!app.install_flow.modal));
    }

    #[test]
    fn multi_project_dialog_search_keeps_selection_across_queries_and_reopen() {
        let mut cx = TestAppContext::single();
        init(&mut cx);
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let alpha = root.join("alpha-search-fixture");
        let beta = root.join("design-only/beta-search-fixture");
        let other = root.join("unselected-search-fixture");
        for project in [&alpha, &beta, &other] { fs::create_dir_all(project).unwrap(); }
        let (app, cx) = cx.add_window_view({
            let data_dir = root.join("data");
            move |window, cx| KitterApp::new_in(data_dir, window, cx)
        });
        app.update_in(cx, |app, window, cx| {
            for project in [&alpha, &beta, &other] {
                app.model.library.config.remember_project(project.clone());
            }
            app.projects_view.global_project_view = false;
            app.open_install_dialog(window, cx);
            app.install_flow.selected_projects.clear();
        });
        cx.refresh().unwrap();
        let scope = cx.debug_bounds("install-scope-projects").unwrap();
        cx.simulate_click(scope.center(), Modifiers::none());
        cx.refresh().unwrap();
        let picker = cx.debug_bounds("install-project-select").unwrap();
        cx.simulate_click(picker.center(), Modifiers::none());
        cx.run_until_parked();
        cx.refresh().unwrap();
        assert!(cx.debug_bounds("install-project-search").is_some());
        app.update_in(cx, |app, window, cx| {
            app.install_flow.project_search.update(cx, |input, cx| input.set_value("ALPHA-SEARCH-FIXTURE", window, cx));
        });
        cx.run_until_parked();
        cx.refresh().unwrap();
        cx.read_entity(&app, |app, cx| {
            let matches = app.matching_install_projects(cx);
            assert_eq!(matches.len(), 1);
            assert_eq!(matches[0].path, alpha);
        });
        let row = cx.debug_bounds("install-project-option-0").unwrap();
        cx.simulate_click(row.center(), Modifiers::none());
        cx.read_entity(&app, |app, _| {
            assert!(app.install_flow.project_picker_open);
            assert!(app.install_flow.selected_projects.contains(&alpha));
        });
        app.update_in(cx, |app, window, cx| {
            app.install_flow.project_search.update(cx, |input, cx| input.set_value("/design-only/", window, cx));
        });
        cx.run_until_parked();
        cx.refresh().unwrap();
        cx.read_entity(&app, |app, cx| assert_eq!(app.matching_install_projects(cx)[0].path, beta));
        let select = cx.debug_bounds("install-projects-select-all").unwrap();
        cx.simulate_click(select.center(), Modifiers::none());
        cx.read_entity(&app, |app, _| {
            assert_eq!(app.install_flow.selected_projects, [alpha.clone(), beta.clone()].into_iter().collect());
        });
        app.update_in(cx, |app, window, cx| {
            app.install_flow.project_search.update(cx, |input, cx| input.set_value("no-matches-search-fixture", window, cx));
        });
        cx.run_until_parked();
        cx.refresh().unwrap();
        assert!(cx.debug_bounds("install-project-option-0").is_none());
        let done = cx.debug_bounds("install-projects-done").unwrap();
        cx.simulate_click(done.center(), Modifiers::none());
        cx.run_until_parked();
        cx.refresh().unwrap();
        assert!(cx.debug_bounds("install-project-search").is_none());
        let picker = cx.debug_bounds("install-project-select").unwrap();
        cx.simulate_click(picker.center(), Modifiers::none());
        cx.run_until_parked();
        app.update_in(cx, |app, window, cx| {
            app.install_flow.project_search.update(cx, |input, cx| input.set_value("", window, cx));
        });
        cx.run_until_parked();
        cx.refresh().unwrap();
        cx.read_entity(&app, |app, cx| {
            assert_eq!(app.matching_install_projects(cx).len(), 3);
            assert_eq!(app.install_flow.selected_projects.len(), 2);
            assert!(!app.install_flow.selected_projects.contains(&other));
        });
    }

    #[test]
    fn multi_project_dialog_preserves_selection_when_switching_scope() {
        use super::InstallTarget;
        let mut cx = TestAppContext::single();
        init(&mut cx);
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let projects = [root.join("000-a"), root.join("001-b")];
        for project in &projects { fs::create_dir_all(project).unwrap(); }
        let (app, cx) = cx.add_window_view({
            let data_dir = root.join("data");
            move |window, cx| KitterApp::new_in(data_dir, window, cx)
        });
        app.update_in(cx, |app, window, cx| {
            for project in &projects { app.model.library.config.remember_project(project.clone()); }
            app.projects_view.global_project_view = false;
            app.open_install_dialog(window, cx);
            app.install_flow.selected_projects = projects.iter().cloned().collect();
            app.install_flow.selected_targets.insert(InstallTarget::Proma);
            app.notify_dialog(cx);
        });
        cx.refresh().unwrap();
        let global = cx.debug_bounds("install-scope-global").unwrap();
        cx.simulate_click(global.center(), Modifiers::none());
        cx.read_entity(&app, |app, _| {
            assert!(app.install_flow.global);
            assert!(!app.install_flow.selected_targets.contains(&InstallTarget::Proma));
        });
        let local = cx.debug_bounds("install-scope-projects").unwrap();
        cx.simulate_click(local.center(), Modifiers::none());
        cx.read_entity(&app, |app, _| {
            assert!(!app.install_flow.global);
            assert_eq!(app.install_flow.selected_projects, projects.iter().cloned().collect());
        });
    }

    #[test]
    fn injected_data_directory_is_the_only_library_source() {
        let mut cx = TestAppContext::single();
        init(&mut cx);
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("kitter-data");

        let (app, cx) = cx.add_window_view({
            let data_dir = data_dir.clone();
            move |window, cx| KitterApp::new_in(data_dir, window, cx)
        });

        cx.read_entity(&app, |app, _| {
            assert_eq!(app.model.library.data_dir(), data_dir);
            assert!(
                app.model
                    .skills
                    .iter()
                    .any(|skill| skill.record.name == "kitter")
            );
        });
        assert!(data_dir.join("registry.json").is_file());
        assert!(data_dir.join("skills/_kitter-builtin/SKILL.md").is_file());
    }

    #[test]
    fn collapsed_skill_groups_are_restored_from_config() {
        let mut cx = TestAppContext::single();
        init(&mut cx);
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("kitter-data");
        let (app, cx) = cx.add_window_view({
            let data_dir = data_dir.clone();
            move |window, cx| KitterApp::new_in(data_dir, window, cx)
        });
        let group_id = app.update(cx, |app, _| {
            let id = app
                .model
                .library
                .create_group("owner/repository")
                .unwrap()
                .id;
            app.skills_view.collapsed_groups.insert(id.clone());
            app.persist_collapsed_groups();
            id
        });

        let mut cx = TestAppContext::single();
        init(&mut cx);
        let (app, cx) = cx.add_window_view({
            let data_dir = data_dir.clone();
            move |window, cx| KitterApp::new_in(data_dir, window, cx)
        });
        cx.read_entity(&app, |app, _| {
            assert!(app.skills_view.collapsed_groups.contains(&group_id));
            assert!(
                app.model
                    .library
                    .config
                    .collapsed_skill_groups
                    .contains(&group_id)
            );
        });

        app.update(cx, |app, _| {
            app.skills_view.collapsed_groups.remove(&group_id);
            app.persist_collapsed_groups();
        });
        let mut cx = TestAppContext::single();
        init(&mut cx);
        let (app, cx) = cx.add_window_view({
            let data_dir = data_dir.clone();
            move |window, cx| KitterApp::new_in(data_dir, window, cx)
        });
        cx.read_entity(&app, |app, _| {
            assert!(!app.skills_view.collapsed_groups.contains(&group_id));
            assert!(app.model.library.config.collapsed_skill_groups_initialized);
        });
    }

    #[test]
    fn sidebar_navigation_switches_the_rendered_page() {
        let mut cx = TestAppContext::single();
        init(&mut cx);
        let temp = tempfile::tempdir().unwrap();
        let (app, cx) = cx.add_window_view({
            let data_dir = temp.path().join("kitter-data");
            move |window, cx| KitterApp::new_in(data_dir, window, cx)
        });
        cx.refresh().unwrap();
        cx.run_until_parked();

        for (selector, expected) in [
            ("nav-projects", Page::Projects),
            ("nav-settings", Page::Settings),
            ("nav-skills", Page::Skills),
        ] {
            let bounds = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} should be rendered"));
            cx.simulate_click(bounds.center(), Modifiers::none());
            cx.run_until_parked();
            cx.read_entity(&app, |app, _| assert!(app.shell.page == expected));
        }
    }

    #[test]
    fn project_details_are_loaded_off_the_ui_thread() {
        let mut cx = TestAppContext::single();
        init(&mut cx);
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("kitter-data");
        let project = temp.path().join("fixture-project");
        let skill = project.join(".agents/skills/fixture-skill");
        fs::create_dir_all(&skill).unwrap();
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: fixture-skill\ndescription: Fixture\n---\n",
        )
        .unwrap();

        let (app, cx) = cx.add_window_view({
            let data_dir = data_dir.clone();
            move |window, cx| KitterApp::new_in(data_dir, window, cx)
        });

        app.update(cx, |app, cx| {
            assert!(app.project_snapshot(&project, cx).is_none());
            assert!(
                !app.projects_view
                    .project_snapshots
                    .borrow()
                    .contains_key(&project)
            );
            assert!(
                app.projects_view
                    .project_snapshot_tasks
                    .borrow()
                    .contains_key(&project)
            );
        });

        cx.run_until_parked();
        cx.read_entity(&app, |app, _| {
            let snapshots = app.projects_view.project_snapshots.borrow();
            let snapshot = snapshots
                .get(&project)
                .expect("background project scan should finish");
            assert_eq!(snapshot.len(), 1);
            assert_eq!(snapshot[0].name, "fixture-skill");
            assert!(
                !app.projects_view
                    .project_snapshot_tasks
                    .borrow()
                    .contains_key(&project)
            );
        });
    }

    #[test]
    fn project_refresh_reloads_installations_changed_outside_kitter() {
        let mut cx = TestAppContext::single();
        init(&mut cx);
        let temp = tempfile::tempdir().unwrap();
        let projects = [temp.path().join("first"), temp.path().join("second")];
        for project in &projects {
            let skill = project.join(".codex/skills/external");
            fs::create_dir_all(&skill).unwrap();
            fs::write(skill.join("SKILL.md"), "---\nname: before-update\ndescription: Fixture\n---\n").unwrap();
        }
        let (app, cx) = cx.add_window_view({
            let data_dir = temp.path().join("kitter-data");
            move |window, cx| KitterApp::new_in(data_dir, window, cx)
        });
        app.update(cx, |app, cx| app.request_project_snapshots(&projects, cx));
        cx.run_until_parked();
        for project in &projects {
            cx.read_entity(&app, |app, _| {
                assert_eq!(app.projects_view.project_snapshots.borrow()[project][0].name, "before-update");
            });
            fs::write(project.join(".codex/skills/external/SKILL.md"),
                "---\nname: after-update\ndescription: Fixture\n---\n").unwrap();
            app.update(cx, |app, cx| {
                app.refresh_context_estimate(project.clone(), cx);
                assert!(!app.projects_view.project_snapshots.borrow().contains_key(project),
                    "refresh must invalidate installation classification as well as token estimates");
                assert!(app.project_snapshot(project, cx).is_none());
            });
            cx.run_until_parked();
            cx.read_entity(&app, |app, _| {
                assert_eq!(app.projects_view.project_snapshots.borrow()[project][0].name, "after-update");
            });
        }
    }

    #[test]
    fn large_project_renders_and_scrolls_with_virtual_rows() {
        effective_view::reset_path_resolution_count();
        let mut cx = TestAppContext::single();
        init(&mut cx);
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("kitter-data");
        let project = temp.path().join("fixture-project");
        let parent_skill = temp.path().join(".agents/skills/fixture-parent");
        fs::create_dir_all(&parent_skill).unwrap();
        fs::write(parent_skill.join("SKILL.md"), "---\nname: fixture-parent\ndescription: Parent fixture\n---\n").unwrap();
        for target in [".agents/skills", ".codex/skills", ".claude/skills"] {
            for index in 1..=180 {
                let name = format!("fixture-skill-{index:03}");
                let skill = project.join(target).join(&name);
                fs::create_dir_all(&skill).unwrap();
                fs::write(
                    skill.join("SKILL.md"),
                    format!("---\nname: {name}\ndescription: Performance fixture {index}\n---\n"),
                )
                .unwrap();
            }
        }

        let (app, cx) = cx.add_window_view({
            let data_dir = data_dir.clone();
            move |window, cx| KitterApp::new_in(data_dir, window, cx)
        });
        app.update(cx, |app, cx| {
            app.shell.page = Page::Projects;
            app.projects_view.global_project_view = false;
            app.projects_view.open_project = Some(project.clone());
            cx.notify();
        });
        cx.refresh().unwrap();
        cx.run_until_parked();

        cx.read_entity(&app, |app, _| {
            assert_eq!(
                app.projects_view
                    .project_snapshots
                    .borrow()
                    .get(&project)
                    .expect("large project scan should finish")
                    .len(),
                180
            );
            assert!(
                app.projects_view
                    .context_estimates
                    .borrow()
                    .contains_key(&project)
            );
        });
        cx.refresh().unwrap();
        cx.run_until_parked();
        let (first_selector, last_selector, fixture_count) = cx.read_entity(&app, |app, _| {
            let estimates = app.projects_view.context_estimates.borrow();
            let direct = app.projects_view.project_snapshots.borrow();
            let rows = effective_view::skill_rows(&estimates[&project].estimates, &direct[&project], None, &project);
            let fixtures = rows.iter().filter(|row| row.name.starts_with("fixture-skill-")).collect::<Vec<_>>();
            assert!(fixtures.len() >= 180);
            (format!("project-skill-{}", fixtures.first().unwrap().key),
             format!("project-skill-{}", fixtures.last().unwrap().key), fixtures.len())
        });
        // GPUI's test selector API requires process-lifetime strings.
        let first_selector: &'static str = Box::leak(first_selector.into_boxed_str());
        let last_selector: &'static str = Box::leak(last_selector.into_boxed_str());
        let first_row = cx
            .debug_bounds(first_selector)
            .expect("first virtual project row should be rendered");
        assert!(cx.debug_bounds(last_selector).is_none());
        let scroll = cx
            .debug_bounds("project-skills-virtual-list")
            .expect("virtual project list should be rendered");
        assert_eq!(first_row.size.width, scroll.size.width);
        assert!(effective_view::path_resolution_count() > 0);
        effective_view::reset_path_resolution_count();
        // GPUI clamps large synthetic wheel deltas. Use bounded page events
        // rather than assuming that all source rows share one pixel offset.
        for _ in 0..fixture_count {
            cx.simulate_event(ScrollWheelEvent {
                position: scroll.center(),
                delta: ScrollDelta::Pixels(point(px(0.), -scroll.size.height)),
                ..Default::default()
            });
            cx.run_until_parked();
            cx.refresh().unwrap();
            if cx.debug_bounds(last_selector).is_some() { break; }
        }
        assert!(cx.debug_bounds(last_selector).is_some());
        assert_eq!(effective_view::path_resolution_count(), 0, "scrolling rebuilt filesystem paths");
        for _ in 0..fixture_count {
            cx.simulate_event(ScrollWheelEvent {
                position: scroll.center(),
                delta: ScrollDelta::Pixels(point(px(0.), scroll.size.height)),
                ..Default::default()
            });
            cx.run_until_parked();
            cx.refresh().unwrap();
            if cx.read_entity(&app, |app, _| {
                let offset = app.projects_view.skill_list.logical_scroll_top();
                offset.item_ix == 0 && offset.offset_in_item == px(0.)
            }) { break; }
        }
        let parent_tab = cx.debug_bounds("project-skill-source-parent").unwrap();
        cx.simulate_click(parent_tab.center(), Modifiers::none());
        cx.run_until_parked();
        cx.refresh().unwrap();
        assert!(cx.debug_bounds(first_selector).is_none());
        let project_tab = cx.debug_bounds("project-skill-source-project").unwrap();
        cx.simulate_click(project_tab.center(), Modifiers::none());
        cx.run_until_parked();
        cx.refresh().unwrap();
        assert!(cx.debug_bounds(first_selector).is_some());
        assert_eq!(effective_view::path_resolution_count(), 0, "source tab changes rebuilt filesystem paths");
    }

    #[test]
    fn groups_drag_in_both_views_and_keep_the_saved_order() {
        use gpui::MouseButton;
        let mut cx = TestAppContext::single();
        init(&mut cx);
        let temp = tempfile::tempdir().unwrap();
        let (app, cx) = cx.add_window_view({
            let data_dir = temp.path().join("data");
            move |window, cx| KitterApp::new_in(data_dir, window, cx)
        });
        let (a, b) = app.update(cx, |app, cx| {
            let a = app.model.library.create_group("A").unwrap().id;
            let b = app.model.library.create_group("B").unwrap().id;
            app.open_group_dialog(cx);
            (a, b)
        });
        let a_row: &'static str = format!("sort-group-{a}").leak();
        let b_row: &'static str = format!("sort-group-{b}").leak();
        let a_handle: &'static str = format!("drag-group-{a}").leak();
        for management in [true, false] {
            if !management {
                app.update(cx, |app, cx| app.close_dialog(cx));
            }
            cx.refresh().unwrap();
            let row = cx.debug_bounds(a_row).unwrap();
            cx.simulate_mouse_move(row.center(), None, Modifiers::none());
            cx.refresh().unwrap();
            let start = if management {
                cx.debug_bounds(a_handle).unwrap().center()
            } else {
                assert!(
                    cx.debug_bounds(a_handle).is_none(),
                    "list must not have a drag handle or its spacing"
                );
                row.center()
            };
            if management {
                let handle = cx.debug_bounds(a_handle).unwrap();
                assert_eq!(handle.size.width, px(24.));
                assert_eq!(handle.origin.x, row.origin.x);
                assert_eq!(
                    cx.debug_bounds(a_row).unwrap(),
                    row,
                    "hover must not shift the row"
                );
            }
            let target = cx.debug_bounds(b_row).unwrap();
            let end = point(
                target.center().x,
                target.center().y + if management { px(5.) } else { px(-5.) },
            );
            cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
            cx.simulate_mouse_move(
                start + point(px(5.), px(0.)),
                MouseButton::Left,
                Modifiers::none(),
            );
            cx.refresh().unwrap();
            cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
            cx.refresh().unwrap();
            assert_eq!(
                cx.debug_bounds("group-drop-line-Management").is_some(),
                management
            );
            assert_eq!(
                cx.debug_bounds("group-drop-line-List").is_some(),
                !management
            );
            cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
            cx.refresh().unwrap();
            app.update(cx, |app, _| {
                let ids = app
                    .model
                    .library
                    .groups()
                    .into_iter()
                    .map(|group| group.id)
                    .collect::<Vec<_>>();
                assert_eq!(
                    ids,
                    if management {
                        vec![b.clone(), a.clone()]
                    } else {
                        vec![a.clone(), b.clone()]
                    }
                );
                assert!(app.skills_view.collapsed_groups.is_empty());
            });
        }
    }

    #[test]
    fn creating_group_inside_add_keeps_selection_and_dialog() {
        use super::{DialogKind, GroupEdit};
        let mut cx = TestAppContext::single();
        init(&mut cx);
        let temp = tempfile::tempdir().unwrap();
        let (app, cx) = cx.add_window_view({
            let data_dir = temp.path().join("data");
            move |window, cx| KitterApp::new_in(data_dir, window, cx)
        });
        app.update_in(cx, |app, window, cx| {
            app.open_add_dialog(window, cx);
            app.add_flow.selected.extend(["one".into(), "two".into()]);
            app.notify_dialog(cx);
        });
        cx.refresh().unwrap();
        let selector = cx.debug_bounds("add-group-control").unwrap();
        cx.simulate_click(
            point(selector.center().x, selector.bottom() - px(12.)),
            Modifiers::none(),
        );
        cx.refresh().unwrap();
        let create = cx.debug_bounds("add-group-create").unwrap();
        cx.simulate_click(create.center(), Modifiers::none());
        cx.refresh().unwrap();
        cx.simulate_input("Team");
        cx.simulate_keystrokes("enter");
        app.update_in(cx, |app, window, cx| {
            assert!(matches!(
                app.shell.dialog_body.as_ref().unwrap().read(cx).kind,
                DialogKind::Add
            ));
            assert_eq!(app.add_flow.group_name.as_deref(), Some("Team"));
            assert_eq!(app.add_flow.selected.len(), 2);
            assert_eq!(app.model.library.groups().len(), 1);
            app.start_group_edit(GroupEdit::Create, window, cx);
            app.groups_flow
                .name_input
                .update(cx, |input, cx| input.set_value("Team", window, cx));
            app.commit_group_edit(cx);
            assert!(app.groups_flow.edit.is_some());
            assert!(app.tags_flow.error.is_some());
            assert_eq!(app.model.library.groups().len(), 1);
        });
    }

    #[test]
    fn busy_add_dialog_blocks_changes_and_recovers_when_idle() {
        use super::{AddTask, Language};
        use gpui::Focusable;
        let mut cx = TestAppContext::single();
        init(&mut cx);
        let temp = tempfile::tempdir().unwrap();
        let (app, cx) = cx.add_window_view({
            let data_dir = temp.path().join("data");
            move |window, cx| KitterApp::new_in(data_dir, window, cx)
        });
        app.update_in(cx, |app, window, cx| {
            app.set_language(Language::En, window, cx);
            app.open_add_dialog(window, cx);
            app.add_flow.primary_input.update(cx, |input, cx| {
                input.set_value("https://github.com/mattpocock/skills", window, cx);
                input.focus_handle(cx).focus(window, cx);
            });
            app.add_flow.selected.extend(["one".into(), "two".into()]);
            app.add_flow.task = Some(AddTask::Scanning);
            app.notify_dialog(cx);
        });
        cx.refresh().unwrap();
        assert!(cx.debug_bounds("scan-progress").is_some());
        cx.simulate_keystrokes("x");
        for selector in ["close-add-modal", "cancel-add"] {
            let bounds = cx.debug_bounds(selector).unwrap();
            cx.simulate_click(bounds.center(), Modifiers::none());
        }
        app.update_in(cx, |app, window, cx| {
            assert_eq!(
                app.add_flow.primary_input.read(cx).value().as_ref(),
                "https://github.com/mattpocock/skills"
            );
            assert!(
                !app.add_flow
                    .primary_input
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
            );
            app.set_add_kind(AddKind::Local, window, cx);
            assert!(app.add_flow.kind == AddKind::Npx);
            app.close_dialog(cx);
            assert!(app.shell.dialog_body.is_some());
            app.add_flow.task = Some(AddTask::Importing);
            app.notify_dialog(cx);
        });
        cx.refresh().unwrap();
        assert!(cx.debug_bounds("scan-progress").is_none());
        app.update(cx, |app, cx| {
            app.add_flow.task = None;
            app.notify_dialog(cx);
        });
        cx.refresh().unwrap();
        let bounds = cx.debug_bounds("close-add-modal").unwrap();
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.read_entity(&app, |app, _| assert!(app.shell.dialog_body.is_none()));
    }

    #[test]
    fn local_folder_picker_scans_inside_the_isolated_window() {
        let mut cx = TestAppContext::single();
        init(&mut cx);
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("kitter-data");
        let source = temp.path().join("fixture-skill");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("SKILL.md"),
            "---\nname: fixture-skill\ndescription: Fixture\n---\n",
        )
        .unwrap();

        let (app, cx) = cx.add_window_view({
            let data_dir = data_dir.clone();
            move |window, cx| KitterApp::new_in(data_dir, window, cx)
        });
        app.update_in(cx, |app, window, cx| {
            app.open_add_dialog(window, cx);
            app.set_add_kind(AddKind::Local, window, cx);
        });
        cx.refresh().unwrap();
        cx.run_until_parked();

        let choose = cx
            .debug_bounds("choose-local-skill")
            .expect("local folder control should be rendered");
        cx.simulate_click(choose.center(), Modifiers::none());
        assert!(cx.did_prompt_for_paths());
        cx.simulate_path_prompt_response({
            let source = source.clone();
            move |options| {
                assert!(options.directories);
                assert!(!options.multiple);
                Some(vec![source])
            }
        });
        cx.run_until_parked();
        cx.read_entity(&app, |app, _| {
            let scan = app.add_flow.scan.as_ref().expect("scan should finish");
            assert_eq!(scan.skills().len(), 1);
            assert_eq!(scan.skills()[0].name, "fixture-skill");
            assert_eq!(app.model.library.data_dir(), data_dir);
        });

        let candidate = cx
            .debug_bounds("scan-skill-0")
            .expect("scanned skill row should be rendered");
        cx.simulate_click(candidate.center(), Modifiers::none());
        cx.run_until_parked();
        let confirm = cx
            .debug_bounds("confirm-add")
            .expect("import control should be rendered");
        cx.simulate_click(confirm.center(), Modifiers::none());
        cx.run_until_parked();
        cx.read_entity(&app, |app, _| {
            assert!(
                app.model
                    .skills
                    .iter()
                    .any(|skill| skill.record.name == "fixture-skill")
            );
            assert!(app.shell.dialog_body.is_none());
        });
        assert!(data_dir.join("skills/fixture-skill/SKILL.md").is_file());
    }

    #[test]
    fn check_updates_shows_scan_progress_notice() {
        use super::Language;
        use std::collections::HashSet;
        use std::time::Duration;

        let mut cx = TestAppContext::single();
        init(&mut cx);
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("kitter-data");
        let source = temp.path().join("skills");
        for name in ["alpha", "beta"] {
            fs::create_dir_all(source.join(name)).unwrap();
            fs::write(
                source.join(name).join("SKILL.md"),
                format!("---\nname: {name}\ndescription: Fixture\n---\n"),
            )
            .unwrap();
        }

        let (app, cx) = cx.add_window_view({
            let data_dir = data_dir.clone();
            move |window, cx| KitterApp::new_in(data_dir, window, cx)
        });
        app.update_in(cx, |app, window, cx| {
            app.set_language(Language::ZhCn, window, cx);
            crate::source::scan_local(&source)
                .unwrap()
                .import_selected(
                    &mut app.model.library,
                    &HashSet::from(["alpha".into(), "beta".into()]),
                    None,
                )
                .unwrap();
            // Linked local sources are intentionally excluded from update
            // checks. Use legacy owned copies to exercise two pending scans.
            for skill in app.model.library.list().unwrap().into_iter()
                .filter(|skill| matches!(skill.record.name.as_str(), "alpha" | "beta"))
            {
                assert!(fs::symlink_metadata(&skill.path).unwrap().file_type().is_symlink());
                fs::remove_file(&skill.path).unwrap();
                crate::package_fs::copy_tree(&source.join(&skill.record.name), &skill.path).unwrap();
            }
            app.refresh(cx);
            app.check_all_updates(cx);
            let progress = app
                .model
                .update_check
                .as_ref()
                .expect("check should start immediately");
            assert_eq!(progress.total, 2);
            assert_eq!(progress.scanned, 0);
            assert!(
                app.shell
                    .notice
                    .as_deref()
                    .is_some_and(|notice| notice.contains("正在检查更新"))
            );
        });
        cx.refresh().unwrap();
        assert!(cx.debug_bounds("app-notice").is_some());
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        cx.refresh().unwrap();
        app.update(cx, |app, _| {
            assert!(app.model.update_check.is_none());
            assert_eq!(app.shell.notice.as_deref(), Some("所有技能都是最新版本"));
        });
        assert!(cx.debug_bounds("app-notice").is_some());
    }
}

fn fit_initial_window_size(display_size: Option<Size<Pixels>>) -> Size<Pixels> {
    let mut window_size = size(px(1180.), px(760.));
    if let Some(display_size) = display_size {
        // Keep the native title bar and resize frame comfortably inside the
        // visible display on high-DPI and smaller screens. GPUI centers against
        // the full monitor, so leave extra vertical room for taskbars and docks.
        window_size.width = window_size.width.min(display_size.width * 0.85);
        window_size.height = window_size.height.min(display_size.height * 0.80);
    }
    window_size
}

fn fit_minimum_window_size(display_size: Option<Size<Pixels>>) -> Size<Pixels> {
    let mut minimum_size = size(px(940.), px(620.));
    if let Some(display_size) = display_size {
        minimum_size.width = minimum_size.width.min(display_size.width * 0.75);
        minimum_size.height = minimum_size.height.min(display_size.height * 0.75);
    }
    minimum_size
}

fn initial_window_size(cx: &App) -> Size<Pixels> {
    fit_initial_window_size(cx.primary_display().map(|display| display.bounds().size))
}

fn minimum_window_size(cx: &App) -> Size<Pixels> {
    fit_minimum_window_size(cx.primary_display().map(|display| display.bounds().size))
}

pub fn run() {
    let app = gpui_platform::application().with_assets(crate::assets::Assets);
    app.on_reopen(open_main_window);
    app.run(|cx: &mut App| {
        cx.on_action(quit);
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        let preview_id = cfg!(debug_assertions).then(|| std::env::var("KITTER_PREVIEW_APP_ID").ok()).flatten();
        let preview_name = cfg!(debug_assertions).then(|| std::env::var("KITTER_PREVIEW_APP_NAME").ok()).flatten();
        cx.set_app_identity(preview_id.as_deref().unwrap_or("dev.kitter.app"), preview_name.as_deref().unwrap_or("Pleat"));
        gpui_component::init(cx);
        crate::assets::register_fonts(cx).expect("failed to register fonts");
        open_main_window(cx);
    });
}

fn open_main_window(cx: &mut App) {
    if let Some(window) = cx.windows().first() {
        let _ = window.update(cx, |_, window, _| window.activate_window());
        cx.activate(true);
        return;
    }
    cx.open_window(
        WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some(cfg!(debug_assertions)
                    .then(|| std::env::var("KITTER_PREVIEW_APP_NAME").ok()).flatten()
                    .filter(|name| !name.trim().is_empty()).unwrap_or_else(|| "Pleat".into()).into()),
                appears_transparent: cfg!(target_os = "macos"),
                traffic_light_position: cfg!(target_os = "macos")
                    .then(|| point(px(16.), px(17.))),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                initial_window_size(cx),
                cx,
            ))),
            window_min_size: Some(minimum_window_size(cx)),
            is_movable: true,
            app_owns_titlebar_drag: cfg!(target_os = "macos"),
            window_background: if cfg!(target_os = "macos") {
                WindowBackgroundAppearance::Blurred
            } else {
                WindowBackgroundAppearance::Opaque
            },
            ..Default::default()
        },
        |window, cx| {
            let view = cx.new(|cx| {
                let app = KitterApp::new(window, cx);
                cx.observe_window_bounds(window, |_, window, cx| {
                    crate::platform::update_soft_window_shadow(window);
                    cx.notify();
                })
                .detach();
                app
            });
            crate::platform::configure_window_material(window, cx.theme().is_dark());
            // Kitter uses the platform window decorations. The component Root's
            // client-side border adds a full-window mouse-move layer that is
            // only needed by borderless Linux windows.
            cx.new(|cx| {
                Root::new(view, window, cx)
                    .bordered(false)
                    .bg(cx.theme().background)
            })
        },
    )
    .expect("failed to open Kitter window");
}
