use super::*;

fn selected_project_skills(
    rows: &[EffectiveSkillRow],
    selected_keys: &HashSet<String>,
    action: ProjectBatchAction,
) -> Vec<ProjectSkill> {
    rows.iter()
        .filter(|row| selected_keys.contains(&row.key))
        .map(|row| ProjectSkill {
            name: row.name.clone(),
            installations: row.batch_installations(action),
        })
        .filter(|skill| !skill.installations.is_empty())
        .collect()
}

impl KitterApp {
    fn project_skills_tabs_with_diagnostic(
        &self,
        skill_count: usize,
        plugin_skill_count: usize,
        incomplete: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        if !incomplete {
            return self.project_skills_tabs(skill_count, plugin_skill_count, cx);
        }
        let p = self.palette();
        let mut tabs = div()
            .h(px(43.))
            .px(px(28.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(16.))
            .border_b_1()
            .border_color(p.border);
        for (tab, label, count) in [
            (ProjectSkillsTab::Skills, self.tr("技能", "Skills"), skill_count),
            (ProjectSkillsTab::Plugins, self.tr("插件", "Plugins"), plugin_skill_count),
        ] {
            let selected = self.projects_view.project_skills_tab == tab;
            tabs = tabs.child(
                div()
                    .id(ElementId::Name(format!("project-skills-tab-{}",
                        if tab == ProjectSkillsTab::Skills { "skills" } else { "plugins" }).into()))
                    .h(px(43.))
                    .px(px(2.))
                    .border_b_1()
                    .border_color(if selected { p.accent } else { rgba(0x00000000) })
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if selected { p.text } else { p.muted })
                    .child(label)
                    .child(
                        div()
                            .font_family(MONO)
                            .text_size(px(10.))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(p.muted)
                            .child(if count == 0 {
                                self.tr("未知", "Unknown").to_string()
                            } else if self.uses_english() {
                                format!("{count} (partial)")
                            } else {
                                format!("{count}（部分）")
                            }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.projects_view.project_skills_tab = tab;
                        if tab == ProjectSkillsTab::Plugins {
                            this.projects_view.batch_project = None;
                            this.projects_view.batch_selected.clear();
                            this.projects_view.batch_filter = None;
                        }
                        cx.notify();
                    })),
            );
        }
        tabs
    }

    fn project_batch_action_label(&self, action: ProjectBatchAction) -> &'static str {
        match action {
            ProjectBatchAction::Remove => self.tr("卸载用户技能", "Uninstall user skills"),
            ProjectBatchAction::DisableProma => self.tr("停用内置预设", "Disable built-in presets"),
            ProjectBatchAction::EnableProma => self.tr("启用内置预设", "Enable built-in presets"),
        }
    }

    fn project_batch_unavailable_reason(&self, row: &EffectiveSkillRow) -> String {
        match self.projects_view.batch_action {
            ProjectBatchAction::Remove => {
                if row.direct_installations.iter().any(|item| item.bundled) {
                    self.tr("Proma 内置预设可停用，请切换到“停用内置预设”", "Switch to Disable built-in presets to deactivate them")
                } else if row.direct_installations.is_empty() {
                    self.tr("此项来自全局、父目录或系统；此项目没有可移除的安装", "Inherited or system skill; no removable installation in this project")
                } else {
                    self.tr("此安装受保护，不能从这里删除", "This installation is protected from removal")
                }
            }
            action => {
                let proma = row.direct_installations.iter().filter(|item| item.target == InstallTarget::Proma).collect::<Vec<_>>();
                if !cfg!(unix) {
                    self.tr("此平台尚未支持 Proma 技能启停", "Proma skill state changes are not supported on this platform")
                } else if proma.is_empty() {
                    self.tr("此项目没有该技能的 Proma 副本；全局和系统来源不受此操作影响", "No Proma copy in this project; inherited and system sources are unaffected")
                } else if proma.iter().any(|item| item.toggleable) {
                    if action == ProjectBatchAction::DisableProma {
                        self.tr("Proma 内置预设已停用；切换到“启用内置预设”可恢复", "Already inactive; switch to Enable built-in presets to restore")
                    } else {
                        self.tr("Proma 内置预设已启用；切换到“停用内置预设”可停用", "Already active; switch to Disable built-in presets to deactivate")
                    }
                } else {
                    self.tr("启停仅用于已核验的 Proma 内置预设；用户技能请安装或卸载", "Only verified Proma built-in presets can change state; install or uninstall user skills")
                }
            }
        }.to_string()
    }

    fn project_batch_action_control(&self, has_proma: bool, cx: &mut Context<Self>) -> Popover {
        let p = self.palette();
        let app = cx.entity().downgrade();
        let selected = self.projects_view.batch_action;
        let actions = [ProjectBatchAction::Remove, ProjectBatchAction::DisableProma, ProjectBatchAction::EnableProma]
            .into_iter().filter(|action| has_proma || *action == ProjectBatchAction::Remove)
            .map(|action| (action, self.project_batch_action_label(action).to_string())).collect::<Vec<_>>();
        Popover::new("project-batch-action").appearance(false).anchor(Anchor::TopRight)
            .trigger(self.dropdown_button("project-batch-action-trigger", self.project_batch_action_label(selected), 12.)
                .min_w(px(102.)).border_0().bg(rgba(0x00000000)).font_weight(FontWeight::NORMAL))
            .content(move |_, _, popover_cx| {
                let mut menu = div().w(px(180.)).p(px(4.)).rounded(px(RADIUS_MENU))
                    .border_1().border_color(p.border).bg(p.elevated).shadow_lg();
                for (action, label) in &actions {
                    let action = *action;
                    let app = app.clone();
                    menu = menu.child(div().id(ElementId::Name(format!("project-batch-action-{}", action as u8).into()))
                        .h(px(30.)).px(px(9.)).rounded(px(RADIUS_CONTROL)).flex().items_center().cursor_pointer()
                        .font_family(FONT_UI).text_size(px(13.)).text_color(p.text)
                        .bg(if action == selected { p.selected } else { rgba(0x00000000) })
                        .hover(move |row| row.bg(p.hover)).child(label.clone())
                        .on_click(popover_cx.listener(move |_, _, _, cx| {
                            let _ = app.update(cx, |this, cx| {
                                this.projects_view.batch_action = action;
                                this.projects_view.batch_selected.clear();
                                this.projects_view.batch_filter = None;
                                cx.notify();
                            });
                            cx.emit(DismissEvent);
                        })));
                }
                menu
            })
    }

    fn project_proma_state_control(&self, selected: Vec<ProjectSkill>, project: PathBuf, cx: &mut Context<Self>) -> Popover {
        let p = self.palette();
        let app = cx.entity().downgrade();
        let enabled = self.projects_view.batch_action == ProjectBatchAction::EnableProma;
        let action = self.tr(if enabled { "启用" } else { "停用" }, if enabled { "Enable" } else { "Disable" }).to_string();
        let has_selection = !selected.is_empty();
        let title = if self.uses_english() { format!("{action} {} built-in presets in Proma", selected.len()) }
            else { format!("在 Proma 中{action} {} 个内置预设", selected.len()) };
        let description = self.tr(
            if enabled { "启用以下工作区的 Proma 内置预设，将保留的预设文件移回启用目录。" }
            else { "停用以下工作区的 Proma 内置预设，保留文件，可随时重新启用。用户技能使用卸载管理。" },
            if enabled { "Enable these Proma built-in presets by restoring their retained files to skills." }
            else { "Disable these Proma built-in presets while retaining their files. Use uninstall for user skills." }
        ).to_string();
        let cancel = self.tr("取消", "Cancel").to_string();
        Popover::new("project-proma-state-confirmation").appearance(false).anchor(Anchor::TopRight)
            .trigger(self.toolbar_text_button("project-proma-state-trigger", action.clone())
                .min_w(px(42.)).disabled(!has_selection))
            .content(move |_, _, popover_cx| {
                let confirm_app = app.clone();
                let selected = selected.clone();
                let project = project.clone();
                let mut locations = div().id("project-proma-state-locations").max_h(px(240.)).overflow_y_scroll();
                for skill in &selected {
                    for installation in &skill.installations {
                        locations = locations.child(div().py(px(5.)).text_size(px(11.)).font_family(MONO)
                            .child(div().text_color(p.text).child(skill.name.clone()))
                            .child(div().text_color(p.muted).child(display_path(&installation.path))));
                    }
                }
                div().w(px(400.)).p(px(16.)).rounded(px(RADIUS_MENU)).border_1().border_color(p.border)
                    .bg(p.elevated).shadow_lg().flex().flex_col().gap(px(12.))
                    .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(title.clone()))
                    .child(div().text_size(px(12.)).text_color(p.secondary).child(description.clone()))
                    .child(locations)
                    .child(div().flex().justify_end().gap(px(8.))
                        .child(Button::new("cancel-proma-state").small().label(cancel.clone())
                            .on_click(popover_cx.listener(|_, _, _, cx| cx.emit(DismissEvent))))
                        .child(Button::new("confirm-proma-state").small().primary().label(action.clone())
                            .on_click(popover_cx.listener(move |_, _, _, cx| {
                                let _ = confirm_app.update(cx, |this, cx| this.apply_project_proma_state(&selected, &project, enabled, cx));
                                cx.emit(DismissEvent);
                            }))))
            })
    }

    fn skill_origin_label(&self, origin: SkillOrigin, global: bool) -> &'static str {
        match origin {
            SkillOrigin::Project => self.tr("项目安装", "Project install"),
            SkillOrigin::Parent => self.tr("父目录继承", "Inherited from parent"),
            SkillOrigin::User if global => self.tr("全局安装", "Global install"),
            SkillOrigin::User => self.tr("全局继承", "Inherited globally"),
            SkillOrigin::System => self.tr("系统提供", "System-provided"),
            SkillOrigin::Builtin => self.tr("内置", "Built-in"),
            SkillOrigin::Integration => self.tr("集成预设", "Integration preset"),
        }
    }

    fn project_skill_source_tabs(
        &self, project: &Path, groups: &[(SkillSourceGroup, usize)],
        selected: Option<SkillSourceGroup>, cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette();
        let mut tabs = div().id("project-skill-source-tabs")
            .h(px(44.)).min_w_0().flex_none().px(px(28.))
            .flex().items_center().gap(px(6.))
            .border_b_1().border_color(p.border).overflow_x_scroll();
        for &(group, count) in groups {
            let key = group.key();
            let active = selected == Some(group);
            let label = match group {
                SkillSourceGroup::Origin(origin) => self.skill_origin_label(origin, self.projects_view.global_project_view),
                SkillSourceGroup::Multiple => self.tr("多种来源", "Multiple sources"),
                SkillSourceGroup::Unknown => self.tr("来源未知", "Unknown source"),
            };
            let project = project.to_path_buf();
            tabs = tabs.child(gpui_base::Button::new(ElementId::Name(format!("project-skill-source-{key}").into()))
                .debug_selector(move || format!("project-skill-source-{key}").into())
                .accessibility_label(format!("{label} {count}{}", if active { self.tr(" · 已选", " · selected") } else { "" }))
                .h(px(28.)).px(px(10.)).rounded(px(7.)).flex_none()
                .flex().items_center().gap(px(6.)).cursor_pointer()
                .font_family(FONT_UI).text_size(px(11.)).font_weight(FontWeight::MEDIUM)
                .text_color(if active { p.text } else { p.secondary })
                .bg(if active { p.selected } else { rgba(0x00000000) })
                .hover(move |tab| tab.bg(p.hover))
                .child(label)
                .child(div().font_family(MONO).text_size(px(10.)).text_color(p.muted).child(count.to_string()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.projects_view.selected_skill_source = Some((project.clone(), group));
                    cx.notify();
                })));
        }
        tabs.into_any_element()
    }

    fn matches_project_skill_filter(&self, row: &EffectiveSkillRow) -> bool {
        let Some(filter) = self.projects_view.batch_filter.as_ref() else {
            return true;
        };
        self.model.skills.iter().any(|skill| {
            skill.record.name == row.name
                && row.direct_installations.iter().any(|installation| {
                    same_file(
                        &installation.path.join("SKILL.md"),
                        &skill.path.join("SKILL.md"),
                    )
                        || (installation.target == InstallTarget::Proma
                            && crate::proma::is_managed(&installation.path, &skill.path))
                })
                && match filter {
                    ProjectSkillFilter::Group(group) => {
                        skill.record.group_id.as_ref() == Some(group)
                    }
                    ProjectSkillFilter::Tag(tag) => self
                        .tags_flow
                        .skills
                        .matches_filter(skill_storage_name(skill), *tag),
                }
        })
    }

    fn project_skill_filter_control(&self, cx: &mut Context<Self>) -> Popover {
        let p = self.palette();
        let app = cx.entity().downgrade();
        let groups = self.model.library.groups();
        let tags = self
            .tags_flow
            .skills
            .tags()
            .iter()
            .map(|tag| {
                (
                    tag.id,
                    self.tags_flow
                        .skills
                        .path(tag.id)
                        .unwrap_or_else(|| tag.name.clone()),
                )
            })
            .collect::<Vec<_>>();
        let selected = self.projects_view.batch_filter.clone();
        let filter_tooltip = if selected.is_some() {
            self.tr("更改筛选", "Change filter")
        } else {
            self.tr("筛选", "Filter")
        }
        .to_string();
        let all_label = self.tr("全部技能", "All skills").to_string();
        let group_label = self.tr("分组", "Groups").to_string();
        let tag_label = self.tr("标签", "Tags").to_string();
        Popover::new("project-skill-batch-filter")
            .appearance(false)
            .anchor(Anchor::TopRight)
            .trigger(
                Button::new("project-skill-batch-filter-trigger")
                    .small()
                    .custom(
                        ButtonCustomVariant::new(cx)
                            .color(rgba(0x00000000).into())
                            .foreground(
                                if selected.is_some() {
                                    p.accent
                                } else {
                                    p.muted
                                }
                                .into(),
                            )
                            .hover(p.hover.into()),
                    )
                    .h(px(CONTROL_HEIGHT))
                    .w(px(CONTROL_HEIGHT))
                    .rounded(px(RADIUS_CONTROL))
                    .child(Self::icon(
                        "icons/hash.svg",
                        15.,
                        if selected.is_some() {
                            p.accent
                        } else {
                            p.muted
                        },
                    ))
                    .tooltip(filter_tooltip),
            )
            .content(move |_, _, popover_cx| {
                let mut menu = div()
                    .id("project-skill-filter-scroll")
                    .w(px(220.))
                    .max_h(px(360.))
                    .overflow_y_scroll()
                    .p(px(4.))
                    .rounded(px(RADIUS_MENU))
                    .border_1()
                    .border_color(p.border)
                    .bg(p.elevated)
                    .shadow_lg();
                let all_app = app.clone();
                menu = menu.child(
                    div()
                        .id("project-skill-filter-all")
                        .h(px(30.))
                        .px(px(8.))
                        .rounded(px(RADIUS_CONTROL))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .bg(if selected.is_none() {
                            p.selected
                        } else {
                            rgba(0x00000000)
                        })
                        .hover(move |row| row.bg(p.hover))
                        .child(all_label.clone())
                        .on_click(popover_cx.listener(move |_, _, _, cx| {
                            let _ = all_app.update(cx, |this, cx| {
                                this.projects_view.batch_filter = None;
                                cx.notify();
                            });
                            cx.emit(DismissEvent);
                        })),
                );
                if !groups.is_empty() {
                    menu = menu.child(
                        div()
                            .px(px(8.))
                            .pt(px(8.))
                            .pb(px(3.))
                            .text_size(px(11.))
                            .text_color(p.muted)
                            .child(group_label.clone()),
                    );
                }
                for group in &groups {
                    let filter = ProjectSkillFilter::Group(group.id.clone());
                    let group_app = app.clone();
                    let checked = selected.as_ref() == Some(&filter);
                    menu = menu.child(
                        div()
                            .id(ElementId::Name(
                                format!("project-skill-filter-group-{}", group.id).into(),
                            ))
                            .h(px(30.))
                            .px(px(8.))
                            .rounded(px(RADIUS_CONTROL))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .bg(if checked {
                                p.selected
                            } else {
                                rgba(0x00000000)
                            })
                            .hover(move |row| row.bg(p.hover))
                            .child(group.name.clone())
                            .on_click(popover_cx.listener(move |_, _, _, cx| {
                                let _ = group_app.update(cx, |this, cx| {
                                    this.projects_view.batch_filter = Some(filter.clone());
                                    cx.notify();
                                });
                                cx.emit(DismissEvent);
                            })),
                    );
                }
                if !tags.is_empty() {
                    menu = menu.child(
                        div()
                            .px(px(8.))
                            .pt(px(8.))
                            .pb(px(3.))
                            .text_size(px(11.))
                            .text_color(p.muted)
                            .child(tag_label.clone()),
                    );
                }
                for (id, name) in &tags {
                    let filter = ProjectSkillFilter::Tag(*id);
                    let tag_app = app.clone();
                    let checked = selected.as_ref() == Some(&filter);
                    menu = menu.child(
                        div()
                            .id(ElementId::Name(
                                format!("project-skill-filter-tag-{id:?}").into(),
                            ))
                            .h(px(30.))
                            .px(px(8.))
                            .rounded(px(RADIUS_CONTROL))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .bg(if checked {
                                p.selected
                            } else {
                                rgba(0x00000000)
                            })
                            .hover(move |row| row.bg(p.hover))
                            .child(name.clone())
                            .on_click(popover_cx.listener(move |_, _, _, cx| {
                                let _ = tag_app.update(cx, |this, cx| {
                                    this.projects_view.batch_filter = Some(filter.clone());
                                    cx.notify();
                                });
                                cx.emit(DismissEvent);
                            })),
                    );
                }
                menu
            })
    }

    fn project_batch_controls(
        &self,
        open_project: &Path,
        all_rows: &[EffectiveSkillRow],
        visible_rows: &[EffectiveSkillRow],
        cx: &mut Context<Self>,
    ) -> Div {
        let p = self.palette();
        let action = self.projects_view.batch_action;
        let selected = selected_project_skills(all_rows, &self.projects_view.batch_selected, action);
        let selected_count = selected.len();
        let selectable_keys = visible_rows
            .iter()
            .filter(|row| !row.batch_installations(action).is_empty())
            .map(|row| row.key.clone())
            .collect::<Vec<_>>();
        let all_selected = !selectable_keys.is_empty()
            && selectable_keys
                .iter()
                .all(|key| self.projects_view.batch_selected.contains(key));
        let visible_selected_count = selectable_keys
            .iter()
            .filter(|key| self.projects_view.batch_selected.contains(*key))
            .count();
        let partially_selected = visible_selected_count > 0 && !all_selected;
        let project = open_project.to_path_buf();
        let global_scope = dirs::home_dir()
            .as_ref()
            .is_some_and(|home| home == open_project);
        let select_all_label = if all_selected {
            self.tr("取消全选", "Deselect all")
        } else {
            self.tr("全选当前结果", "Select visible")
        }
        .to_string();
        let remove_label = if global_scope {
            self.tr("从用户级目录移除", "Remove from user-level locations")
        } else {
            self.tr("从此项目移除", "Remove from project")
        }
        .to_string();
        let select_first_label = self.tr("请先选择技能", "Select skills first").to_string();
        let exit_label = self.tr("退出选择", "Exit selection").to_string();
        let select_keys = selectable_keys.clone();
        let has_proma = all_rows.iter().any(|row| row.direct_installations.iter().any(|item| item.target == InstallTarget::Proma));
        let mut controls = div().h(px(CONTROL_HEIGHT)).font_family(FONT_UI).font_weight(FontWeight::NORMAL)
            .flex().items_center().gap(px(6.));
        if !select_keys.is_empty() {
            controls = controls.child(
                div()
                    .id("project-batch-select-visible")
                    .size(px(CONTROL_HEIGHT))
                    .rounded(px(RADIUS_CONTROL))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(move |button| button.bg(p.hover))
                    .child(
                        div()
                            .size(px(16.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(if all_selected || partially_selected {
                                p.text
                            } else {
                                p.border_strong
                            })
                            .bg(if all_selected || partially_selected {
                                p.text
                            } else {
                                p.surface
                            })
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(all_selected, |indicator| {
                                indicator.child(Self::icon("icons/check.svg", 12., p.on_accent))
                            })
                            .when(partially_selected, |indicator| {
                                indicator.child(
                                    div().w(px(8.)).h(px(1.5)).rounded(px(1.)).bg(p.on_accent),
                                )
                            }),
                    )
                    .tooltip(move |window, cx| {
                        Tooltip::new(select_all_label.clone()).build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        for key in &select_keys {
                            if all_selected {
                                this.projects_view.batch_selected.remove(key);
                            } else {
                                this.projects_view.batch_selected.insert(key.clone());
                            }
                        }
                        cx.notify();
                    })),
            );
        }
        let selected_label = if self.uses_english() { format!("{selected_count} selected") }
            else { format!("已选 {selected_count}") };
        controls = controls.child(div().h(px(CONTROL_HEIGHT)).min_w(px(42.))
            .text_size(px(12.)).text_color(p.muted).flex().items_center().child(selected_label))
            .child(self.project_batch_action_control(has_proma, cx));
        controls = controls.child(self.project_skill_filter_control(cx));
        if action == ProjectBatchAction::Remove {
            controls = controls.child(
                div()
                    .id("project-batch-remove")
                    .size(px(CONTROL_HEIGHT))
                    .rounded(px(RADIUS_CONTROL))
                    .bg(if selected_count > 0 {
                        p.danger_soft
                    } else {
                        rgba(0x00000000)
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor(if selected_count > 0 {
                        CursorStyle::PointingHand
                    } else {
                        CursorStyle::Arrow
                    })
                    .child(Self::icon(
                        "icons/trash.svg",
                        15.,
                        if selected_count > 0 {
                            p.danger
                        } else {
                            p.muted
                        },
                    ))
                    .tooltip(move |window, cx| {
                        Tooltip::new(if selected_count > 0 {
                            remove_label.clone()
                        } else {
                            select_first_label.clone()
                        })
                        .build(window, cx)
                    })
                    .when(selected_count > 0, |button| {
                        button
                            .hover(move |button| button.opacity(0.8))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    this.delete_flow.selected = selected
                                        .iter()
                                        .flat_map(|skill| {
                                            unique_installation_paths(&skill.installations)
                                        })
                                        .collect();
                                    this.delete_flow.confirmation =
                                        Some(DeleteConfirmation::ProjectSkills {
                                            project: project.clone(),
                                            skills: selected.clone(),
                                            batch: true,
                                        });
                                    this.open_delete_dialog(window, cx);
                                }),
                            )
                    }),
            );
        } else {
            controls = controls.child(self.project_proma_state_control(selected, project, cx));
        }
        {
            let keys = self.projects_view.batch_selected.iter().cloned().collect::<Vec<_>>();
            let project = open_project.to_path_buf();
            let explanation = self.cross_project_explanation();
            controls = controls.child(self.toolbar_text_button("project-cross-project", self.tr("跨项目管理", "Manage across projects"))
                .tooltip(move |window, cx| Tooltip::new(explanation.clone()).build(window, cx))
                .on_click(cx.listener(move |this, _, window, cx| {
                    let action = match action { ProjectBatchAction::Remove => crate::cross_project::Action::Remove,
                        ProjectBatchAction::EnableProma => crate::cross_project::Action::Enable,
                        ProjectBatchAction::DisableProma => crate::cross_project::Action::Disable };
                    this.open_cross_project(keys.clone(), Some(project.clone()), action, None, window, cx);
                })));
        }
        controls.child(
                div()
                    .id("project-batch-cancel")
                    .size(px(CONTROL_HEIGHT))
                    .rounded(px(RADIUS_CONTROL))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(move |button| button.bg(p.hover))
                    .child(Self::icon("icons/x.svg", 15., p.secondary))
                    .tooltip(move |window, cx| Tooltip::new(exit_label.clone()).build(window, cx))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.projects_view.batch_project = None;
                        this.projects_view.batch_selected.clear();
                        this.projects_view.batch_filter = None;
                        cx.notify();
                    })),
            )
    }

    pub(super) fn proma_installation_status(
        &self,
        name: &str,
        key: &str,
        installations: &[ProjectSkillInstallation],
        project: &Path,
        cx: &mut Context<Self>,
    ) -> Option<Popover> {
        let p = self.palette();
        let mut seen = HashSet::new();
        let mut copies = installations
            .iter()
            .filter(|copy| copy.target == InstallTarget::Proma)
            .filter(|copy| seen.insert(project::installation_key(&copy.path)))
            .collect::<Vec<_>>();
        if copies.is_empty() {
            return None;
        }
        let workspace_count = copies.iter()
            .filter_map(|copy| copy.path.parent().and_then(Path::parent))
            .collect::<HashSet<_>>()
            .len();
        copies.sort_by_key(|copy| match copy.status.as_deref() {
            Some("modified") => 0,
            Some("update_pending") => 1,
            _ => 2,
        });
        let preset_only = copies.iter().all(|copy| copy.bundled);
        let integration_only = copies.iter().all(|copy| copy.integration.is_some());
        let has_links = copies.iter().any(|copy| matches!(copy.status.as_deref(), Some("external_link" | "broken_link")));
        let state_label = |copy: &ProjectSkillInstallation| match copy.state.as_deref() {
            _ if copy.status.as_deref() == Some("broken_link") => self.tr("待清理", "Needs cleanup"),
            Some("active") if copy.bundled => self.tr("已启用", "Active"),
            Some("inactive") if copy.bundled => self.tr("已停用", "Inactive"),
            Some("active") => self.tr("已安装", "Installed"),
            Some("inactive") => self.tr("旧停用副本", "Legacy inactive copy"),
            _ => self.tr("状态未知", "Unknown state"),
        };
        let status_label = |copy: &ProjectSkillInstallation| match copy.status.as_deref() {
            Some("integration_variant") => format!("{} · {}", copy.integration.as_deref().unwrap_or_default(), self.tr("本机适配", "Locally adapted")),
            Some("preset_variant") => self.tr("预设副本 · 有差异", "Preset copy · differs").to_string(),
            _ if copy.bundled => self.tr("预设副本", "Bundled copy").to_string(),
            _ if copy.integration.is_some() => copy.integration.clone().unwrap_or_default(),
            Some("current") => self.tr("最新", "Current").to_string(),
            Some("update_pending") => self.tr("待同步", "Update pending").to_string(),
            Some("modified") => self.tr("副本已修改", "Modified").to_string(),
            Some("external_link") => self.tr("软链接安装", "Link installation").to_string(),
            Some("broken_link") => self.tr("失效链接", "Broken link").to_string(),
            Some("unmanaged") => self.tr("未托管", "Unmanaged").to_string(),
            _ => self.tr("待核验", "Unverified").to_string(),
        };
        let mut issues = Vec::new();
        for (status, label) in [
            ("preset_variant", self.tr("预设有差异", "preset differs")),
            ("modified", self.tr("副本已修改", "modified")),
            ("update_pending", self.tr("待同步", "pending update")),
        ] {
            let count = copies
                .iter()
                .filter(|copy| copy.status.as_deref() == Some(status))
                .count();
            if count > 0 {
                issues.push(if copies.len() == 1 { label.to_string() } else { format!("{label} {count}") });
            }
        }
        let unverified = copies
            .iter()
            .filter(|copy| {
                !copy.bundled && copy.integration.is_none() && !matches!(
                    copy.status.as_deref(),
                    Some("current" | "modified" | "update_pending" | "external_link" | "broken_link" | "unmanaged")
                )
            })
            .count();
        if unverified > 0 {
            issues.push(format!("{} {unverified}", self.tr("待核验", "unverified")));
        }
        let has_issues = !issues.is_empty();
        let inactive = copies.iter().all(|copy| copy.state.as_deref() == Some("inactive"));
        let active_count = copies.iter().filter(|copy| copy.state.as_deref() == Some("active")).count();
        let activation = if copies.len() == 1 {
            state_label(copies[0]).to_string()
        } else if inactive {
            if preset_only { self.tr("已停用", "Inactive") } else { self.tr("旧停用副本", "Legacy inactive copies") }.to_string()
        } else if active_count == copies.len() {
            if preset_only { self.tr("已启用", "Active") } else { self.tr("已安装", "Installed") }.to_string()
        } else if active_count > 0 {
            format!("{} {active_count}/{}", if preset_only { self.tr("部分启用", "Partly active") } else { self.tr("部分位于启用目录", "Partly in active directory") }, copies.len())
        } else {
            self.tr("状态未知", "Unknown state").to_string()
        };
        let label = if has_issues {
            format!("Proma {activation} · {}", issues.join(" · "))
        } else if copies.len() == 1 {
            format!("Proma {activation} · {}", status_label(copies[0]))
        } else if workspace_count > 1 {
            if self.uses_english() { format!("Proma {activation} · {workspace_count} workspaces") }
            else { format!("Proma {activation} · {workspace_count} 个工作区") }
        } else {
            format!("Proma {activation} · {}", copies.len())
        };
        let details = copies.iter().map(|copy| {
            let reason = match copy.status.as_deref() {
                Some("integration_variant") => self.tr("此集成副本包含本机适配，现有内容已保留并由集成维护；不参与 Pleat 自动同步，不在此启停或卸载。服务是否可用请在 Proma 中确认。", "This integration copy has local adaptations. Its contents are preserved and maintained by the integration; excluded from Pleat auto-sync, state changes and uninstall here. Check service availability in Proma."),
                Some("preset_variant") => self.tr("匹配本机 Proma 预设模板，但与当前应用自带内容不同。已归为内置预设并保留本机内容；启停仍受保护。", "Matches a configured Proma preset template but differs from the current app. Shown as a built-in preset while preserving local content; state changes remain protected."),
                _ if copy.bundled => self.tr("Proma 内置预设，可在批量管理中启用或停用。", "Proma preset; enable or disable it in batch management."),
                _ if copy.integration.is_some() => self.tr("此副本由集成维护，不参与本地技能自动同步。", "Managed by an integration; excluded from local skill auto-sync."),
                Some("modified") => self.tr("副本已自行修改，现有内容已保留；自动同步不会覆盖。", "This copy has local changes. Its contents are preserved and auto-sync will not overwrite them."),
                Some("update_pending") => self.tr("副本与来源不同。已登记的本地技能会在内容稳定后自动同步；同步遇到冲突时保留现有副本。", "This copy differs from its source. Registered local skills sync after edits settle; conflicts preserve the existing copy."),
                Some("external_link") => self.tr("这是旧软链接安装。可以仅移除链接，保留源文件；来源已在 Pleat 收录时可转换为工作区副本。", "This is a legacy link. Remove only the link, or convert it to a workspace copy when its source is registered in Pleat."),
                Some("broken_link") => self.tr("链接目标已不存在，可以清理工作区里的链接入口。", "The link target is missing. Remove the broken link from this workspace."),
                Some("unmanaged") => self.tr("此副本未由 Pleat 托管，不参与本地技能自动同步。", "This copy is not managed by Pleat; excluded from local skill auto-sync."),
                Some("current") if copy.state.as_deref() == Some("inactive") => self.tr("这是以前保留在停用目录的用户副本。可卸载，或从 Pleat 重新安装；用户技能不再提供启停。", "This legacy user copy remains in the inactive directory. Uninstall it or reinstall it from Pleat; user skills no longer have state toggles."),
                Some("current") => self.tr("副本与来源一致。", "This copy matches its source."),
                _ => self.tr("尚未核验副本状态，现有内容已保留。", "Copy status is unverified; its existing contents are preserved."),
            };
            (copy.path.clone(), format!("{} · {}", state_label(copy), status_label(copy)), reason.to_string(), matches!(copy.status.as_deref(), Some("modified" | "update_pending" | "preset_variant")))
        }).collect::<Vec<_>>();
        let title = format!("Proma · {name}");
        let multiple = workspace_count > 1;
        let scope = if copies.len() == 1 {
            self.tr("当前工作区的技能副本", "Skill copy in this workspace").to_string()
        } else if !multiple {
            if self.uses_english() { format!("{} copies in this workspace", copies.len()) }
            else { format!("当前工作区的 {} 份同名副本", copies.len()) }
        } else if self.uses_english() {
            format!("Copies in {workspace_count} Proma workspaces linked to this project")
        } else {
            format!("此项目关联的 {workspace_count} 个 Proma 工作区")
        };
        let separate_states = self.tr("安装、卸载和预设启停分别作用于所选工作区。", "Installations, uninstalls and preset state changes apply to selected workspaces.").to_string();
        let workspace_label = self.tr("工作区", "Workspace").to_string();
        let close = self.tr("关闭详情", "Close details").to_string();
        let app = cx.entity().downgrade();
        let skill_keys = copies.iter().map(|copy| effective_view::installation_skill_key(copy)).collect::<Vec<_>>();
        let conversion_paths = copies.iter().filter(|copy| matches!(copy.status.as_deref(), Some("external_link" | "broken_link")))
            .map(|copy| copy.path.clone()).collect::<Vec<_>>();
        let project = project.to_path_buf();
        let manage = if preset_only { self.tr("管理预设启停", "Manage preset state") } else { self.tr("管理安装与卸载", "Manage installations") }.to_string();
        let manage_action = if preset_only { if inactive { crate::cross_project::Action::Enable } else { crate::cross_project::Action::Disable } } else { crate::cross_project::Action::Remove };
        let convert = self.tr("转换为副本", "Convert to copies").to_string();
        Some(Popover::new(ElementId::Name(format!("proma-status-{key}").into()))
            .appearance(false).anchor(Anchor::TopLeft)
            .trigger(gpui_base::Button::new(ElementId::Name(format!("proma-status-trigger-{key}").into()))
                .accessibility_label(format!("{name} · {label} · {}", self.tr("查看详情", "View details")))
                .h(px(26.)).ml(px(8.)).px(px(8.)).rounded(px(7.5)).flex().items_center().gap(px(5.))
                .font_family(FONT_UI).font_weight(FontWeight::MEDIUM).text_size(px(12.))
                .text_color(if has_issues { p.warning } else { p.secondary })
                .bg(if has_issues || inactive { p.raised } else { rgba(0x00000000) })
                .hover(move |button| button.bg(p.hover))
                .styles(|styles| styles.selected(|style| style.bg(p.selected)))
                .when(inactive, |button| button.child(Self::icon("icons/pause.svg", 12., p.secondary)))
                .child(label)
                .child(Self::icon("icons/chevron-down.svg", 10., p.muted)))
            .content(move |_, _, popover_cx| {
                let workspaces = crate::proma::home().and_then(|root| crate::proma::workspaces(&root).ok()).unwrap_or_default();
                let mut list = div().id("proma-status-workspaces").max_h(px(300.)).overflow_y_scroll();
                for (path, status, reason, warning) in &details {
                    let workspace_path = path.parent().and_then(Path::parent);
                    let workspace = workspaces.iter().find(|workspace| Some(workspace.path.as_path()) == workspace_path)
                        .map(|workspace| workspace.name.clone())
                        .unwrap_or_else(|| workspace_path.and_then(Path::file_name).unwrap_or_default().to_string_lossy().into_owned());
                    let directory = path.parent().and_then(Path::file_name).unwrap_or_default().to_string_lossy();
                    list = list.child(div().py(px(10.)).border_b_1().border_color(p.border)
                        .child(div().flex().items_center().gap(px(8.))
                            .child(div().min_w_0().flex_1().truncate().text_size(px(12.)).text_color(p.text).child(format!("{workspace_label} · {workspace}")))
                            .child(div().text_size(px(11.)).text_color(if *warning { p.warning } else { p.muted }).child(status.clone())))
                        .child(div().mt(px(5.)).text_size(px(12.)).line_height(relative(1.5)).text_color(p.secondary).child(reason.clone()))
                        .child(div().mt(px(5.)).font_family(MONO).text_size(px(11.)).text_color(p.muted).child(format!("{workspace}/{directory}/{name}", workspace = workspace_path.and_then(Path::file_name).unwrap_or_default().to_string_lossy(), name = path.file_name().unwrap_or_default().to_string_lossy()))));
                }
                div().w(px(420.)).p(px(14.)).rounded(px(RADIUS_MENU)).border_1().border_color(p.border)
                    .bg(p.elevated).shadow_lg().flex().flex_col().gap(px(8.))
                    .child(div().flex().items_center()
                        .child(div().min_w_0().flex_1().truncate().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(title.clone()))
                        .child(gpui_base::Button::new("close-proma-status").accessibility_label(close.clone())
                            .size(px(24.)).rounded(px(RADIUS_CONTROL)).flex().items_center().justify_center()
                            .hover(move |button| button.bg(p.hover)).child(Self::icon("icons/x.svg", 13., p.muted))
                            .on_click(popover_cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))))
                    .child(div().text_size(px(11.)).text_color(p.muted).child(scope.clone()))
                    .when(multiple, |panel| panel.child(div().text_size(px(11.)).text_color(p.secondary).child(separate_states.clone())))
                    .child(list)
                    .child({
                        let mut controls = div().flex().items_center().gap(px(5.));
                        if let Some(entity) = app.upgrade().filter(|_| !integration_only) {
                            controls = controls.child(entity.read(popover_cx).toolbar_text_button("proma-status-manage-workspaces", manage.clone())
                                .on_click(popover_cx.listener({
                                    let app = app.clone(); let keys = skill_keys.clone(); let project = project.clone();
                                    move |_, _, window, cx| {
                                        let _ = app.update(cx, |this, cx| this.open_cross_project(keys.clone(), Some(project.clone()), manage_action, Some("proma".into()), window, cx));
                                        cx.emit(DismissEvent);
                                    }
                                })));
                            if has_links {
                                controls = controls.child(entity.read(popover_cx).toolbar_text_button("proma-status-convert-links", convert.clone())
                                    .on_click(popover_cx.listener({
                                        let app = app.clone(); let keys = skill_keys.clone(); let project = project.clone(); let paths = conversion_paths.clone();
                                        move |_, _, window, cx| {
                                            let _ = app.update(cx, |this, cx| this.open_proma_conversion(keys.clone(), project.clone(), paths.clone(), window, cx));
                                            cx.emit(DismissEvent);
                                        }
                                    })));
                            }
                        }
                        controls
                    })
            }))
    }

    fn project_skill_row(
        &self,
        effective: &EffectiveSkillRow,
        open_project: &Path,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette();
        if effective.is_metadata_only() {
            let hint = effective.native_hint.clone().unwrap_or_default();
            let detail = format!("{}\n{}", effective.description, hint);
            return div().id(ElementId::Name(effective.key.clone().into()))
                .w_full().min_h(px(84.)).px(px(32.)).py(px(10.))
                .border_b_1().border_color(p.border).flex().flex_col().gap(px(3.))
                .child(div().flex().items_center().gap(px(10.))
                    .child(div().font_family(MONO).text_size(px(13.)).child(effective.name.clone()))
                    .child(div().text_size(px(11.)).text_color(p.muted)
                        .child(format!("{} · {}", self.tr("仅原生元数据，状态未知", "Native metadata only, unknown state"),
                            effective.agents.iter().map(|agent| agent.label()).collect::<Vec<_>>().join(" / ")))))
                .child(div().text_size(px(12.)).text_color(p.secondary).truncate().child(effective.description.clone()))
                .child(div().text_size(px(11.)).text_color(p.muted).truncate().child(hint))
                .tooltip(move |window, cx| {
                    let detail = detail.clone();
                    Tooltip::element(move |_, _| div().w(px(420.)).max_w(px(420.))
                        .whitespace_normal().text_size(px(12.)).child(detail.clone())).build(window, cx)
                }).into_any_element();
        }
        let inactive = effective.is_inactive();
        let managed = effective
            .direct_installations
            .iter()
            .any(|installation| installation.managed);
        let mut location_labels = effective
            .locations
            .iter()
            .map(|location| display_effective_root(location, open_project))
            .collect::<Vec<_>>();
        if effective.built_in {
            location_labels.push(self.tr("内置", "Built-in").to_string());
        }
        let locations = location_labels.join(" · ");
        let proma_status = self.proma_installation_status(&effective.name, &effective.key, &effective.direct_installations, open_project, cx);
        let key = effective.key.clone();
        let description = effective.description.clone();
        let installed_for_delete = ProjectSkill {
            name: effective.name.clone(),
            installations: effective.removable_installations(),
        };
        let can_delete = !installed_for_delete.installations.is_empty();
        let batch_mode = self.projects_view.batch_project.as_deref() == Some(open_project);
        let can_select = !effective.batch_installations(self.projects_view.batch_action).is_empty();
        let batch_checked = self.projects_view.batch_selected.contains(&key);
        let row_selector = format!("project-skill-{key}");
        let mut row = div()
            .id(ElementId::Name(row_selector.clone().into()))
            .debug_selector(move || row_selector)
            .w_full()
            .h(px(60.))
            .relative()
            .px(px(32.))
            .border_b_1()
            .border_color(p.border)
            .flex()
            .items_center()
            .when(inactive, |row| row.bg(p.raised))
            .hover(move |row| row.bg(p.hover))
            .when(batch_mode && can_select, |row| {
                row.child(
                    div()
                        .absolute()
                        .left(px(8.))
                        .top(px(0.))
                        .bottom(px(0.))
                        .flex()
                        .items_center()
                        .child(
                            Checkbox::new(ElementId::Name(
                                format!("batch-project-skill-{key}").into(),
                            ))
                            .tab_stop(false)
                            .checked(batch_checked),
                        ),
                )
            })
            .when(batch_mode && !can_select, |row| {
                let reason = self.project_batch_unavailable_reason(effective);
                row.child(div().id(ElementId::Name(format!("batch-project-unavailable-{key}").into()))
                    .absolute().left(px(8.)).top(px(0.)).bottom(px(0.))
                    .flex().items_center().justify_center().w(px(16.))
                    .child(div().size(px(16.)).rounded(px(4.)).border_1().border_color(p.border)
                        .flex().items_center().justify_center().text_size(px(11.)).text_color(p.muted).child("−"))
                    .tooltip(move |window, cx| Tooltip::new(reason.clone()).build(window, cx)))
            })
            .child(div().size(px(15.)).flex_none().child(Self::icon(
                "icons/package.svg",
                15.,
                if inactive { p.muted } else { p.secondary },
            )))
            .child(
                div()
                    .ml(px(10.))
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .id(ElementId::Name(
                                        format!("project-skill-name-{key}").into(),
                                    ))
                                    .min_w_0()
                                    .truncate()
                                    .font_family(MONO)
                                    .text_size(px(13.))
                                    .text_color(if inactive { p.muted } else { p.text })
                                    .child(effective.name.clone())
                                    .when(!description.is_empty(), |name| {
                                        let tooltip_description = description.clone();
                                        name.tooltip(move |window, cx| {
                                            let description = tooltip_description.clone();
                                            Tooltip::element(move |_, _| {
                                                div()
                                                    .w(px(320.))
                                                    .max_w(px(320.))
                                                    .font_family(MONO)
                                                    .whitespace_normal()
                                                    .line_height(relative(1.45))
                                                    .text_size(px(12.))
                                                    .child(description.clone())
                                            })
                                            .build(window, cx)
                                        })
                                    }),
                            )
                            .when(effective.duplicate_name, |title| {
                                title.child(div().ml(px(8.)).text_size(px(11.)).text_color(p.muted)
                                    .child(format!("{} · {}", self.tr("同名", "Same name"),
                                        effective.agents.iter().map(|agent| agent.label()).collect::<Vec<_>>().join(" / "))))
                            })
                            .when(effective.manual_only, |title| {
                                title.child(self.manual_skill_badge().ml(px(10.)))
                            })
                            .when(!effective.shadowed_agents.is_empty(), |title| {
                                title.child(div().ml(px(8.)).text_size(px(11.)).text_color(p.muted)
                                    .child(format!("{}: {}", self.tr("被同名来源覆盖", "Shadowed"),
                                        effective.shadowed_agents.iter().map(|agent| agent.label()).collect::<Vec<_>>().join(" / "))))
                            })
                            .when(!effective.disabled_agents.is_empty(), |title| {
                                title.child(div().ml(px(8.)).text_size(px(11.)).text_color(p.muted)
                                    .child(format!("{}: {}", self.tr("已禁用", "Disabled"),
                                        effective.disabled_agents.iter().map(|agent| agent.label()).collect::<Vec<_>>().join(" / "))))
                            })
                            .when(!effective.unloaded_agents.is_empty(), |title| {
                                title.child(div().ml(px(8.)).text_size(px(11.)).text_color(p.muted)
                                    .child(format!("{}: {}", self.tr("已安装，宿主未加载", "Installed, not loaded"),
                                        effective.unloaded_agents.iter().map(|agent| agent.label()).collect::<Vec<_>>().join(" / "))))
                            })
                            .when(!effective.unverified_agents.is_empty(), |title| {
                                title.child(div().ml(px(8.)).text_size(px(11.)).text_color(p.muted)
                                    .child(format!("{}: {}", self.tr("状态未知", "Unknown"),
                                        effective.unverified_agents.iter().map(|agent| agent.label()).collect::<Vec<_>>().join(" / "))))
                            })
                            .when(managed, |title| {
                                title.child(self.managed_skill_badge().ml(px(8.)))
                            })
                            .when_some(proma_status, |title, status| title.child(status)),
                    )
                    .child(
                        div()
                            .mt(px(3.))
                            .min_w_0()
                            .truncate()
                            .font_family(MONO)
                            .text_size(px(12.))
                            .text_color(p.muted)
                            .child(locations),
                    ),
            )
            .child(
                self.effective_agent_badges(format!("project-{key}"), &effective.agents, effective.proma_is_inactive(), cx)
                    .ml(px(12.)),
            );
        if !batch_mode && let Some(action) = &effective.grok_manual {
            match action {
                Ok(action) => {
                    let action = action.clone();
                    let project = open_project.to_path_buf();
                    let tooltip = format!("{}\n{}\n{}", self.tr(
                        "在以下目录启动 Grok 后粘贴；引用同源本机适配（如存在）",
                        "Start Grok in the directory below, then paste; references the same source's local adaptation when present",
                    ), project.display(), action.invocation.text);
                    row = row.child(self.toolbar_text_button(
                        ElementId::Name(format!("grok-manual-invocation-{key}").into()),
                        self.tr("复制 Grok 调用", "Copy Grok invocation"),
                    ).ml(px(10.)).flex_shrink_0()
                        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let current = crate::agents::grok::manual::invocation(
                                &project, &action.skill, action.installed_alias.as_deref(),
                            );
                            match current {
                                Ok(Some(current)) if current.source_identity == action.invocation.source_identity
                                    && current.source_revision == action.invocation.source_revision
                                    && current.context_identity == action.invocation.context_identity
                                    && current.includes_adaptation == action.invocation.includes_adaptation => {
                                    cx.write_to_clipboard(ClipboardItem::new_string(current.text));
                                    let message = this.tr("已复制 Grok 调用；工作目录见按钮提示", "Copied Grok invocation; see the button tooltip for the working directory").to_string();
                                    this.show_notice(message, cx);
                                }
                                _ => {
                                    let message = this.tr("项目、技能源或适配入口已变化，请刷新后重试", "The project, source or adaptation entry changed; refresh and retry").to_string();
                                    this.show_notice(message, cx);
                                }
                            }
                        })));
                }
                Err(reason) => {
                    let reason = reason.clone();
                    row = row.child(div().id(ElementId::Name(format!("grok-manual-unavailable-{key}").into()))
                        .ml(px(10.)).text_size(px(11.)).text_color(p.muted)
                        .child(self.tr("Grok 调用暂不可用", "Grok invocation unavailable"))
                        .tooltip(move |window, cx| Tooltip::new(reason.clone()).build(window, cx)));
                }
            }
        }
        if batch_mode && can_select {
            let selected_key = key.clone();
            row = row
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.projects_view.batch_selected.remove(&selected_key) {
                        this.projects_view
                            .batch_selected
                            .insert(selected_key.clone());
                    }
                    cx.notify();
                }));
        } else if !batch_mode && can_delete {
            let project_for_delete = open_project.to_path_buf();
            row = row.child(
                self.danger_icon_button(
                    ElementId::Name(format!("remove-project-{key}").into()),
                    "icons/trash.svg",
                    cx,
                )
                .ml(px(10.))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        this.delete_flow.selected =
                            unique_installation_paths(&installed_for_delete.installations)
                                .into_iter()
                                .collect();
                        this.delete_flow.confirmation = Some(DeleteConfirmation::ProjectSkills {
                            project: project_for_delete.clone(),
                            skills: vec![installed_for_delete.clone()],
                            batch: false,
                        });
                        this.open_delete_dialog(window, cx);
                    }),
                ),
            );
        }
        row.into_any_element()
    }

    fn project_agent_filter_control(
        &self,
        entries: &[crate::project_discovery::ProjectEntry],
        matching: &[crate::project_discovery::ProjectEntry],
        cx: &mut Context<Self>,
    ) -> Popover {
        let p = self.palette();
        let dark = self.uses_dark_theme();
        let selected = &self.projects_view.project_agent_filter;
        let all_label = self.tr("全部 Agent", "All agents").to_string();
        let unmarked_label = self.tr("未标注 Agent", "Unmarked agent").to_string();
        let not_installed_label = self.tr("未检测到", "Not detected").to_string();
        let selected_count = selected.agents.len() + usize::from(selected.unmarked);
        let label = if selected.is_all() {
            all_label.clone()
        } else if selected_count == 1 {
            selected
                .agents
                .first()
                .map(|agent| agent.label().to_string())
                .unwrap_or_else(|| unmarked_label.clone())
        } else if self.uses_english() {
            format!("{selected_count} selected")
        } else {
            format!("已选 {selected_count} 项")
        };
        let mut options = vec![(
            ProjectAgentFilterOption::All,
            all_label,
            matching.len(),
            None,
            true,
        )];
        let known_projects = self.all_project_entries();
        let mut agents = crate::agents::AGENT_ICON_ORDER.to_vec();
        agents.sort_by_key(|info| crate::agents::display_sort_key(info.name,
            AgentKind::from_id(info.id).is_some_and(|kind| ProjectAgentFilter::agent_available(kind, &self.model.installed_agents, &known_projects))));
        for info in agents {
            let Some(kind) = AgentKind::from_id(info.id) else {
                continue;
            };
            options.push((
                ProjectAgentFilterOption::Agent(kind),
                info.name.to_string(),
                matching
                    .iter()
                    .filter(|entry| entry.agents.contains(&kind))
                    .count(),
                Some(info.icon_path),
                ProjectAgentFilter::agent_available(kind, &self.model.installed_agents, &known_projects),
            ));
        }
        if entries.iter().any(|entry| entry.agents.is_empty()) || selected.unmarked {
            options.push((
                ProjectAgentFilterOption::Unmarked,
                unmarked_label,
                matching
                    .iter()
                    .filter(|entry| entry.agents.is_empty())
                    .count(),
                None,
                true,
            ));
        }
        let app = cx.entity().downgrade();
        Popover::new("project-agent-filter-menu")
            .appearance(false)
            .anchor(Anchor::TopRight)
            .trigger(
                self.dropdown_button("project-agent-filter-trigger", label, 12.)
                    .h(px(26.))
                    .max_w(px(138.))
                    .border_0()
                    .bg(rgba(0x00000000))
                    .text_color(if selected.is_all() { p.muted } else { p.accent }),
            )
            .content(move |_, _, popover_cx| {
                let selected = app
                    .upgrade()
                    .map(|app| {
                        app.read(popover_cx)
                            .projects_view
                            .project_agent_filter
                            .clone()
                    })
                    .unwrap_or_default();
                let mut menu = div()
                    .id("project-agent-filter-options")
                    .w(px(224.))
                    .max_h(px(316.))
                    .p(px(4.))
                    .rounded(px(RADIUS_MENU))
                    .border_1()
                    .border_color(p.border)
                    .bg(p.elevated)
                    .shadow_lg()
                    .overflow_y_scroll();
                for (index, (filter, label, count, icon, available)) in options.iter().enumerate() {
                    let filter = *filter;
                    let checked = selected.selected(filter);
                    let available = *available || checked;
                    let status = if available { count.to_string() } else { not_installed_label.clone() };
                    let app = app.clone();
                    let popover = popover_cx.entity();
                    menu = menu.child(
                        gpui_base::Button::new(ElementId::Name(
                            format!("project-agent-filter-option-{index}").into(),
                        ))
                        .accessibility_label(format!("{label} · {status}"))
                        .h(px(30.))
                        .w_full()
                        .px(px(8.))
                        .rounded(px(RADIUS_CONTROL))
                        .flex()
                        .items_center()
                        .gap(px(7.))
                        .font_family(FONT_UI)
                        .text_size(px(13.))
                        .text_color(if available { p.text } else { p.muted })
                        .bg(if checked {
                            p.selected
                        } else {
                            rgba(0x00000000)
                        })
                        .when(available, |row| row.hover(move |row| row.bg(p.hover)))
                        .child(div().w(px(14.)).when(checked, |slot| {
                            slot.child(Self::icon("icons/check.svg", 12., p.secondary))
                        }))
                        .child(
                            div()
                                .w(px(18.))
                                .flex_none()
                                .flex()
                                .justify_center()
                                .when_some(*icon, |slot, icon| {
                                    let size = Self::brand_icon_size(icon, 16.);
                                    if available {
                                        slot.child(Self::brand_icon(icon, size, p.text, dark))
                                    } else if icon.ends_with(".png") {
                                        slot.child(div().opacity(0.4).child(Self::brand_icon(icon, size, p.muted, dark)))
                                    } else {
                                        slot.child(Self::icon(icon, size, p.muted))
                                    }
                                }),
                        )
                        .child(div().min_w_0().flex_1().truncate().child(label.clone()))
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(p.muted)
                                .child(status),
                        )
                        .on_click(move |_, _, cx| {
                            if !available { return; }
                            let _ = app.update(cx, |this, cx| {
                                this.projects_view.project_agent_filter.toggle(filter);
                                cx.notify();
                            });
                            popover.update(cx, |_, cx| cx.notify());
                        }),
                    );
                }
                menu
            })
    }

    fn project_list_label(&self, view: ProjectListView) -> &'static str {
        match view {
            ProjectListView::Active => self.tr("活跃项目", "Active projects"),
            ProjectListView::Archived => self.tr("已归档／隐藏", "Archived / hidden"),
            ProjectListView::All => self.tr("全部项目", "All projects"),
        }
    }

    fn project_list_control(&self, entries: &[crate::project_discovery::ProjectEntry], count: usize, cx: &mut Context<Self>) -> Popover {
        let p = self.palette();
        let selected = self.projects_view.list_view;
        let app = cx.entity().downgrade();
        let options = [ProjectListView::Active, ProjectListView::Archived, ProjectListView::All]
            .map(|view| (view, self.project_list_label(view).to_string(), entries.iter().filter(|entry| view.matches(entry)).count()));
        let help = [
            self.tr("项目列表显示", "Project visibility"),
            self.tr("活跃项目：默认显示；任一关联 Agent 活跃就保留。", "Active: shown by default while any associated Agent is active."),
            self.tr("已归档／隐藏：关联 Agent 均已归档，或你手动隐藏。", "Archived / hidden: every associated Agent explicitly archived it, or you hid it in Pleat."),
            self.tr("全部项目：同时查看以上两类。", "All: show both groups together."),
            self.tr("右键可隐藏；在归档／隐藏列表中可恢复到活跃项目。", "Right-click to hide; restore from the Archived / hidden list."),
            self.tr("目录、技能和安装记录均保留。状态不明或读取失败不会自动归档；Codex 项目可手动隐藏。", "Folders, skills and installation records are retained. Unknown states or scan failures never trigger archival; Codex projects can be hidden manually."),
        ];
        Popover::new("project-list-view-menu").appearance(false).anchor(Anchor::TopLeft)
            .trigger(self.dropdown_button("project-list-view-trigger", format!("{}  {count}", self.project_list_label(selected)), 12.)
                .h(px(26.)).max_w(px(145.)).border_0().bg(rgba(0x00000000))
                .tooltip(move |window, cx| Tooltip::element(move |_, _| {
                    div().w(px(320.)).py(px(6.)).flex().flex_col().gap(px(7.)).text_size(px(12.)).line_height(relative(1.5))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(help[0]))
                        .children(help[1..].iter().map(|text| div().child(*text)))
                }).build(window, cx)))
            .content(move |_, _, popover_cx| {
                let mut menu = div().w(px(210.)).p(px(4.)).rounded(px(RADIUS_MENU))
                    .border_1().border_color(p.border).bg(p.elevated).shadow_lg();
                for (view, label, count) in &options {
                    let view = *view;
                    let app = app.clone();
                    menu = menu.child(div().id(ElementId::Name(format!("project-list-view-{}", view as u8).into()))
                        .h(px(32.)).px(px(9.)).rounded(px(RADIUS_CONTROL)).flex().items_center().gap(px(8.))
                        .cursor_pointer().text_size(px(13.)).text_color(p.text)
                        .bg(if view == selected { p.selected } else { rgba(0x00000000) })
                        .hover(move |row| row.bg(p.hover))
                        .child(div().w(px(14.)).when(view == selected, |icon| icon.child(Self::icon("icons/check.svg", 14., p.text))))
                        .child(div().flex_1().child(label.clone()))
                        .child(div().text_color(p.muted).child(count.to_string()))
                        .on_click(popover_cx.listener(move |_, _, _, cx| {
                            let _ = app.update(cx, |this, cx| {
                                this.projects_view.list_view = view;
                                this.projects_view.batch_project = None;
                                this.projects_view.batch_selected.clear();
                                this.projects_view.batch_filter = None;
                                cx.notify();
                            });
                            cx.emit(DismissEvent);
                        })));
                }
                menu
            })
    }

    pub(super) fn projects_page(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        #[cfg(debug_assertions)]
        let render_started = Instant::now();
        let p = self.palette();
        let query = self
            .projects_view
            .project_search
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let global_root = dirs::home_dir();
        let global_skills = global_root
            .as_ref()
            .and_then(|path| self.project_snapshot(path, cx));
        let global_skill_count = global_skills.as_ref().map(Vec::len);
        let all_projects = self.all_project_entries();
        let view_projects = all_projects.iter().filter(|entry| self.projects_view.list_view.matches(entry)).cloned().collect::<Vec<_>>();
        let total_project_count = view_projects.len();
        let matching_projects = view_projects
            .iter()
            .filter(|entry| {
                let path = &entry.path;
                let matches_query = query.is_empty()
                    || entry.name.to_lowercase().contains(&query)
                    || path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_lowercase()
                        .contains(&query)
                    || path.display().to_string().to_lowercase().contains(&query);
                let matches_tag = self.tags_flow.selected_project_filter.is_none_or(|tag| {
                    self.tags_flow
                        .projects
                        .matches_filter(&project_tag_key(path), tag)
                });
                matches_query && matches_tag
            })
            .cloned()
            .collect::<Vec<_>>();
        let agent_filter_control =
            self.project_agent_filter_control(&view_projects, &matching_projects, cx);
        let projects = matching_projects
            .into_iter()
            .filter(|entry| self.projects_view.project_agent_filter.matches(entry))
            .collect::<Vec<_>>();
        let visible_project_count = projects.len();
        let list_control = self.project_list_control(&all_projects, visible_project_count, cx);
        let project_context_app = cx.entity().downgrade();
        let mut recent_rows = div()
            .id("projects-scroll")
            .flex_1()
            .min_h_0()
            .px(px(8.))
            .overflow_y_scroll();
        if projects.is_empty() {
            recent_rows = recent_rows.child(
                div()
                    .mt(px(30.))
                    .px(px(16.))
                    .text_size(px(13.))
                    .text_color(p.muted)
                    .text_center()
                    .child(if self.projects_view.list_view == ProjectListView::Archived && view_projects.is_empty() {
                        self.tr("没有已归档或隐藏的项目", "No archived or hidden projects")
                    } else if all_projects.is_empty() {
                        self.tr("还没有项目", "No projects yet")
                    } else {
                        self.tr("没有匹配的项目", "No matching projects")
                    }),
            );
        }
        for entry in projects {
            let inactive = !entry.is_active();
            let status = if entry.hidden {
                Some(self.tr("已隐藏", "Hidden"))
            } else if entry.archived && entry.retained {
                Some(self.tr("手动保留", "Kept active"))
            } else if entry.archived {
                Some(self.tr("已归档", "Archived"))
            } else { None };
            let retained = entry.retained && entry.archived;
            let agent_shortcuts = self.project_agent_shortcuts(&entry, cx);
            let proma_workspace_count = entry.proma_workspaces.len();
            let path = entry.path;
            let project_label = entry.name;
            let selected_path = path.clone();
            let selected = !self.projects_view.global_project_view
                && self.projects_view.open_project.as_ref() == Some(&path);
            let project_key = project_tag_key(&path);
            let assigned_project_tags = self
                .tags_flow
                .projects
                .assigned_tags(&project_key)
                .into_iter()
                .filter_map(|tag| self.tags_flow.projects.path(tag.id))
                .collect::<Vec<_>>();
            let mut project_tag_chips = div().mt(px(4.)).flex().gap(px(4.)).flex_wrap();
            for (index, tag) in assigned_project_tags.iter().enumerate() {
                project_tag_chips = project_tag_chips.child(
                    div()
                        .id(ElementId::Name(
                            format!("project-tag-{}-{index}", path.display()).into(),
                        ))
                        .px(px(3.))
                        .rounded(px(5.))
                        .font_family(MONO)
                        .text_size(px(10.))
                        .text_color(p.secondary)
                        .child(format!("#{tag}")),
                );
            }
            let reveal_path = path.clone();
            let set_tags_app = project_context_app.clone();
            let set_tags_path = path.clone();
            let remove_app = project_context_app.clone();
            let remove_path = path.clone();
            let set_tags_label = self.tr("设置标签", "Set tags").to_string();
            let reveal_label = if cfg!(target_os = "macos") {
                self.tr("在访达中显示", "Show in Finder")
            } else {
                self.tr("在文件夹中显示", "Show in folder")
            }
            .to_string();
            let visibility_action = if inactive { ProjectVisibilityAction::Restore } else { ProjectVisibilityAction::Hide };
            let remove_label = if inactive { self.tr("恢复到活跃项目", "Restore to active projects") }
                else { self.tr("在 Pleat 中隐藏", "Hide in Pleat") }.to_string();
            let follow_label = self.tr("跟随 Agent 归档状态", "Follow Agent archive status").to_string();
            let follow_app = project_context_app.clone();
            let follow_path = path.clone();
            recent_rows = recent_rows.child(
                div()
                    .id(ElementId::Name(
                        format!("project-{}", path.display()).into(),
                    ))
                    .role(Role::Button)
                    .aria_label(format!("{} · {}", self.tr("管理项目", "Manage project"), path.display()))
                    .min_h(px(48.))
                    .px(px(8.))
                    .py(px(6.))
                    .my(px(2.))
                    .rounded(px(RADIUS_CONTROL))
                    .flex()
                    .items_start()
                    .cursor_pointer()
                    .bg(if selected {
                        p.selected
                    } else {
                        rgba(0x00000000)
                    })
                    .hover(move |row| row.bg(p.hover))
                    .child(
                        div()
                            .w(px(28.))
                            .h(px(18.))
                            .flex_none()
                            .rounded(px(RADIUS_CONTROL))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Self::icon("icons/folder.svg", 16., p.secondary)),
                    )
                    .child(
                        div()
                            .ml(px(10.))
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .font_family(MONO)
                                    .text_size(px(13.))
                                    .line_height(px(18.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(if inactive { p.muted } else { p.text })
                                    .truncate()
                                    .child(project_label),
                            )
                            .child(
                                div()
                                    .mt(px(3.))
                                    .font_family(MONO)
                                    .text_size(px(11.))
                                    .line_height(px(14.))
                                    .text_color(p.muted)
                                    .truncate()
                                    .child(display_path(&path)),
                            )
                            .when(!entry.agents.is_empty(), |content| content.child(
                                div().flex().items_center().gap(px(6.)).child(agent_shortcuts)
                                    .when(proma_workspace_count > 1, |line| line.child(
                                        div().mt(px(4.)).text_size(px(11.)).text_color(p.muted).child(
                                            if self.uses_english() { format!("Proma · {proma_workspace_count} workspaces") }
                                            else { format!("Proma · {proma_workspace_count} 个工作区") }
                                        )
                                    ))
                            ))
                            .when(!assigned_project_tags.is_empty(), |content| {
                                content.child(project_tag_chips)
                            })
                            .when_some(status, |content, status| content.child(
                                div().mt(px(3.)).text_size(px(11.)).text_color(p.muted).child(status)
                            )),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.projects_view.open_project.as_ref() != Some(&selected_path) {
                            this.projects_view.batch_project = None;
                            this.projects_view.batch_selected.clear();
                            this.projects_view.batch_filter = None;
                        }
                        this.projects_view.open_project = Some(selected_path.clone());
                        this.projects_view.global_project_view = false;
                        this.projects_view.selected_project_agent = None;
                        this.projects_view.project_agents_expanded = false;
                        cx.notify();
                    }))
                    .context_menu(move |menu, _, _| {
                        let reveal_path = reveal_path.clone();
                        let set_tags_app = set_tags_app.clone();
                        let set_tags_path = set_tags_path.clone();
                        let remove_app = remove_app.clone();
                        let remove_path = remove_path.clone();
                        let follow_app = follow_app.clone();
                        let follow_path = follow_path.clone();
                        menu.min_w(px(160.))
                            .item(
                                PopupMenuItem::new(set_tags_label.clone())
                                    .icon(Icon::default().path("icons/hash.svg"))
                                    .on_click(move |_, _, cx| {
                                        let _ = set_tags_app.update(cx, |this, cx| {
                                            this.open_project_tag_assignment_dialog(
                                                set_tags_path.clone(),
                                                cx,
                                            );
                                        });
                                    }),
                            )
                            .item(
                                PopupMenuItem::new(reveal_label.clone())
                                    .icon(Icon::new(IconName::Folder))
                                    .on_click(move |_, _, cx| cx.reveal_path(&reveal_path)),
                            )
                            .item(PopupMenuItem::new(remove_label.clone()).on_click(move |_, window, cx| {
                                let _ = remove_app.update(cx, |this, cx| {
                                    this.change_project_visibility(&remove_path, visibility_action, window, cx);
                                });
                            }))
                            .when(retained, |menu| menu.item(PopupMenuItem::new(follow_label.clone()).on_click(move |_, window, cx| {
                                let _ = follow_app.update(cx, |this, cx| this.change_project_visibility(&follow_path, ProjectVisibilityAction::FollowAgent, window, cx));
                            })))
                    }),
            );
        }
        let global_selected = self.projects_view.global_project_view;
        let global_row = div()
            .id("global-project-skills")
            .role(Role::Button)
            .aria_label(self.tr("管理全局技能", "Manage global skills"))
            .min_h(px(52.))
            .mx(px(8.))
            .mb(px(6.))
            .px(px(8.))
            .rounded(px(RADIUS_CONTROL))
            .flex()
            .items_center()
            .cursor_pointer()
            .bg(if global_selected {
                p.selected
            } else {
                rgba(0x00000000)
            })
            .hover(move |row| row.bg(p.hover))
            .child(
                div()
                    .size(px(28.))
                    .flex_none()
                    .rounded(px(RADIUS_CONTROL))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Self::icon("icons/house.svg", 16., p.secondary)),
            )
            .child(
                div()
                    .ml(px(10.))
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .font_family(MONO)
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.tr("全局生效", "Global")),
                    )
                    .child(
                        div()
                            .mt(px(3.))
                            .font_family(MONO)
                            .text_size(px(11.))
                            .text_color(p.muted)
                            .truncate()
                            .child(match global_skill_count {
                                Some(count) if self.uses_english() => {
                                    counted(count, "user-level skill", "user-level skills")
                                }
                                Some(count) => format!("{count} 个用户级技能"),
                                None => self.tr("正在扫描…", "Scanning…").into(),
                            }),
                    ),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.projects_view.batch_project = None;
                this.projects_view.batch_selected.clear();
                this.projects_view.batch_filter = None;
                this.projects_view.global_project_view = true;
                this.projects_view.selected_project_agent = None;
                this.projects_view.project_agents_expanded = false;
                cx.notify();
            }));
        let recent_panel = div()
            .w_full()
            .min_w_0()
            .min_h_0()
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(p.surface)
            .child(
                self.panel_header(
                    "projects-panel-window-drag",
                    self.tr("项目", "Projects"),
                    total_project_count,
                    cx,
                )
                .child(
                    self.labeled_icon_button(
                        "open-project-folder",
                        "icons/plus.svg",
                        p.text,
                        self.tr("打开项目文件夹", "Open project folder"),
                        cx,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| this.browse_project(window, cx)),
                    ),
                ),
            )
            .child(
                div().px(px(10.)).py(px(8.)).child(
                    Input::new(&self.projects_view.project_search)
                        .small()
                        .h(px(SEARCH_HEIGHT))
                        .w_full()
                        .rounded(px(999.))
                        .bg(p.surface)
                        .border_color(p.border_strong)
                        .text_size(px(14.))
                        .prefix(Self::icon("icons/search.svg", 16., p.muted)),
                ),
            )
            .child(global_row)
            .child(
                div()
                    .px(px(16.))
                    .pb(px(6.))
                    .flex()
                    .items_center()
                    .text_size(px(12.))
                    .text_color(p.muted)
                    .child(
                        div().min_w_0().flex_1().child(list_control),
                    )
                    .child(agent_filter_control)
                    .child(self.tag_filter_control(TagScope::Projects, cx)),
            )
            .when_some(self.tags_flow.selected_project_filter.and_then(|tag| self.tags_flow.projects.path(tag)), |panel, tag| {
                panel.child(div().px(px(16.)).pb(px(6.)).text_size(px(11.)).text_color(p.secondary).child(format!("#{tag}")))
            })
            .child(recent_rows);

        let selected_root = if self.projects_view.global_project_view {
            global_root.as_ref()
        } else {
            self.projects_view.open_project.as_ref()
        };
        let detail = if let Some(open_project) = selected_root {
            let is_global = self.projects_view.global_project_view;
            let inactive_entry = all_projects.iter().find(|entry| entry.path == *open_project && !entry.is_active());
            let visibility_notice = inactive_entry.map(|entry| if entry.hidden {
                self.tr("已在 Pleat 中隐藏 · 文件和技能均保留", "Hidden in Pleat · files and skills are retained")
            } else {
                self.tr("已在 Agent 中归档 · 文件和技能均保留", "Archived in Agent · files and skills are retained")
            });
            let restore_path = open_project.clone();
            let project_skills = if is_global {
                global_skills
            } else {
                self.project_snapshot(open_project, cx)
            };
            let estimates = project_skills
                .as_ref()
                .and_then(|_| self.context_estimate_snapshot(open_project, cx));
            let loading = project_skills.is_none() || estimates.is_none();
            let project_skills = project_skills.unwrap_or_default();
            let estimates = estimates.unwrap_or_default();
            let incomplete = estimates.iter().any(|estimate| {
                self.projects_view.selected_project_agent.is_none_or(|agent| agent == estimate.agent)
                    && estimate.diagnostic.is_some()
            });
            let diagnostic_notice = self.tr(
                "发现结果不完整：部分 Agent 的实际生效条件尚无法确认；仅展示已知结果，技能和插件总数未知。",
                "Discovery is incomplete: availability conditions for some Agents cannot be confirmed. Only known results are shown; skill and plugin totals are unknown.",
            );
            let context_panel = if loading {
                div()
                    .h(px(86.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(9.))
                    .text_size(px(13.))
                    .text_color(p.muted)
                    .child(self.shell.spinner_accent.clone())
                    .child(self.tr("正在扫描项目…", "Scanning project…"))
            } else {
                self.context_estimate_panel(open_project, &estimates, cx)
            };
            let all_effective_rows = if loading {
                Arc::new(Vec::new())
            } else {
                let scanned_at = self.projects_view.context_estimates.borrow()[open_project].scanned_at;
                self.projects_view.skill_rows_cache.borrow_mut().get_or_build(
                    SkillRowsCacheKey {
                        project: open_project.clone(),
                        generation: self.projects_view.scan_generation,
                        agent: self.projects_view.selected_project_agent,
                        scanned_at,
                    },
                    &estimates,
                    &project_skills,
                )
            };
            let skill_count = all_effective_rows.len();
            let skill_query = self.projects_view.effective_skill_search.read(cx).value();
            let matching_rows = all_effective_rows.iter().filter(|row| row.matches_query(&skill_query)).cloned().collect::<Vec<_>>();
            let source_groups = skill_source_groups(&matching_rows);
            let selected_source = self.projects_view.selected_skill_source.as_ref()
                .filter(|(project, group)| project == open_project && source_groups.iter().any(|(available, _)| available == group))
                .map(|(_, group)| *group)
                .or_else(|| source_groups.first().map(|(group, _)| *group));
            let batch_mode = self.projects_view.batch_project.as_deref() == Some(open_project);
            let can_batch = batch_mode
                || all_effective_rows
                    .iter()
                    .any(|row| row.can_remove() || !row.batch_installations(ProjectBatchAction::DisableProma).is_empty()
                        || !row.batch_installations(ProjectBatchAction::EnableProma).is_empty());
            let effective_rows = Arc::new(matching_rows.iter()
                .filter(|row| Some(row.source_group()) == selected_source)
                .filter(|row| !batch_mode || self.matches_project_skill_filter(row))
                .cloned().collect::<Vec<_>>());
            let mut skills = div()
                .id("project-skills-scroll")
                .debug_selector(|| "project-skills-scroll".into())
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .px(px(0.))
                .pt(px(14.))
                .pb(px(28.));
            if effective_rows.is_empty() && !loading {
                let selected_agent = self
                    .projects_view
                    .selected_project_agent
                    .map(AgentKind::label)
                    .unwrap_or_default();
                skills = skills.child(
                    div()
                        .mt(px(70.))
                        .flex()
                        .flex_col()
                        .items_center()
                        .text_color(p.muted)
                        .child(Self::icon("icons/package.svg", 28., p.muted))
                        .child(div().mt(px(10.)).text_size(px(14.)).child(
                            if incomplete {
                                self.tr("无法确认生效技能；发现结果未知或不完整", "Effective skills cannot be confirmed; discovery is unknown or incomplete").to_string()
                            } else if !skill_query.trim().is_empty() {
                                self.tr("没有匹配名称或描述的技能", "No skill names or descriptions match the search").to_string()
                            } else if batch_mode && self.projects_view.batch_filter.is_some() {
                                self.tr("当前分类没有符合筛选条件的技能", "No skills match the filter in this category").to_string()
                            } else if self.projects_view.selected_project_agent.is_some() {
                                if self.uses_english() {
                                    format!("No effective skills for {selected_agent}")
                                } else {
                                    format!("没有检测到对 {selected_agent} 生效的技能")
                                }
                            } else if is_global {
                                self.tr(
                                    "没有检测到用户级生效的技能",
                                    "No user-level skills detected",
                                )
                                .to_string()
                            } else {
                                self.tr(
                                    "没有检测到对这个项目生效的技能",
                                    "No effective skills detected for this project",
                                )
                                .to_string()
                            },
                        )),
                );
            }
            if !effective_rows.is_empty() {
                let rows = Arc::clone(&effective_rows);
                let list_key = (Some(open_project.clone()), self.projects_view.selected_project_agent, selected_source, rows.len());
                if *self.projects_view.skill_list_items.borrow() != list_key {
                    self.projects_view.skill_list.reset(rows.len());
                    *self.projects_view.skill_list_items.borrow_mut() = list_key;
                }
                let project = open_project.clone();
                skills = skills.child(
                    div().id("project-skills-virtual-list")
                        .debug_selector(|| "project-skills-virtual-list".into()).flex_1().min_h_0()
                        .child(list(
                        self.projects_view.skill_list.clone(),
                        cx.processor(move |this, index, _, cx| {
                            this.project_skill_row(&rows[index], &project, cx)
                        }),
                    )
                    .size_full()),
                );
            }
            let plugin_groups =
                effective_plugin_groups(&estimates, self.projects_view.selected_project_agent);
            let plugin_skill_count = plugin_groups
                .iter()
                .map(|plugin| plugin.skills.len())
                .sum::<usize>();
            let mut tabs = self
                .project_skills_tabs_with_diagnostic(skill_count, plugin_skill_count, incomplete, cx)
                .child(div().flex_1());
            if self.projects_view.project_skills_tab == ProjectSkillsTab::Skills {
                tabs = tabs.child(Input::new(&self.projects_view.effective_skill_search)
                    .small().w(px(230.)).mr(px(10.))
                    .prefix(Self::icon("icons/search.svg", 14., p.muted)));
            }
            if can_batch && self.projects_view.project_skills_tab == ProjectSkillsTab::Skills {
                if batch_mode {
                    tabs = tabs.child(self.project_batch_controls(
                        open_project,
                        &all_effective_rows,
                        &effective_rows,
                        cx,
                    ));
                } else {
                    let batch_project_path = open_project.clone();
                    let batch_label = self.tr("批量管理", "Batch manage").to_string();
                    tabs = tabs.child(
                        div().h(px(CONTROL_HEIGHT)).flex().items_center().child(
                            div()
                                .id("project-batch-toggle")
                                .h(px(CONTROL_HEIGHT)).px(px(7.))
                                .rounded(px(RADIUS_CONTROL))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .hover(move |button| button.bg(p.hover))
                                .child(Self::icon("icons/check.svg", 15., p.secondary))
                                .child(div().ml(px(4.)).text_size(px(12.)).text_color(p.secondary).child(batch_label.clone()))
                                .tooltip(move |window, cx| {
                                    Tooltip::new(batch_label.clone()).build(window, cx)
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.projects_view.batch_project =
                                        Some(batch_project_path.clone());
                                    this.projects_view.batch_selected.clear();
                                    this.projects_view.batch_action = if this.projects_view.selected_project_agent == Some(AgentKind::Proma) {
                                        ProjectBatchAction::DisableProma
                                    } else { ProjectBatchAction::Remove };
                                    cx.notify();
                                })),
                        ),
                    );
                }
            }
            let skills = match self.projects_view.project_skills_tab {
                ProjectSkillsTab::Skills => skills,
                ProjectSkillsTab::Plugins if incomplete && plugin_groups.is_empty() => div()
                    .id("project-plugins-scroll")
                    .flex_1().min_h_0().px(px(28.)).pt(px(70.))
                    .text_size(px(14.)).text_color(p.muted)
                    .child(self.tr("无法确认插件技能；发现结果未知或不完整", "Plugin skills cannot be confirmed; discovery is unknown or incomplete")),
                ProjectSkillsTab::Plugins => self.effective_plugins_list(plugin_groups, cx),
            };
            div()
                .relative()
                .min_w_0()
                .flex_1()
                .h_full()
                .min_h_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex_none()
                        .px(px(24.))
                        .pt(px(24.))
                        .pb(px(18.))
                        .border_b_1()
                        .border_color(p.border)
                        .child(
                            div()
                                .font_family(MONO)
                                .text_size(px(16.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(self.selectable_text(
                                    "project-detail-name",
                                    300,
                                    if is_global {
                                        self.tr("全局生效", "Global").to_string()
                                    } else {
                                        all_projects.iter()
                                            .find(|entry| entry.path == *open_project)
                                            .map(|entry| entry.name.clone())
                                            .unwrap_or_else(|| open_project.file_name().unwrap_or_default()
                                                .to_string_lossy().into_owned())
                                    },
                                    window,
                                    cx,
                                )),
                        )
                        .when(!is_global, |header| {
                            header.child(
                                div()
                                    .mt(px(4.))
                                    .font_family(MONO)
                                    .text_size(px(12.))
                                    .text_color(p.muted)
                                    .child(self.selectable_text(
                                        "project-detail-path",
                                        301,
                                        display_path(open_project),
                                        window,
                                        cx,
                                    )),
                            )
                        })
                        .when_some(visibility_notice, |header, message| header.child(
                            div().mt(px(10.)).flex().items_center().gap(px(10.)).text_size(px(12.)).text_color(p.muted)
                                .child(message)
                                .child(Button::new("restore-project-visibility").xsmall().label(self.tr("恢复到活跃项目", "Restore to active projects"))
                                    .on_click(cx.listener(move |this, _, window, cx| this.change_project_visibility(&restore_path, ProjectVisibilityAction::Restore, window, cx))))
                        )),
                )
                .child(
                    div()
                        .px(px(28.))
                        .pt(px(10.))
                        .pb(px(12.))
                        .flex_none()
                        .border_b_1()
                        .border_color(p.border)
                        .child(context_panel),
                )
                .child(tabs)
                .when(incomplete, |detail| detail.child(
                    div().px(px(28.)).py(px(8.)).text_size(px(12.)).text_color(p.warning)
                        .child(diagnostic_notice),
                ))
                .when(self.projects_view.project_skills_tab == ProjectSkillsTab::Skills && !source_groups.is_empty(), |panel| {
                    panel.child(self.project_skill_source_tabs(open_project, &source_groups, selected_source, cx))
                })
                .child(skills)
                .child(self.window_drag_strip("project-detail-window-drag", 24., cx))
        } else {
            let first = self.model.library.config.recent_projects.is_empty();
            div()
                .relative()
                .min_w_0()
                .flex_1()
                .h_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .size(px(48.))
                        .rounded(px(RADIUS_CARD))
                        .bg(p.raised)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Self::icon("icons/folder.svg", 22., p.secondary)),
                )
                .child(
                    div()
                        .mt(px(16.))
                        .text_size(px(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(if first {
                            self.tr("打开第一个项目文件夹", "Open your first project folder")
                        } else {
                            self.tr("打开一个项目文件夹", "Open a project folder")
                        }),
                )
                .child(
                    div()
                        .mt(px(7.))
                        .max_w(px(430.))
                        .text_center()
                        .text_size(px(14.))
                        .line_height(relative(1.55))
                        .text_color(p.muted)
                        .child(self.tr(
                            "Pleat 会读取项目里的技能，并让你管理它们的安装边界。",
                            "Pleat reads the skills in a project and lets you manage where they are installed.",
                        )),
                )
                .child(
                    self.primary_icon_button(
                        "empty-open-project",
                        "icons/folder.svg",
                        self.tr("打开项目文件夹", "Open project folder"),
                        cx,
                    )
                    .mt(px(17.))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| this.browse_project(window, cx)),
                    ),
                )
                .child(self.window_drag_strip("empty-project-detail-window-drag", 24., cx))
        };
        let page = layout::content(
            &self.shell.content_layout,
            self.content_pane_width(window, cx),
            recent_panel,
            detail,
        );
        #[cfg(debug_assertions)]
        if let Some(path) = std::env::var_os("KITTER_PREVIEW_PROJECT_TIMINGS")
            && let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path)
        {
            use std::io::Write;
            let _ = writeln!(file, "{:.6}", render_started.elapsed().as_secs_f64() * 1000.);
        }
        page
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, path::PathBuf};

    use crate::{InstallTarget, ProjectSkillInstallation};

    use super::{EffectiveSkillRow, selected_project_skills};

    fn row(name: &str, paths: &[&str]) -> EffectiveSkillRow {
        EffectiveSkillRow {
            key: paths.first().map(|path| super::super::effective_view::skill_file_key(&PathBuf::from(path).join("SKILL.md")))
                .unwrap_or_else(|| format!("fixture:{name}")),
            name: name.into(),
            duplicate_name: false,
            description: String::new(),
            native_hint: None, grok_manual: None,
            locations: Vec::new(),
            built_in: false,
            agents: Vec::new(),
            manual_only: false, shadowed_agents: Vec::new(), disabled_agents: Vec::new(), unloaded_agents: Vec::new(), unverified_agents: Vec::new(),
            origins: Vec::new(),
            direct_installations: paths
                .iter()
                .map(|path| ProjectSkillInstallation {
                    target: InstallTarget::Universal,
                    path: PathBuf::from(path),
                    managed: true, source: None,
                    removable: true, toggleable: false,
                    bundled: false,
                    integration: None,
                    state: None,
                    status: None,
                })
                .collect(),
        }
    }

    #[test]
    fn same_named_batch_selection_requires_the_actual_source_key() {
        let rows = [row("pdf", &["/project/.codex/skills/pdf"]), row("pdf", &["/native/skills/pdf"])];
        let by_name = HashSet::from(["pdf".to_string()]);
        assert!(selected_project_skills(&rows, &by_name, super::ProjectBatchAction::Remove).is_empty(),
            "a display name cannot select multiple independent copies");
        let by_source = HashSet::from(["file:/project/.codex/skills/pdf/SKILL.md".to_string()]);
        let targets = selected_project_skills(&rows, &by_source, super::ProjectBatchAction::Remove);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].installations[0].path, PathBuf::from("/project/.codex/skills/pdf"));
    }

    #[test]
    fn batch_removal_uses_only_the_selected_rows_direct_installations() {
        let rows = vec![
            row("selected", &["/project/.agents/skills/selected"]),
            row("shadowed", &["/project/.claude/skills/shadowed"]),
            row("external-only", &[]),
        ];
        let selected = HashSet::from([rows[0].key.clone(), rows[2].key.clone()]);

        let targets = selected_project_skills(&rows, &selected, super::ProjectBatchAction::Remove);

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].name, "selected");
        assert_eq!(targets[0].installations.len(), 1);
        assert_eq!(
            targets[0].installations[0].path,
            PathBuf::from("/project/.agents/skills/selected")
        );
    }

    #[test]
    fn batch_removal_excludes_protected_installations_in_mixed_rows() {
        let mut mixed = row("mixed", &["/project/.agents/skills/mixed", "/pilot/skills/mixed"]);
        mixed.direct_installations[1].target = InstallTarget::Proma;
        mixed.direct_installations[1].managed = false;
        mixed.direct_installations[1].removable = false;
        let mut protected = row("protected", &["/pilot/skills/protected"]);
        protected.direct_installations[0].removable = false;
        assert!(mixed.can_remove());
        assert!(!protected.can_remove());
        let selected = HashSet::from([mixed.key.clone(), protected.key.clone()]);
        let targets = selected_project_skills(&[mixed, protected], &selected, super::ProjectBatchAction::Remove);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].installations.len(), 1);
        assert_eq!(targets[0].installations[0].target, InstallTarget::Universal);
    }

    #[cfg(unix)]
    #[test]
    fn preset_state_selection_targets_only_matching_proma_copies() {
        use super::ProjectBatchAction;
        let mut mixed = row("pdf", &["/project/.agents/skills/pdf", "/pilot/skills/pdf", "/pilot/skills-inactive/pdf"]);
        for (index, state) in [(1, "active"), (2, "inactive")] {
            let installation = &mut mixed.direct_installations[index];
            installation.target = InstallTarget::Proma;
            installation.managed = false;
            installation.removable = false;
            installation.bundled = true;
            installation.toggleable = true;
            installation.state = Some(state.into());
        }
        let mut external = mixed.clone();
        external.name = "protected".into();
        external.key = "fixture:protected".into();
        for installation in &mut external.direct_installations { installation.bundled = false; installation.toggleable = false; }
        let inherited = row("inherited", &[]);
        let rows = [mixed, external, inherited];
        let selected = rows.iter().map(|row| row.key.clone()).collect();
        for (action, path) in [(ProjectBatchAction::DisableProma, "/pilot/skills/pdf"),
                               (ProjectBatchAction::EnableProma, "/pilot/skills-inactive/pdf")] {
            let targets = selected_project_skills(&rows, &selected, action);
            assert_eq!(targets.len(), 1);
            assert_eq!(targets[0].name, "pdf");
            assert_eq!(targets[0].installations.len(), 1);
            assert_eq!(targets[0].installations[0].path, PathBuf::from(path));
        }
        let removal = selected_project_skills(&rows, &selected, ProjectBatchAction::Remove);
        assert_eq!(removal[0].installations[0].target, InstallTarget::Universal);
    }
}
