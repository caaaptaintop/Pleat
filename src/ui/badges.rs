use super::*;
use gpui_component::menu::DropdownMenu as _;

fn proma_directory_label(path: &Path) -> String {
    let workspace = path.parent().and_then(Path::file_name).unwrap_or_default();
    format!(
        "{} / {}",
        workspace.to_string_lossy(),
        path.file_name().unwrap_or_default().to_string_lossy()
    )
}

impl KitterApp {
    pub(super) fn project_agent_shortcuts(
        &self,
        entry: &crate::project_discovery::ProjectEntry,
        cx: &mut Context<Self>,
    ) -> Div {
        let p = self.palette();
        let dark = self.uses_dark_theme();
        let mut shortcuts = div()
            .mt(px(4.))
            .flex()
            .items_center()
            .gap(px(4.))
            .flex_wrap();
        for agent in crate::agents::AGENT_ICON_ORDER
            .iter()
            .copied()
            .filter(|agent| entry.agents.iter().any(|kind| kind.id() == agent.id))
        {
            let roots = entry.skill_directories(agent);
            let existing = roots
                .iter()
                .filter(|root| root.is_dir())
                .cloned()
                .collect::<Vec<_>>();
            let is_proma = agent.id == "proma";
            // Proma offers one active directory per workspace. Inactive
            // directories belong exclusively to the icon's context menu.
            let choices = if is_proma { roots } else { existing.clone() };
            let label = format!(
                "{} · {}",
                agent.name,
                self.tr("打开项目技能目录", "Open project skill folder")
            );
            let tooltip = if is_proma && choices.len() > 1 {
                format!(
                    "{} · {}",
                    agent.name,
                    self.tr("选择工作区技能目录", "Choose a workspace skill folder")
                )
            } else if existing.is_empty() {
                format!(
                    "{} · {}",
                    agent.name,
                    self.tr("尚无项目技能目录", "No project skill folder yet")
                )
            } else if existing.len() == 1 {
                format!("{label}\n{}", display_path(&existing[0]))
            } else {
                format!(
                    "{} · {}",
                    agent.name,
                    self.tr("选择项目技能目录", "Choose a project skill folder")
                )
            };
            let button = Button::new(ElementId::Name(
                format!("project-agent-{}-{}", entry.path.display(), agent.id).into(),
            ))
            .xsmall()
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(rgba(0x00000000).into())
                    .foreground(p.text.into())
                    .hover(p.hover.into())
                    .active(p.selected.into()),
            )
            .size(px(22.))
            .p(px(0.))
            .rounded(px(6.))
            .child(
                div()
                    .id("project-agent-icon")
                    .role(Role::Image)
                    .aria_label(label)
                    .child(Self::brand_icon(
                        agent.icon_path,
                        Self::brand_icon_size(agent.icon_path, 17.),
                        p.text,
                        dark,
                    )),
            );
            let active = choices.clone();
            let shortcut = if !is_proma && choices.len() > 1 {
                let app = cx.entity().downgrade();
                let project = entry.path.clone();
                let missing_label = self.tr("暂无技能目录", "No skill folder yet").to_string();
                button
                    .dropdown_menu(move |mut menu, _, _| {
                        for path in &choices {
                            let path = path.clone();
                            let app = app.clone();
                            let available = path.is_dir();
                            let mut label = if is_proma {
                                proma_directory_label(&path)
                            } else {
                                display_effective_root(&path, &project)
                            };
                            if !available {
                                label.push_str(&format!(" · {missing_label}"));
                            }
                            menu = menu.item(
                                PopupMenuItem::new(label)
                                    .icon(Icon::new(IconName::Folder))
                                    .disabled(!available)
                                    .on_click(move |_, _, cx| {
                                        let _ = app.update(cx, |this, cx| {
                                            this.open_project_skill_directory(&path, cx)
                                        });
                                    }),
                            );
                        }
                        menu.min_w(px(240.)).max_w(px(480.))
                    })
                    .into_any_element()
            } else {
                button
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        if let Some(path) = existing.first() {
                            this.open_project_skill_directory(path, cx);
                        } else {
                            this.show_notice(
                                format!(
                                    "{} · {}",
                                    agent.name,
                                    this.tr("尚无项目技能目录", "No project skill folder yet")
                                ),
                                cx,
                            );
                        }
                    }))
                    .into_any_element()
            };
            // Keep the tooltip owned by the shortcut, so leaving Projects drops it.
            let shortcut = div()
                .id(ElementId::Name(format!("project-agent-tooltip-{}-{}", entry.path.display(), agent.id).into()))
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .child(shortcut);
            if is_proma {
                let mut seen = BTreeSet::new();
                let inactive = entry
                    .proma_workspaces
                    .iter()
                    .map(|workspace| workspace.join("skills-inactive"))
                    .filter(|path| seen.insert(crate::project_discovery::path_identity(path)))
                    .collect::<Vec<_>>();
                let multiple_workspaces = active.len() > 1;
                shortcuts = shortcuts.child(
                    div()
                        .id(ElementId::Name(
                            format!("project-proma-shortcut-{}", entry.path.display()).into(),
                        ))
                        .capture_any_mouse_down(cx.listener(
                            move |this, event: &MouseDownEvent, window, cx| {
                                let inactive_click = event.button == MouseButton::Right;
                                if inactive_click
                                    || (multiple_workspaces && event.button == MouseButton::Left)
                                {
                                    cx.stop_propagation();
                                    this.open_proma_directory_menu(
                                        if inactive_click {
                                            inactive.clone()
                                        } else {
                                            active.clone()
                                        },
                                        inactive_click,
                                    if inactive_click { event.position } else { event.position + point(px(-11.), px(14.)) },
                                        window,
                                        cx,
                                    );
                                }
                            },
                        ))
                        .child(shortcut),
                );
            } else {
                shortcuts = shortcuts.child(shortcut);
            }
        }
        shortcuts
    }

    fn open_proma_directory_menu(
        &mut self,
        paths: Vec<PathBuf>,
        inactive: bool,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let app = cx.entity().downgrade();
        let open_label = self
            .tr("打开停用技能目录", "Open inactive skill folder")
            .to_string();
        let missing_label = self
            .tr(
                if inactive {
                    "暂无停用技能目录"
                } else {
                    "暂无技能目录"
                },
                if inactive {
                    "No inactive skill folder yet"
                } else {
                    "No skill folder yet"
                },
            )
            .to_string();
        let menu = gpui_component::menu::PopupMenu::build(window, cx, move |mut menu, _, _| {
            if inactive && paths.len() > 1 {
                menu = menu.label(open_label.clone());
            }
            if paths.is_empty() {
                return menu.item(PopupMenuItem::new(missing_label.clone()).disabled(true));
            }
            for path in &paths {
                let available = path.is_dir();
                let label = if inactive && paths.len() == 1 {
                    if available {
                        open_label.clone()
                    } else {
                        missing_label.clone()
                    }
                } else {
                    let mut label = proma_directory_label(path);
                    if !available {
                        label.push_str(&format!(" · {missing_label}"));
                    }
                    label
                };
                let path = path.clone();
                let app = app.clone();
                menu = menu.item(
                    PopupMenuItem::new(label)
                        .icon(Icon::new(IconName::Folder))
                        .disabled(!available)
                        .on_click(move |_, _, cx| {
                            let _ = app.update(cx, |this, cx| {
                                this.open_project_skill_directory(&path, cx)
                            });
                        }),
                );
            }
            menu.min_w(px(200.)).max_w(px(480.))
        });
        let menu_id = menu.entity_id();
        cx.subscribe_in(&menu, window, move |this, _, _: &DismissEvent, _, cx| {
            if this
                .projects_view
                .proma_directory_menu
                .as_ref()
                .is_some_and(|(_, menu)| menu.entity_id() == menu_id)
            {
                this.projects_view.proma_directory_menu = None;
                cx.notify();
            }
        })
        .detach();
        menu.focus_handle(cx).focus(window, cx);
        self.projects_view.proma_directory_menu = Some((position, menu));
        cx.notify();
    }

    fn open_project_skill_directory(&mut self, path: &Path, cx: &mut Context<Self>) {
        if path.is_dir() {
            cx.open_with_system(path);
        } else {
            self.show_notice(
                self.tr(
                    "技能目录已不存在，请刷新后重试",
                    "The skill folder no longer exists; refresh and try again",
                ),
                cx,
            );
        }
    }

    pub(super) fn agent_badges(
        &self,
        popover_id: String,
        targets: &[InstallTarget],
        include_global_only: bool,
        proma_inactive: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let agents = crate::agents::AGENT_ICON_ORDER
            .iter()
            .filter(|agent| {
                if agent.global_only {
                    include_global_only
                } else {
                    targets.iter().any(|target| agent.supports_target(*target))
                }
            })
            .map(|agent| (agent.name, agent.icon_path))
            .collect::<Vec<_>>();
        self.render_agent_badges(popover_id, agents, proma_inactive, cx)
    }

    pub(super) fn effective_agent_badges(
        &self,
        popover_id: String,
        kinds: &[AgentKind],
        proma_inactive: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let agents = crate::agents::AGENT_ICON_ORDER
            .iter()
            .filter(|agent| kinds.iter().any(|kind| kind.id() == agent.id))
            .map(|agent| (agent.name, agent.icon_path))
            .collect::<Vec<_>>();
        self.render_agent_badges(popover_id, agents, proma_inactive, cx)
    }

    pub(super) fn render_agent_badges(
        &self,
        popover_id: String,
        mut agents: Vec<(&'static str, &'static str)>,
        proma_inactive: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        agents.sort_by_key(|(name, _)| crate::agents::display_sort_key(name, true));
        let p = self.palette();
        let dark = self.uses_dark_theme();
        let hidden = agents.len().saturating_sub(5);
        let visible = agents.len().min(5);
        let badge_width = if visible == 0 {
            0.
        } else {
            25. + (visible.saturating_sub(1) as f32 * 20.)
        } + if hidden > 0 { 20. } else { 0. };
        let brand_color = p.text;
        let inactive_label = self.tr("已停用", "Inactive").to_string();
        let mut badges = div().flex().items_center();
        for (index, (name, icon)) in agents.iter().take(5).enumerate() {
            let inactive = proma_inactive && *name == "Proma";
            badges = badges.child(
                div()
                    .id(ElementId::Name(format!("agent-badge-{popover_id}-{index}").into()))
                    .relative()
                    .role(Role::Image)
                    .aria_label(if inactive { format!("{name} · {inactive_label}") } else { name.to_string() })
                    .ml(if index == 0 { px(0.) } else { px(-5.) })
                    .size(px(25.))
                    .rounded_full()
                    .border_1()
                    .border_color(rgb(0xffffff))
                    .bg(rgb(0xf5f5f7))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(div().opacity(if inactive { 0.35 } else { 1. }).child(Self::brand_icon(
                        icon,
                        Self::brand_icon_size(icon, 17.),
                        p.text,
                        dark,
                    )))
                    .when(inactive, |badge| badge.child(
                        div().absolute().right(px(-3.)).bottom(px(-2.)).size(px(13.))
                            .rounded_full().bg(p.surface).border_1().border_color(p.border_strong)
                            .flex().items_center().justify_center()
                            .child(Self::icon("icons/pause.svg", 8., p.secondary))
                    )),
            );
        }
        if hidden > 0 {
            badges = badges.child(
                Popover::new(ElementId::Name(format!("agent-menu-{popover_id}").into()))
                    .appearance(false)
                    .anchor(Anchor::TopRight)
                    .trigger(
                        Button::new(ElementId::Name(format!("agent-more-{popover_id}").into()))
                            .label(format!("+{hidden}"))
                            .xsmall()
                            .custom(
                                ButtonCustomVariant::new(cx)
                                    .color(p.raised.into())
                                    .foreground(p.muted.into())
                                    .hover(p.hover.into())
                                    .active(p.selected.into()),
                            )
                            .ml(px(-5.))
                            .size(px(25.))
                            .rounded(px(999.))
                            .font_family(MONO)
                            .text_size(px(8.)),
                    )
                    .content(move |_, _, _| {
                        let mut menu = div()
                            .id("agent-popover-scroll")
                            .w(px(176.))
                            .max_h(px(246.))
                            .p(px(4.))
                            .rounded(px(RADIUS_MENU))
                            .border_1()
                            .border_color(p.border_strong)
                            .bg(p.elevated)
                            .shadow_lg()
                            .overflow_y_scroll();
                        for (name, icon) in agents.clone() {
                            let inactive = proma_inactive && name == "Proma";
                            menu = menu.child(
                                div()
                                    .h(px(29.))
                                    .px(px(8.))
                                    .rounded(px(RADIUS_CONTROL))
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(
                                        div()
                                            .size(px(24.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .opacity(if inactive { 0.35 } else { 1. })
                                            .child(Self::brand_icon(
                                                icon,
                                                Self::brand_icon_size(icon, 18.),
                                                brand_color,
                                                dark,
                                            )),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .text_color(p.secondary)
                                            .child(if inactive { format!("{name} · {inactive_label}") } else { name.to_string() }),
                                    ),
                            );
                        }
                        menu
                    }),
            );
        }
        div()
            .w(px(badge_width))
            .flex_none()
            .flex()
            .items_center()
            .child(badges)
    }
}
