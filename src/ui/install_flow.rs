use super::*;

impl KitterApp {
    pub(super) fn install_modal(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let p = self.palette();
        let has_target = !self.install_flow.selected_targets.is_empty()
            && (!self.install_flow.selected_targets.contains(&InstallTarget::Proma)
                || self.can_install_proma())
            && if self.install_flow.global {
                dirs::home_dir().is_some()
            } else {
                !self.install_flow.selected_projects.is_empty()
            };
        let selected_keys = self.selected_skill_keys();
        let selected_count = selected_keys.len();
        let selected_label = if selected_count > 1 {
            if self.uses_english() {
                counted(selected_count, "skill", "skills")
            } else {
                format!("{} 个技能", selected_count)
            }
        } else {
            self.selected_skill()
                .map(|skill| skill.record.name.clone())
                .unwrap_or_else(|| self.tr("技能", "Skill").to_string())
        };
        let project_picker = self.install_projects_picker(cx);
        let close_button = div()
            .id("close-install-modal")
            .size(px(CONTROL_HEIGHT))
            .rounded(px(RADIUS_CONTROL))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .bg(p.raised)
            .hover(move |button| button.bg(p.hover))
            .text_color(p.text)
            .child(Self::icon("icons/x.svg", 14., p.text))
            .on_click(cx.listener(|this, _, _, cx| {
                this.install_flow.modal = false;
                this.close_dialog(cx);
            }));
        let install_content = div()
            .id("install-modal-content")
            .px(px(20.))
            .pb(px(20.))
            .max_h(px((f32::from(window.bounds().size.height) - 180.).clamp(300., 700.)))
            .overflow_y_scroll()
            .overflow_x_hidden()
            .bg(p.elevated)
            .child(
                div()
                    .mb(px(7.))
                    .text_size(px(14.))
                    .text_color(p.muted)
                    .child(self.tr("生效范围", "Scope")),
            )
            .child(project_picker)
            .child(
                div()
                    .mt(px(18.))
                    .mb(px(7.))
                    .text_size(px(13.))
                    .text_color(p.secondary)
                    .child(self.tr("技能目录", "Skill directories")),
            )
            .child(self.target_group(cx))
            .child(self.install_destination_preview());
        let panel = div()
            .relative()
            .bg(p.elevated)
            .rounded(px(RADIUS_MODAL))
            .overflow_hidden()
            .child(
                div()
                    .rounded_tl(px(RADIUS_MODAL))
                    .rounded_tr(px(RADIUS_MODAL))
                    .bg(p.elevated)
                    .px(px(20.))
                    .pt(px(20.))
                    .pb(px(14.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .font_family(FONT_UI)
                                    .text_size(px(16.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(if self.uses_english() {
                                        format!("Install {}", selected_label.clone())
                                    } else {
                                        format!("安装 {}", selected_label.clone())
                                    }),
                            )
                            .child(div().flex_1())
                            .child(close_button),
                    ),
            )
            .child(install_content)
            .child(
                div()
                    .h(px(60.))
                    .rounded_bl(px(RADIUS_MODAL))
                    .rounded_br(px(RADIUS_MODAL))
                    .px(px(20.))
                    .border_t_1()
                    .border_color(p.border)
                    .bg(p.elevated)
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .child(div().flex_1().text_size(px(12.)).text_color(p.muted)
                        .child(if self.install_flow.global {
                            self.tr("全局安装", "Global install").to_string()
                        } else if self.uses_english() {
                            format!("{selected_count} skills · {} projects · {} targets", self.install_flow.selected_projects.len(), self.install_flow.selected_targets.len())
                        } else {
                            format!("{selected_count} 个技能 · {} 个项目 · {} 个目标", self.install_flow.selected_projects.len(), self.install_flow.selected_targets.len())
                        }))
                    .child(
                        div()
                            .id("cancel-install")
                            .h(px(DIALOG_CONTROL_HEIGHT))
                            .px(px(16.))
                            .rounded(px(RADIUS_CONTROL))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .text_size(px(14.))
                            .hover(move |button| button.bg(p.hover))
                            .child(self.tr("取消", "Cancel"))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.install_flow.modal = false;
                                    this.close_dialog(cx);
                                }),
                            ),
                    )
                    .child(
                        div()
                            .id("confirm-install").debug_selector(|| "confirm-install".into())
                            .h(px(DIALOG_CONTROL_HEIGHT))
                            .px(px(16.))
                            .rounded(px(RADIUS_CONTROL))
                            .border_1()
                            .border_color(p.border)
                            .bg(if has_target { p.accent_fill } else { p.raised })
                            .text_color(if has_target { p.on_accent } else { p.muted })
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .text_size(px(14.))
                            .child(self.tr("安装", "Install"))
                            .when(has_target, |el| {
                                el.hover(move |button| button.opacity(0.82)).on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.install_selected(cx)),
                                )
                            }),
                    ),
            );
        let _ = window;
        panel
    }

    fn install_projects_picker(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette();
        let global = self.install_flow.global;
        let mut scope = div().flex().gap(px(8.));
        for (id, is_global, label) in [
            ("install-scope-projects", false, self.tr("项目安装 · 可多选", "Projects · select multiple")),
            ("install-scope-global", true, self.tr("全局安装", "Global install")),
        ] {
            scope = scope.child(div().id(id).debug_selector(move || id.into()).h(px(36.)).px(px(12.))
                .rounded(px(RADIUS_CONTROL)).border_1().border_color(p.border)
                .bg(if global == is_global { p.selected } else { p.surface })
                .flex().items_center().gap(px(8.)).cursor_pointer().text_size(px(13.))
                .child(Checkbox::new(ElementId::Name(format!("{id}-checkbox").into()))
                    .checked(global == is_global))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.install_flow.global = is_global;
                    this.install_flow.project_picker_open = false;
                    if is_global {
                        this.install_flow.selected_targets.remove(&InstallTarget::Proma);
                    }
                    this.notify_dialog(cx);
                })));
        }
        let mut body = div().child(scope);
        if global {
            return body.child(div().mt(px(9.)).text_size(px(12.)).text_color(p.muted)
                .child(self.tr("安装到 Agent 的用户级技能目录", "Install to each Agent's user-level skills directory")));
        }
        let count = self.install_flow.selected_projects.len();
        let label = if self.uses_english() {
            format!("{count} projects selected")
        } else {
            format!("已选 {count} 个项目")
        };
        let app = cx.entity().downgrade();
        let open_app = app.clone();
        let focus = self.install_flow.project_search.read(cx).focus_handle(cx);
        let picker = Popover::new("install-project-menu")
            .appearance(false)
            .anchor(Anchor::TopRight)
            .open(self.install_flow.project_picker_open)
            .track_focus(&focus)
            .on_open_change(move |open, _, cx| {
                let _ = open_app.update(cx, |this, cx| {
                    this.install_flow.project_picker_open = *open;
                    this.notify_dialog(cx);
                });
            })
            .trigger(self.dropdown_button("install-project-select", label, 13.).w_full())
            .content(move |_, _, popover_cx| {
                app.update(popover_cx, |this, cx| this.install_projects_menu(cx))
                    .unwrap_or_else(|_| div())
            });
        body = body.child(div().mt(px(12.)).flex().items_center().gap(px(8.))
            .child(div().id("install-project-select-wrapper")
                .debug_selector(|| "install-project-select".into()).flex_1().child(picker))
            .child(self.bordered_icon_button("modal-browse", "icons/folder.svg", p.text, cx)
                .on_mouse_down(MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.browse_project(window, cx)))));
        body.child(div().mt(px(8.)).text_size(px(12.)).text_color(p.muted)
            .whitespace_normal()
            .child(self.tr("可直接选择本地项目文件夹；Proma 需关联自己的工作区。",
                "Choose local project folders directly; Proma needs a linked workspace.")))
    }

    fn install_destination_preview(&self) -> Div {
        let p = self.palette();
        let global = self.install_flow.global;
        let projects = if global {
            dirs::home_dir().into_iter().collect::<Vec<_>>()
        } else {
            self.install_flow.selected_projects.iter().cloned().collect::<Vec<_>>()
        };
        if projects.is_empty() {
            return div();
        }
        let project_count = projects.len();
        let entries = self.project_entries();
        let selected = self.selected_skill_keys();
        let skill_name = (selected.len() == 1).then(|| {
            self.model.skills.iter()
                .find(|skill| selected.iter().any(|key| key == skill_storage_name(skill)))
                .map(|skill| skill.record.name.clone())
        }).flatten();
        let targets = crate::agents::PROJECT_INSTALL_TARGETS.iter().copied()
            .chain(std::iter::once(InstallTarget::Proma))
            .filter(|target| self.install_flow.selected_targets.contains(target))
            .collect::<Vec<_>>();
        let mut rows = div().id("install-destination-list")
            .max_h(px(180.)).overflow_y_scroll().flex().flex_col();
        for (index, project) in projects.into_iter().enumerate() {
            let name = if global {
                self.tr("用户级 · 所有项目", "User-level · all projects").to_string()
            } else {
                entries.iter().find(|entry| entry.path == project)
                    .map(|entry| entry.name.clone())
                    .unwrap_or_else(|| project.file_name().unwrap_or_default().to_string_lossy().into_owned())
            };
            let workspace = (!global).then(|| crate::proma::installation_workspace(&project));
            let mut row = div().px(px(12.)).py(px(9.)).flex().flex_col().gap(px(5.))
                .when(index > 0, |row| row.border_t_1().border_color(p.border))
                .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).child(name));
            if targets.is_empty() {
                row = row.child(div().font_family(MONO).text_size(px(11.))
                    .text_color(p.muted).whitespace_normal().child(project.display().to_string()));
            }
            for target in &targets {
                let directory = if *target == InstallTarget::Proma {
                    workspace.as_ref().and_then(|result| result.as_ref().ok())
                        .map(|workspace| workspace.path.join("skills"))
                } else if global {
                    crate::agents::global_target_root(&project, *target).ok()
                } else {
                    crate::agents::installation_root(&project, *target).ok()
                };
                let Some(mut path) = directory else {
                    row = row.child(div().text_size(px(11.)).text_color(p.muted)
                        .child(self.tr("安装目录配置无效，请检查后重试", "Invalid install directory; check configuration and retry")));
                    continue;
                };
                if let Some(name) = &skill_name {
                    path.push(name);
                }
                let label = if *target == InstallTarget::Universal {
                    self.tr("共享目录", "Shared directory")
                } else {
                    crate::agents::INDEPENDENT_INSTALL_TARGETS.iter()
                        .find(|info| info.target == *target).map(|info| info.name).unwrap_or_default()
                };
                row = row.child(div().flex().flex_col().gap(px(2.))
                    .child(div().text_size(px(11.)).text_color(p.secondary).child(label))
                    .child(div().font_family(MONO).text_size(px(11.))
                        .text_color(p.muted).whitespace_normal().child(path.display().to_string())));
            }
            if let Some(Err(_)) = workspace {
                let reason = match crate::proma::project_workspaces(&project) {
                    Ok(workspaces) if workspaces.is_empty() => {
                        self.tr("未关联工作区", "No workspace linked").to_string()
                    }
                    Ok(workspaces) if workspaces.len() > 1 => {
                        if self.uses_english() {
                            format!("{} workspaces linked; one installation workspace must be specified", workspaces.len())
                        } else {
                            format!("关联 {} 个工作区，需要明确安装工作区", workspaces.len())
                        }
                    }
                    _ => self.tr("无法读取或确认工作区", "Cannot read or resolve the workspace").to_string(),
                };
                let message = if self.uses_english() {
                    format!("Proma unavailable: {reason}")
                } else {
                    format!("Proma 暂不可选：{reason}")
                };
                row = row.child(div().text_size(px(11.)).whitespace_normal()
                    .text_color(if self.install_flow.selected_targets.contains(&InstallTarget::Proma) {
                        p.danger
                    } else {
                        p.muted
                    }).child(message));
            }
            rows = rows.child(row);
        }
        div().mt(px(14.))
            .child(div().mb(px(7.)).text_size(px(12.)).text_color(p.secondary)
                .child(if global {
                    self.tr("实际安装位置 · 用户级", "Actual destinations · user-level").to_string()
                } else if self.uses_english() {
                    format!("Actual destinations · {project_count} projects")
                } else {
                    format!("实际安装位置 · {project_count} 个项目")
                }))
            .child(div().rounded(px(RADIUS_CONTROL)).border_1().border_color(p.border)
                .bg(p.surface).overflow_hidden().child(rows))
            .child(div().mt(px(5.)).text_size(px(11.)).text_color(p.muted).whitespace_normal()
                .child(if targets.is_empty() {
                    self.tr("选择上方的技能目录后，这里会显示将写入的完整路径。",
                        "Choose skill directories above to see the full destination paths.")
                } else if selected.len() > 1 {
                    self.tr("每个技能会在这些目录下各占一个子目录。",
                        "Each skill gets its own subdirectory in these locations.")
                } else {
                    self.tr("仅安装到以上位置。", "Installs only to the locations above.")
                }))
    }

    pub(super) fn matching_install_projects(&self, cx: &App) -> Vec<crate::project_discovery::ProjectEntry> {
        let query = self.install_flow.project_search.read(cx).value().trim().to_lowercase();
        let home = dirs::home_dir();
        self.project_entries().into_iter()
            .filter(|entry| Some(&entry.path) != home.as_ref())
            .filter(|entry| entry.name.to_lowercase().contains(&query)
                || entry.path.to_string_lossy().to_lowercase().contains(&query)
                || display_path(&entry.path).to_lowercase().contains(&query))
            .collect()
    }

    fn install_projects_menu(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette();
        let choices = self.matching_install_projects(cx);
        let has_query = !self.install_flow.project_search.read(cx).value().trim().is_empty();
        let matching_paths = choices.iter().map(|entry| entry.path.clone()).collect::<BTreeSet<_>>();
        let count = self.install_flow.selected_projects.len();
        let mut menu_background = p.elevated;
        menu_background.a = 1.;
        let mut toolbar = div().py(px(8.)).flex().items_center()
            .gap(px(8.)).text_size(px(12.)).text_color(p.muted)
            .child(if self.uses_english() { format!("{count} selected · {} results", choices.len()) }
                else { format!("已选 {count} 个 · {} 个结果", choices.len()) })
            .child(div().flex_1());
        for (id, label, select_all) in [
            ("install-projects-select-all", if has_query { self.tr("全选结果", "Select results") } else { self.tr("全选", "Select all") }, true),
            ("install-projects-clear", self.tr("清空全部", "Clear all"), false),
        ] {
            let paths = matching_paths.clone();
            toolbar = toolbar.child(div().id(id).debug_selector(move || id.into()).h(px(28.)).px(px(7.))
                .rounded(px(RADIUS_CONTROL)).flex().items_center().cursor_pointer()
                .hover(move |button| button.bg(p.hover)).child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if select_all {
                        this.install_flow.selected_projects.extend(paths.iter().cloned());
                    } else {
                        this.install_flow.selected_projects.clear();
                    }
                    this.notify_dialog(cx);
                })));
        }
        let mut rows = div().id("install-project-list").h(px(240.)).overflow_y_scroll()
            .track_scroll(&self.install_flow.project_scroll)
            .rounded(px(RADIUS_CONTROL)).border_1().border_color(p.border).bg(p.surface);
        if choices.is_empty() {
            rows = rows.child(div().p(px(16.)).text_size(px(13.)).text_color(p.muted)
                .child(self.tr("没有匹配的项目", "No matching projects")));
        }
        for (index, entry) in choices.into_iter().enumerate() {
            let checked = self.install_flow.selected_projects.contains(&entry.path);
            let path = entry.path;
            rows = rows.child(div().id(ElementId::Name(format!("install-project-option-{index}").into()))
                .debug_selector(move || format!("install-project-option-{index}"))
                .min_h(px(52.)).px(px(11.)).py(px(6.)).flex().items_center().gap(px(9.))
                .cursor_pointer().bg(if checked { p.selected } else { p.surface })
                .hover(move |row| row.bg(p.hover))
                .child(Checkbox::new(ElementId::Name(format!("install-project-check-{index}").into())).checked(checked))
                .child(div().min_w_0().flex_1().flex().flex_col().gap(px(3.))
                    .child(div().text_size(px(13.)).truncate().child(entry.name))
                    .child(div().font_family(MONO).text_size(px(11.)).text_color(p.muted)
                        .truncate().child(display_path(&path))))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.install_flow.selected_projects.remove(&path) {
                        this.install_flow.selected_projects.insert(path.clone());
                    }
                    this.notify_dialog(cx);
                })));
        }
        div().w(px(512.)).p(px(10.)).rounded(px(RADIUS_MENU))
            .border_1().border_color(p.border_strong).bg(menu_background).shadow_lg()
            .child(div().debug_selector(|| "install-project-search".into())
                .child(Input::new(&self.install_flow.project_search).w_full()
                    .prefix(Self::icon("icons/search.svg", 15., p.muted))))
            .child(toolbar).child(rows)
            .child(div().mt(px(8.)).flex().justify_end()
                .child(div().id("install-projects-done").debug_selector(|| "install-projects-done".into())
                    .h(px(30.)).px(px(12.)).rounded(px(RADIUS_CONTROL))
                    .flex().items_center().cursor_pointer().text_size(px(13.))
                    .bg(p.selected).hover(move |button| button.bg(p.hover))
                    .child(self.tr("完成选择", "Done"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.install_flow.project_picker_open = false;
                        this.shell.focus_handle.focus(window, cx);
                        this.notify_dialog(cx);
                    }))))
    }

    pub(super) fn can_install_proma(&self) -> bool {
        if self.install_flow.global { return false; }
        if self.install_flow.modal {
            !self.install_flow.selected_projects.is_empty()
                && self.install_flow.selected_projects.iter()
                    .all(|project| crate::proma::installation_workspace(project).is_ok())
        } else {
            self.projects_view.open_project.as_ref()
                .is_some_and(|project| crate::proma::installation_workspace(project).is_ok())
        }
    }

    pub(super) fn toggle_install_target(&mut self, target: InstallTarget) {
        if self.install_flow.selected_targets.remove(&target) { return; }
        if target != InstallTarget::Universal
            && !self.available_project_agents().iter().any(|agent| agent.owns_target(target)) { return; }
        if target != InstallTarget::Proma || self.can_install_proma() {
            self.install_flow.selected_targets.insert(target);
        }
    }

    pub(super) fn target_group(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette();
        let universal = self
            .install_flow
            .selected_targets
            .contains(&InstallTarget::Universal);
        let available_agents = self.available_project_agents();
        let compatible_agents = available_agents.iter()
            .filter(|agent| !agent.global_only && agent.supports_target(InstallTarget::Universal))
            .map(|agent| agent.name).collect::<Vec<_>>().join(" · ");
        let compatible_label = if self.uses_english() {
            format!("Available compatible agents: {compatible_agents}")
        } else {
            format!("可管理的兼容 Agent：{compatible_agents}")
        };
        let universal_box = div()
            .rounded(px(RADIUS_CARD))
            .border_1()
            .border_color(p.border)
            .bg(p.surface)
            .overflow_hidden()
            .child(
                div()
                    .id("target-universal")
                    .h(px(48.))
                    .px(px(13.))
                    .cursor_pointer()
                    .bg(if universal { p.selected } else { p.surface })
                    .when(universal, |row| {
                        row.rounded_tl(px(RADIUS_CARD - 1.))
                            .rounded_tr(px(RADIUS_CARD - 1.))
                    })
                    .flex()
                    .items_center()
                    .child(Checkbox::new("target-universal-checkbox").checked(universal))
                    .child(
                        div()
                            .ml(px(9.))
                            .text_size(px(14.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.tr("共享目录", "Shared directory")),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .font_family(MONO)
                            .text_size(px(12.))
                            .text_color(p.muted)
                            .child(if self.install_flow.global {
                                "~/.agents/skills"
                            } else {
                                self.tr("项目目录/.agents/skills", "project/.agents/skills")
                            }),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_install_target(InstallTarget::Universal);
                        this.notify_dialog(cx);
                    })),
            )
            .child(div().px(px(13.)).pb(px(10.)).text_size(px(11.))
                .text_color(p.muted).whitespace_normal().child(compatible_label));
        let mut independent_rows = div().flex().flex_wrap();
        let mut targets = crate::agents::INDEPENDENT_INSTALL_TARGETS.to_vec();
        targets.sort_by_key(|info| crate::agents::display_sort_key(info.name,
            available_agents.iter().any(|agent| agent.owns_target(info.target))));
        for info in targets {
            if info.target == InstallTarget::Proma && self.install_flow.global {
                continue;
            }
            let available = available_agents.iter().any(|agent| agent.owns_target(info.target));
            independent_rows = independent_rows.child(self.target_row(info, available, cx).min_w_0().flex_basis(relative(0.5)));
        }
        let independent_box = div()
            .mt(px(10.))
            .rounded(px(RADIUS_CARD))
            .border_1()
            .border_color(p.border)
            .bg(p.surface)
            .overflow_hidden()
            .child(
                div()
                    .px(px(13.))
                    .h(px(42.))
                    .flex()
                    .items_center()
                    .text_size(px(14.))
                    .font_weight(FontWeight::MEDIUM)
                    .child(self.tr("Agent 专用目录", "Agent directories")),
            )
            .child(independent_rows)
            .child(div().px(px(13.)).py(px(9.)).border_t_1().border_color(p.border)
                .text_size(px(11.)).text_color(p.muted).whitespace_normal()
                .child(self.tr("已使用共享目录时，兼容的 Agent 通常无需重复选择。",
                    "With a shared directory, compatible Agents usually need no extra selection.")));
        div().child(universal_box).child(independent_box)
    }

    pub(super) fn target_row(
        &self,
        info: crate::agents::InstallTargetInfo,
        available: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let p = self.palette();
        let id = match info.target {
            InstallTarget::Codex => "target-codex",
            InstallTarget::ClaudeCode => "target-claude",
            InstallTarget::Cursor => "target-cursor",
            InstallTarget::OpenCode => "target-opencode",
            InstallTarget::Pi => "target-pi",
            InstallTarget::Proma => "target-proma",
            InstallTarget::Omp => "target-omp",
            InstallTarget::Grok => "target-grok",
            InstallTarget::Antigravity => "target-antigravity",
            InstallTarget::Droid => "target-droid",
            InstallTarget::Copilot => "target-copilot",
            InstallTarget::Trae => "target-trae",
            InstallTarget::WorkBuddy => "target-workbuddy",
            InstallTarget::OpenClaw => "target-openclaw",
            InstallTarget::Hermes => "target-hermes",
            InstallTarget::Universal => "target-universal",
        };
        let active = self.install_flow.selected_targets.contains(&info.target);
        let installed = self.model.installed_agents.iter().any(|agent| agent.owns_target(info.target));
        let available = available && (info.target != InstallTarget::Proma || self.can_install_proma());
        let is_last = crate::agents::INDEPENDENT_INSTALL_TARGETS
            .last()
            .is_some_and(|last| last.target == info.target);
        div()
            .id(id).debug_selector(move || id.into())
            .h(px(48.))
            .px(px(13.))
            .border_t_1()
            .border_color(p.border)
            .bg(if active { p.selected } else { p.surface })
            .text_color(if available { p.text } else { p.muted })
            .when(installed && !available, |row| row.opacity(0.55))
            .when(is_last, |row| {
                row.rounded_bl(px(RADIUS_CARD - 1.))
                    .rounded_br(px(RADIUS_CARD - 1.))
            })
            .flex()
            .items_center()
            .when(available || active, |row| row.cursor_pointer())
            .child(Checkbox::new(ElementId::Name(format!("{id}-checkbox").into())).checked(active))
            .child(
                div()
                    .ml(px(9.))
                    .size(px(28.))
                    .rounded(px(RADIUS_CONTROL))
                    .bg(if info.icon_path == "icons/provider-codex.svg" {
                        rgba(0x00000000)
                    } else {
                        p.raised
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(self.agent_status_icon(
                        info.icon_path,
                        18.,
                        installed,
                    )),
            )
            .child(div().ml(px(9.)).min_w_0().flex_1().truncate().text_size(px(13.)).child(info.name))
            .child(
                div()
                    .font_family(MONO)
                    .text_size(px(11.))
                    .text_color(p.muted)
                    .ml(px(8.)).max_w(px(90.)).truncate()
                    .child(if !installed && available {
                        self.tr("有关联项目", "Project associated").to_string()
                    } else if !installed {
                        self.tr("未检测到", "Not detected").to_string()
                    } else if self.install_flow.global {
                        dirs::home_dir()
                            .map(|home| {
                                crate::agents::global_target_root(&home, info.target).map(|root| display_path(&root))
                                    .unwrap_or_else(|_| self.tr("目录配置无效", "Invalid directory").to_string())
                            })
                            .unwrap_or_default()
                    } else if info.target == InstallTarget::Proma && !self.can_install_proma() {
                        if self.install_flow.selected_projects.is_empty() {
                            self.tr("先选择项目", "Choose projects").to_string()
                        } else {
                            self.tr("见下方说明", "See details below").to_string()
                        }
                    } else {
                        crate::agents::project_target_directory(info.target)
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|_| self.tr("目录配置无效", "Invalid directory").to_string())
                    }),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if !available && !active { return; }
                this.toggle_install_target(info.target);
                this.notify_dialog(cx);
            }))
    }

    pub(super) fn delete_confirmation_modal(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let p = self.palette();
        let Some(confirmation) = self.delete_flow.confirmation.clone() else {
            return div();
        };
        let (title, message, location, action, success) = match &confirmation {
            DeleteConfirmation::LibrarySkills { skills } => {
                let linked_count = skills
                    .iter()
                    .filter(|(name, _)| self.model.library.is_linked_source(name))
                    .count();
                let labels = skills
                    .iter()
                    .map(|(name, _)| {
                        self.model
                            .skills
                            .iter()
                            .find(|skill| skill_storage_name(skill) == name.as_str())
                            .map(|skill| skill.record.name.clone())
                            .unwrap_or_else(|| name.clone())
                    })
                    .collect::<Vec<_>>();
                let label = labels.join(", ");
                (
                    if labels.len() == 1 {
                        if self.uses_english() {
                            format!("Delete “{label}”?")
                        } else {
                            format!("删除「{label}」？")
                        }
                    } else if self.uses_english() {
                        format!("Delete {} skills?", labels.len())
                    } else {
                        format!("删除 {} 个技能？", labels.len())
                    },
                    (if linked_count == skills.len() {
                        self.tr("仅移除 Pleat 托管与安装链接，原始源目录保留。", "Remove Pleat entries and installation links. Original source folders are kept.")
                    } else if linked_count > 0 {
                        self.tr("托管副本与安装将被删除；链接来源的原始目录保留。", "Managed copies and installations will be removed. Linked source folders are kept.")
                    } else { self.tr(
                        "这会从 Pleat 中删除这些技能，同时移除它们在项目中的安装。以下位置的文件也会被删除，且无法恢复：",
                        "This removes these skills from Pleat and from projects where they are installed. Files at these locations will also be deleted and cannot be restored:",
                    ) })
                    .to_string(),
                    Some(
                        skills
                            .iter()
                            .map(|(_, path)| display_path(path))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                    self.tr("删除", "Delete"),
                    self.tr("技能已删除", "Skills deleted"),
                )
            }
            DeleteConfirmation::ProjectSkills {
                project,
                skills,
                batch,
            } => {
                let global_scope = dirs::home_dir()
                    .as_ref()
                    .is_some_and(|home| home == project);
                let kinds = skills
                    .iter()
                    .flat_map(|skill| &skill.installations)
                    .filter(|installation| self.delete_flow.selected.contains(&installation.path))
                    .map(project::removal_kind)
                    .collect::<Vec<_>>();
                let deletes_files = kinds
                    .iter()
                    .any(|kind| matches!(kind, project::RemovalKind::SourceFiles));
                let links_only = !kinds.is_empty() && kinds.iter().all(|kind| matches!(kind, project::RemovalKind::ExternalLink { .. }));
                let external_sources = unique_external_sources(&kinds);
                let message = if *batch && global_scope {
                    self.tr(
                        "直接保存在用户级目录中的技能文件会被删除；通过链接安装的技能只会移除链接，原始文件会保留。",
                        "Skill files stored directly in user-level locations will be deleted. For linked skills, only the links will be removed; the original files will remain.",
                    )
                } else if *batch {
                    self.tr(
                        "直接保存在此项目中的技能文件会被删除；通过链接安装的技能只会移除链接，原始文件会保留。",
                        "Skill files stored directly in this project will be deleted. For linked skills, only the links will be removed; the original files will remain.",
                    )
                } else if deletes_files {
                    if global_scope {
                        self.tr(
                            "所选用户级安装中包含直接保存的技能，确认后这些文件会被删除，且无法恢复。",
                            "Some selected user-level installations are stored directly there. Their files will be deleted and cannot be restored.",
                        )
                    } else {
                        self.tr(
                            "所选位置中包含直接保存在项目里的技能，确认后这些文件会被删除，且无法恢复。",
                            "Some selected installations are stored directly in the project. Their files will be deleted and cannot be restored.",
                        )
                    }
                } else if !external_sources.is_empty() {
                    if global_scope {
                        self.tr(
                            "只会移除选中的用户级安装，原始技能文件不会受到影响。",
                            "Only the selected user-level installations will be removed. The original skill files will not be affected.",
                        )
                    } else {
                        self.tr(
                            "只会移除这个项目中的安装，原始技能文件不会受到影响。",
                            "Only the project installations will be removed. The original skill files will not be affected.",
                        )
                    }
                } else {
                    if global_scope {
                        self.tr(
                            "只会移除选中的用户级安装。Pleat 中保存的技能不会被删除，你之后仍可以再次安装。",
                            "Only the selected user-level installations will be removed. The skill saved in Pleat will remain available to install again.",
                        )
                    } else {
                        self.tr(
                            "只会移除这个项目中的安装。Pleat 中保存的技能不会被删除，你之后仍可以再次安装。",
                            "Only the project installations will be removed. The skill saved in Pleat will remain available to install again.",
                        )
                    }
                };
                (
                    if *batch && global_scope {
                        self.tr(
                            "从用户级目录卸载所选技能？",
                            "Uninstall selected skills from user-level locations?",
                        )
                        .to_string()
                    } else if *batch {
                        self.tr(
                            "从此项目卸载所选技能？",
                            "Uninstall selected skills from this project?",
                        )
                        .to_string()
                    } else if global_scope && self.uses_english() {
                        format!("Uninstall “{}” from user-level locations?", skills[0].name)
                    } else if global_scope {
                        format!("从用户级目录中卸载「{}」？", skills[0].name)
                    } else if self.uses_english() {
                        format!("Uninstall “{}” from this project?", skills[0].name)
                    } else {
                        format!("从这个项目中卸载「{}」？", skills[0].name)
                    },
                    message.to_string(),
                    (!*batch && !external_sources.is_empty()).then(|| external_sources.join("\n")),
                    if links_only {
                        self.tr("仅移除链接", "Remove links only")
                    } else if deletes_files {
                        self.tr("删除", "Delete")
                    } else {
                        self.tr("卸载", "Uninstall")
                    },
                    if global_scope {
                        self.tr("已从用户级目录中卸载", "Uninstalled from user-level locations")
                    } else {
                        self.tr("已从项目中卸载", "Uninstalled from project")
                    },
                )
            }
        };

        let close_button = div()
            .id("close-delete-modal")
            .size(px(CONTROL_HEIGHT))
            .flex_shrink_0()
            .rounded(px(RADIUS_CONTROL))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .bg(p.raised)
            .hover(move |button| button.bg(p.hover))
            .text_color(p.text)
            .child(Self::icon("icons/x.svg", 14., p.text))
            .on_click(cx.listener(|this, _, _, cx| {
                this.delete_flow.confirmation = None;
                this.close_dialog(cx);
            }));
        let mut body = div()
            .id("delete-confirmation-body")
            .max_h(px(520.))
            .overflow_y_scroll()
            .rounded_tl(px(RADIUS_MODAL))
            .rounded_tr(px(RADIUS_MODAL))
            .bg(p.elevated)
            .p(px(20.))
            .child(
                div()
                    .min_w_0()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .id("delete-confirmation-title")
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title.clone())
                            .tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx)),
                    )
                    .child(close_button),
            )
            .child(
                div()
                    .mt(px(10.))
                    .text_size(px(14.))
                    .line_height(relative(1.55))
                    .text_color(p.secondary)
                    .child(message),
            )
            .when_some(location, |body, location| {
                body.child(
                    div()
                        .mt(px(10.))
                        .px(px(11.))
                        .py(px(9.))
                        .rounded(px(RADIUS_CONTROL))
                        .bg(p.raised)
                        .font_family(MONO)
                        .text_size(px(12.))
                        .text_color(p.secondary)
                        .child(location),
                )
            });
        if let DeleteConfirmation::ProjectSkills { skills, batch, .. } = &confirmation {
            body = body.child(
                div()
                    .mt(px(16.))
                    .mb(px(6.))
                    .text_size(px(13.))
                    .text_color(p.secondary)
                    .child(self.tr("选择要卸载的位置", "Choose installations to uninstall")),
            );
            for skill in skills {
                if *batch || skills.len() > 1 {
                    body = body.child(
                        div()
                            .mt(px(12.))
                            .font_family(MONO)
                            .text_size(px(12.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(skill.name.clone()),
                    );
                }
                let unique_paths = unique_installation_paths(&skill.installations);
                for installation in skill
                    .installations
                    .iter()
                    .filter(|installation| unique_paths.contains(&installation.path))
                {
                    let path = installation.path.clone();
                    let checked = self.delete_flow.selected.contains(&path);
                    let target = crate::agents::target_directory(installation.target);
                    let removal_kind = project::removal_kind(installation);
                    let deletes_files = matches!(removal_kind, project::RemovalKind::SourceFiles);
                    let external_source = match removal_kind {
                        project::RemovalKind::ExternalLink { source } if *batch => {
                            Some(display_path(&source))
                        }
                        _ => None,
                    };
                    body = body.child(
                        div()
                            .id(ElementId::Name(
                                format!("delete-path-{}", path.display()).into(),
                            ))
                            .mt(px(6.))
                            .min_h(px(if external_source.is_some() { 64. } else { 48. }))
                            .px(px(11.))
                            .rounded(px(RADIUS_CARD))
                            .border_1()
                            .border_color(p.border)
                            .bg(p.surface)
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .child(
                                Checkbox::new(ElementId::Name(
                                    format!("delete-checkbox-{}", path.display()).into(),
                                ))
                                .checked(checked),
                            )
                            .child(
                                div()
                                    .ml(px(10.))
                                    .min_w_0()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(8.))
                                            .text_size(px(13.))
                                            .child(target)
                                            .when(*batch && deletes_files, |title| {
                                                title.child(
                                                    div()
                                                        .text_size(px(11.))
                                                        .text_color(p.danger)
                                                        .child(self.tr("删除文件", "Delete files")),
                                                )
                                            }),
                                    )
                                    .child(
                                        div()
                                            .mt(px(2.))
                                            .font_family(MONO)
                                            .text_size(px(11.))
                                            .text_color(p.muted)
                                            .truncate()
                                            .child(display_path(&path)),
                                    )
                                    .when_some(external_source, |row, source| {
                                        row.child(
                                            div()
                                                .mt(px(2.))
                                                .font_family(MONO)
                                                .text_size(px(11.))
                                                .text_color(p.muted)
                                                .child(if self.uses_english() {
                                                    format!("Original: {source}")
                                                } else {
                                                    format!("原始目录：{source}")
                                                }),
                                        )
                                    }),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.delete_flow.selected.remove(&path) {
                                    this.delete_flow.selected.insert(path.clone());
                                }
                                this.notify_dialog(cx);
                            })),
                    );
                }
            }
        }
        let can_confirm = !self.delete_flow.busy
            && (!matches!(confirmation, DeleteConfirmation::ProjectSkills { .. })
                || !self.delete_flow.selected.is_empty());
        div()
            .relative()
            .rounded(px(RADIUS_MODAL))
            .bg(p.elevated)
            .overflow_hidden()
            .child(body)
            .child(
                div()
                    .h(px(60.))
                    .rounded_bl(px(RADIUS_MODAL))
                    .rounded_br(px(RADIUS_MODAL))
                    .px(px(18.))
                    .border_t_1()
                    .border_color(p.border)
                    .bg(p.elevated)
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        div()
                            .id("cancel-delete")
                            .h(px(DIALOG_CONTROL_HEIGHT))
                            .px(px(16.))
                            .rounded(px(RADIUS_CONTROL))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .text_size(px(14.))
                            .hover(move |button| button.bg(p.hover))
                            .child(self.tr("取消", "Cancel"))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.delete_flow.confirmation = None;
                                    this.close_dialog(cx);
                                }),
                            ),
                    )
                    .child(
                        div()
                            .id("confirm-delete")
                            .h(px(DIALOG_CONTROL_HEIGHT))
                            .px(px(16.))
                            .rounded(px(RADIUS_CONTROL))
                            .border_1()
                            .border_color(p.border)
                            .bg(if can_confirm { p.danger } else { p.danger_soft })
                            .flex()
                            .items_center()
                            .cursor(if can_confirm {
                                CursorStyle::PointingHand
                            } else {
                                CursorStyle::Arrow
                            })
                            .text_size(px(14.))
                            .text_color(if can_confirm { p.on_accent } else { p.muted })
                            .child(action)
                            .when(can_confirm, |button| {
                                button.on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
                                    if this.delete_flow.busy {
                                        return;
                                    }
                                    this.delete_flow.busy = true;
                                    match &confirmation {
                                        DeleteConfirmation::LibrarySkills { skills } => {
                                            let mut removed = 0usize;
                                            let mut failures = Vec::new();
                                            for (name, _) in skills {
                                                match this.model.library.remove_by_storage(name) {
                                                    Ok(()) => removed += 1,
                                                    Err(error) => failures.push(error.to_string()),
                                                }
                                            }
                                            this.delete_flow.confirmation = None;
                                            this.close_dialog(cx);
                                            this.refresh(cx);
                                            if failures.is_empty() {
                                                this.show_notice(success, cx);
                                            } else if this.uses_english() {
                                                this.show_notice(
                                                    format!(
                                                        "Deleted {removed}; {} failed",
                                                        failures.len()
                                                    ),
                                                    cx,
                                                );
                                            } else {
                                                this.show_notice(
                                                    format!(
                                                        "已删除 {} 个，{} 个失败",
                                                        removed,
                                                        failures.len()
                                                    ),
                                                    cx,
                                                );
                                            }
                                        }
                                        DeleteConfirmation::ProjectSkills { project, skills, .. } => {
                                            let report = project::remove_project_skills_at(&project,
                                                skills.iter().flat_map(|skill| &skill.installations).filter(|installation| {
                                                    this.delete_flow.selected
                                                        .contains(&installation.path)
                                                }),
                                            );
                                            let message = if report.failures.is_empty() {
                                                success.to_string()
                                            } else if report.removed > 0 {
                                                if this.uses_english() {
                                                    format!(
                                                        "Removed {}; {} could not be removed",
                                                        counted(report.removed, "location", "locations"),
                                                        report.failures.len()
                                                    )
                                                } else {
                                                    format!(
                                                        "已移除 {} 个位置，另有 {} 个位置未能移除",
                                                        report.removed,
                                                        report.failures.len()
                                                    )
                                                }
                                            } else if this.uses_english() {
                                                format!(
                                                    "{} could not be removed. Check access and try again",
                                                    counted(report.failures.len(), "location", "locations")
                                                )
                                            } else {
                                                format!(
                                                    "{} 个位置未能移除，请检查访问权限后重试",
                                                    report.failures.len()
                                                )
                                            };
                                            if report.removed > 0 {
                                                this.projects_view.context_estimates
                                                    .borrow_mut()
                                                    .remove(project);
                                                if !dirs::home_dir()
                                                    .as_ref()
                                                    .is_some_and(|home| home == project)
                                                {
                                                    this.model.library.config.touch_project(&project);
                                                    let _ = this.model.library.save();
                                                }
                                            }
                                            this.delete_flow.confirmation = None;
                                            this.delete_flow.selected.clear();
                                            this.projects_view.batch_project = None;
                                            this.projects_view.batch_selected.clear();
                                            this.projects_view.batch_filter = None;
                                            this.close_dialog(cx);
                                            this.show_notice(message, cx);
                                            this.refresh(cx);
                                        }
                                    }
                                }))
                            }),
                    ),
            )
    }
}
