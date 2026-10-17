use super::*;
use crate::library::StorageAction;

impl KitterApp {
    pub(super) fn settings_page(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let p = self.palette();
        let row = |title: &'static str, description: &'static str| {
            div()
                .min_h(px(60.))
                .px(px(16.))
                .py(px(12.))
                .flex()
                .items_center()
                .gap(px(24.))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .child(
                            div()
                                .text_size(px(14.))
                                .font_weight(FontWeight::MEDIUM)
                                .child(title),
                        )
                        .child(
                            div()
                                .mt(px(2.))
                                .text_size(px(12.))
                                .text_color(p.muted)
                                .child(description),
                        ),
                )
        };
        let language_label = match self.model.library.config.language {
            Language::System => self.tr("跟随系统", "System"),
            Language::ZhCn => "简体中文",
            Language::En => "English",
        };
        let app = cx.entity().downgrade();
        let selected_language = self.model.library.config.language;
        let english = self.uses_english();
        let language_control = Popover::new("language-menu")
            .appearance(false)
            .anchor(Anchor::TopRight)
            .trigger(
                self.dropdown_button("language-select", language_label, 14.)
                    .w(px(180.)),
            )
            .content(move |_, _, popover_cx| {
                let mut menu = div()
                    .w(px(180.))
                    .p(px(4.))
                    .rounded(px(RADIUS_MENU))
                    .border_1()
                    .border_color(p.border_strong)
                    .bg(p.elevated)
                    .shadow_lg();
                for (index, (language, label)) in [
                    (
                        Language::System,
                        if english { "System" } else { "跟随系统" },
                    ),
                    (Language::ZhCn, "简体中文"),
                    (Language::En, "English"),
                ]
                .into_iter()
                .enumerate()
                {
                    let active = selected_language == language;
                    let app = app.clone();
                    menu = menu.child(
                        div()
                            .id(ElementId::Name(format!("language-option-{index}").into()))
                            .h(px(29.))
                            .px(px(8.))
                            .rounded(px(RADIUS_CONTROL))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .bg(if active { p.selected } else { rgba(0x00000000) })
                            .hover(move |item| item.bg(p.hover))
                            .font_family(FONT_UI)
                            .text_size(px(13.))
                            .child(label)
                            .on_click(popover_cx.listener(move |_, _, window, cx| {
                                let _ = app.update(cx, |this, cx| {
                                    this.set_language(language, window, cx);
                                });
                                cx.emit(DismissEvent);
                            })),
                    );
                }
                menu
            });
        let preferences_group = div()
            .w_full()
            .rounded(px(RADIUS_CARD))
            .border_1()
            .border_color(p.border)
            .bg(p.surface)
            .overflow_hidden()
            .child(
                row(
                    self.tr("语言", "Language"),
                    self.tr(
                        "选择 Pleat 界面的显示语言。",
                        "Choose the language used by Pleat.",
                    ),
                )
                .child(language_control),
            );
        let appearance_group = div()
            .w_full()
            .rounded(px(RADIUS_CARD))
            .border_1()
            .border_color(p.border)
            .bg(p.surface)
            .overflow_hidden()
            .child(
                row(
                    self.tr("主题", "Theme"),
                    self.tr(
                        "跟随系统，或固定使用浅色与深色外观。",
                        "Follow the system or use a fixed light or dark appearance.",
                    ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(2.))
                        .child(self.small_choice(
                            "theme-system",
                            self.tr("跟随系统", "System"),
                            self.model.library.config.theme == Theme::System,
                            cx,
                            |this| this.model.library.config.theme = Theme::System,
                        ))
                        .child(self.small_choice(
                            "theme-light",
                            self.tr("浅色", "Light"),
                            self.model.library.config.theme == Theme::Light,
                            cx,
                            |this| this.model.library.config.theme = Theme::Light,
                        ))
                        .child(self.small_choice(
                            "theme-dark",
                            self.tr("深色", "Dark"),
                            self.model.library.config.theme == Theme::Dark,
                            cx,
                            |this| this.model.library.config.theme = Theme::Dark,
                        )),
                ),
            );
        let library_group = div()
            .w_full()
            .rounded(px(RADIUS_CARD))
            .border_1()
            .border_color(p.border)
            .bg(p.surface)
            .overflow_hidden()
            .child(
                row(
                    self.tr("技能库位置", "Library location"),
                    self.tr(
                        "统一管理技能源的文件夹。",
                        "The folder used to manage your skill sources.",
                    ),
                )
                .child(
                    div()
                        .w(px(380.))
                        .flex()
                        .items_center()
                        .gap(px(7.))
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .font_family(MONO)
                                .text_size(px(12.))
                                .text_color(p.secondary)
                                .truncate()
                                .child(self.selectable_text(
                                    "settings-library-path",
                                    400,
                                    display_path(&self.model.library.config.library_dir),
                                    window,
                                    cx,
                                )),
                        )
                        .child(
                            self.toolbar_text_button(
                                "open-library",
                                self.tr("打开文件夹", "Open folder"),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.reveal_path(&this.model.library.config.library_dir);
                            })),
                        ),
                ),
            );

        let advanced_toggle = self
            .toolbar_text_button(
                "storage-advanced",
                self.tr("高级存储设置", "Advanced storage settings"),
            )
            .mt(px(8.))
            .gap(px(4.))
            .child(Self::icon(
                if self.settings_storage.advanced {
                    "icons/chevron-up.svg"
                } else {
                    "icons/chevron-down.svg"
                },
                12.,
                p.secondary,
            ))
            .on_click(cx.listener(|this, _, _, cx| {
                this.settings_storage.advanced = !this.settings_storage.advanced;
                cx.notify();
            }));
        let advanced_group = div()
            .mt(px(8.)).w_full().rounded(px(RADIUS_CARD))
            .border_1().border_color(p.border).bg(p.surface).overflow_hidden()
            .child(
                row(self.tr("更换技能库", "Change library"), self.tr("先预览影响，再确认迁移或切换；不会删除原有安装。", "Review the impact before moving or switching; existing installations are retained."))
                    .child(div().flex().gap(px(4.))
                        .child(self.toolbar_text_button("move-library", self.tr("迁移当前库…", "Move current library…"))
                            .on_click(cx.listener(|this, _, window, cx| this.choose_storage_target(StorageAction::MoveLibrary, window, cx))))
                        .child(self.toolbar_text_button("use-library", self.tr("使用已有库…", "Use existing library…"))
                            .on_click(cx.listener(|this, _, window, cx| this.choose_storage_target(StorageAction::UseLibrary, window, cx)))))
            )
            .child(
                div().px(px(16.)).py(px(12.)).border_t_1().border_color(p.border)
                    .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child(self.tr("应用数据目录", "App data folder")))
                    .child(div().mt(px(5.)).font_family(MONO).text_size(px(12.)).text_color(p.secondary)
                        .child(display_path(self.model.library.data_dir())))
                    .when(self.model.library.data_dir().file_name().is_some_and(|name| name == "Kitter"), |row| {
                        row.child(div().mt(px(8.)).text_size(px(12.)).text_color(p.muted)
                            .child(self.tr("可迁移到 Pleat；原路径保留兼容入口，现有技能和本机环境继续可用。", "Move to Pleat while retaining a compatibility entry for existing skills and local runtimes.")))
                            .child(self.toolbar_text_button("move-data-to-pleat", self.tr("预览迁移到 Pleat", "Preview move to Pleat"))
                                .mt(px(6.)).on_click(cx.listener(|this, _, _, cx| this.preview_pleat_data_migration(cx))))
                    })
            );

        let about_group = div()
            .w_full()
            .rounded(px(RADIUS_CARD))
            .border_1()
            .border_color(p.border)
            .bg(p.surface)
            .p(px(16.))
            .flex()
            .items_center()
            .gap(px(14.))
            .child(img("brand/pleat-icon.png").size(px(72.)).flex_none())
            .child(
                div()
                    .flex_1()
                    .child(div().text_size(px(20.)).font_weight(FontWeight::MEDIUM).child("Pleat"))
                    .child(div().mt(px(2.)).text_size(px(12.)).text_color(p.secondary)
                        .child(format!("{} · {}", self.tr("版本", "Version"), env!("CARGO_PKG_VERSION"))))
                    .child(div().mt(px(8.)).text_size(px(12.)).text_color(p.muted)
                        .child(self.tr("基于 Kitter", "Based on Kitter")))
                    .child(
                        div().mt(px(6.)).flex().child(
                            gpui_base::Link::new("about-project-home")
                                .href("https://github.com/caaaptaintop/Pleat")
                                .open_with(|href, _, _, cx| cx.open_url(href))
                                .accessibility_label(self.tr("项目主页", "Project homepage"))
                                .text_size(px(12.))
                                .text_color(p.accent)
                                .cursor_pointer()
                                .hover(|link| link.underline())
                                .child(self.tr("项目主页", "Project homepage")),
                        ),
                    ),
            );

        div()
            .relative()
            .flex_1()
            .h_full()
            .p(px(20.))
            .flex()
            .justify_center()
            .child(
                div()
                    .id("settings-scroll")
                    .w_full()
                    .h_full()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        div().w_full().flex().justify_center().child(
                            div()
                                .w_full()
                                .max_w(px(SETTINGS_CONTENT_MAX_WIDTH))
                                .pb(px(40.))
                                .child(
                                    div()
                                        .mb(px(10.))
                                        .text_size(px(14.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(self.tr("偏好设置", "Preferences")),
                                )
                                .child(preferences_group)
                                .child(
                                    div()
                                        .mt(px(40.))
                                        .mb(px(10.))
                                        .text_size(px(14.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(self.tr("外观", "Appearance")),
                                )
                                .child(appearance_group)
                                .child(
                                    div()
                                        .mt(px(40.))
                                        .mb(px(10.))
                                        .text_size(px(14.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(self.tr("技能库", "Skill library")),
                                )
                                .child(library_group)
                                .child(advanced_toggle)
                                .when(self.settings_storage.advanced, |content| {
                                    content.child(advanced_group)
                                })
                                .child(
                                    div()
                                        .mt(px(28.))
                                        .mb(px(10.))
                                        .text_size(px(14.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(self.tr("关于 Pleat", "About Pleat")),
                                )
                                .child(about_group),
                        ),
                    ),
            )
            .child(self.window_drag_strip("settings-window-drag", 20., cx))
    }
}
