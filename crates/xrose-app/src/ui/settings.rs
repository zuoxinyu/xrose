use gpui_base::Disableable;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Theme, ThemeMode as ComponentThemeMode};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::model::CoreKind;
use xrose_core::service::ToastLevel;
use xrose_core::settings::{
    BackdropStyle, HttpLatencyMode, Language, LogLevel, MotionMode, Settings, ThemeMode,
};
use xrose_core::system::CheckLevel;

use super::AppView;
use super::theme::{Palette, palette};
use super::widgets::*;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum SettingsTab {
    Appearance,
    Startup,
    Core,
    #[default]
    Network,
    Updates,
    Helper,
}

impl SettingsTab {
    const ALL: [Self; 6] = [
        Self::Appearance,
        Self::Startup,
        Self::Core,
        Self::Network,
        Self::Updates,
        Self::Helper,
    ];

    fn label(self, lang: Language) -> SharedString {
        match self {
            Self::Appearance => super::tr(lang, "外观", "Appearance"),
            Self::Core => super::tr(lang, "内核", "Core"),
            Self::Network => super::tr(lang, "网络与代理", "Network & proxy"),
            Self::Startup => super::tr(lang, "启动与窗口", "Startup & window"),
            Self::Updates => super::tr(lang, "更新与备份", "Updates & backups"),
            Self::Helper => super::tr(lang, "提权助手", "Privilege helper"),
        }
    }

    fn keyboard_target(self, key: &str) -> Option<Self> {
        tab_keyboard_target(self as usize, Self::ALL.len(), key).map(|index| Self::ALL[index])
    }
}

impl AppView {
    fn modify_settings(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Settings)) {
        let mut settings = self.service.settings();
        f(&mut settings);
        self.service.update_settings(settings);
        self.sync(cx);
    }

    fn lan_account_input(&self, cx: &App) -> xrose_core::settings::ProxyAccount {
        xrose_core::settings::ProxyAccount {
            username: self.lan_user_input.read(cx).value().to_string(),
            password: self.lan_password_input.read(cx).value().to_string(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn urltest_setting_row(
        &self,
        p: &Palette,
        label: impl Into<SharedString>,
        description: impl Into<SharedString>,
        input: &Entity<InputState>,
        saved: &str,
        default_url: &'static str,
        ids: (&'static str, &'static str),
        set_value: fn(&mut Settings, String),
        cx: &mut Context<Self>,
    ) -> Div {
        let value = input.read(cx).value();
        let trimmed = value.trim().to_string();
        let valid = xrose_core::settings::valid_http_test_url(&trimmed);
        let dirty = trimmed != saved.trim();
        let can_reset = trimmed != default_url || saved.trim() != default_url;
        let input_handle = input.clone();
        let reset_handle = input.clone();
        let saved_for_reset = saved.trim().to_string();
        let (save_id, reset_id) = ids;

        setting_row(
            p,
            label,
            description,
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(super::theme::SETTING_LABEL_WIDTH * 2.))
                                .child(Input::new(input)),
                        )
                        .when(dirty, |d| {
                            d.child(
                                Button::new(save_id)
                                    .primary()
                                    .label(self.tr("应用", "Apply"))
                                    .disabled(!valid)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let next = input_handle.read(cx).value().trim().to_string();
                                        if xrose_core::settings::valid_http_test_url(&next) {
                                            input_handle.update(cx, |state, cx| {
                                                state.set_value(next.clone(), window, cx);
                                            });
                                            this.modify_settings(cx, |s| set_value(s, next));
                                        }
                                    })),
                            )
                        })
                        .when(can_reset, |d| {
                            d.child(
                                Button::new(reset_id)
                                    .label(self.tr("重置", "Reset"))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        reset_handle.update(cx, |state, cx| {
                                            state.set_value(default_url, window, cx);
                                        });
                                        if saved_for_reset != default_url {
                                            this.modify_settings(cx, |s| {
                                                set_value(s, default_url.to_string())
                                            });
                                        }
                                    })),
                            )
                        }),
                )
                .when(!valid, |d| {
                    d.child(div().text_xs().text_color(p.danger_text).child(self.tr(
                        "请输入有效的 HTTP/HTTPS 地址",
                        "Enter a valid HTTP/HTTPS URL",
                    )))
                }),
        )
    }

    fn advanced_network_settings(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let settings = &self.snapshot.settings;
        let lang = self.lang();
        let lan_account = self.lan_account_input(cx);
        let lan_port = valid_port(self.lan_port_input.read(cx).value().as_ref());
        let latency_mode_row = setting_row(
            p,
            self.tr("延迟测速口径", "Latency mode"),
            self.tr(
                "「统一延迟」建立连接后仅计单次请求往返，剔除协议握手差异；「含首次握手」计入冷启动建连与 TLS 握手耗时。",
                "\"Unified\" warms the connection and measures one round trip; \"With handshake\" includes cold connection and TLS setup.",
            ),
            ChoiceGroup::new(
                "http-latency-mode",
                vec![
                    self.tr("统一延迟", "Unified"),
                    self.tr("含首次握手", "With handshake"),
                ],
                usize::from(settings.http_latency_mode == HttpLatencyMode::FullHandshake),
            )
            .on_change(cx.listener(|this, index: &usize, _, cx| {
                let mode = if *index == 0 {
                    HttpLatencyMode::Unified
                } else {
                    HttpLatencyMode::FullHandshake
                };
                this.modify_settings(cx, |s| s.http_latency_mode = mode);
            })),
        );
        let domestic_row = self.urltest_setting_row(
            p,
            self.tr("国内出口测速地址", "Domestic URL test"),
            self.tr(
                "用于本机直连 HTTP 测速；本机吞吐测速默认使用国内镜像源。",
                "Used for direct HTTP latency probes; local throughput probes use a domestic CDN by default.",
            ),
            &self.domestic_urltest_input,
            &settings.domestic_urltest_url,
            xrose_core::settings::DEFAULT_DOMESTIC_URLTEST_URL,
            ("save-domestic-urltest", "reset-domestic-urltest"),
            |s, v| s.domestic_urltest_url = v,
            cx,
        );
        let overseas_row = self.urltest_setting_row(
            p,
            self.tr("国外出口测速地址", "Overseas URL test"),
            with_reload_note(
                lang,
                settings.hot_reload,
                "用于节点与组网出口 HTTP 测速及自动切换测速",
                "Used for node, mesh exit, and automatic group URL tests",
            ),
            &self.overseas_urltest_input,
            &settings.urltest_url,
            xrose_core::settings::DEFAULT_OVERSEAS_URLTEST_URL,
            ("save-overseas-urltest", "reset-overseas-urltest"),
            |s, v| s.urltest_url = v,
            cx,
        );
        div().flex().flex_col().gap_3()
            .child(latency_mode_row)
            .child(domestic_row)
            .child(overseas_row)
            .child(divider(p))
            .when(settings.allow_lan, |card| card
                .child(setting_row(p, self.tr("第二端口", "Second port"),
                    self.tr("启用后主端口仅供本机使用，局域网设备使用第二端口", "Use the main port locally and the second port for LAN devices"),
                    toggle("separate-lan-port", self.tr("使用独立局域网端口", "Use a separate LAN port"), settings.separate_lan_port)
                        .disabled(!settings.separate_lan_port && lan_port.is_none())
                        .on_change(cx.listener(move |this, enabled, _, cx| this.modify_settings(cx, |s| {
                            s.separate_lan_port = *enabled;
                            if *enabled { if let Some(port) = lan_port { s.lan_port = port; } }
                        })))))
                .child(setting_row(p, self.tr("局域网端口", "LAN port"), "",
                    div().flex().items_center().gap_2()
                        .child(div().w(px(super::theme::SETTING_LABEL_WIDTH)).child(Input::new(&self.lan_port_input)))
                        .when(lan_port != Some(settings.lan_port), |d| d.child(Button::new("save-lan-port")
                            .label(self.tr("应用", "Apply")).disabled(lan_port.is_none())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(port) = valid_port(this.lan_port_input.read(cx).value().as_ref()) { this.modify_settings(cx, |s| s.lan_port = port); }
                            }))))
                        .when(lan_port.is_none(), |d| d.child(div().text_sm().text_color(p.danger_text)
                            .child(self.tr("端口必须是 1–65535", "Enter a port from 1 to 65535"))))))
                .child(setting_row(p, self.tr("局域网认证", "LAN authentication"),
                    self.tr("输入用户名和密码后启用。共用主端口时，本机客户端也需认证", "Enter credentials to enable. Local clients also authenticate when sharing the main port."),
                    toggle("lan-auth", self.tr("要求账号认证", "Require authentication"), settings.lan_auth)
                        .disabled(!settings.lan_auth && !lan_account.valid())
                        .on_change(cx.listener(|this, enabled, _, cx| {
                            let account = this.lan_account_input(cx);
                            this.modify_settings(cx, |s| { s.lan_auth = *enabled; if *enabled { s.lan_account = account; } });
                        }))))
                .child(setting_row(p, self.tr("用户名", "Username"), "",
                    div().w(px(super::theme::SETTING_LABEL_WIDTH)).child(Input::new(&self.lan_user_input))))
                .child(setting_row(p, self.tr("密码", "Password"), "",
                    div().flex().items_center().gap_2()
                        .child(div().w(px(super::theme::SETTING_LABEL_WIDTH)).child(Input::new(&self.lan_password_input)))
                        .when(settings.lan_auth && lan_account != settings.lan_account, |d| d.child(Button::new("save-lan-account")
                            .label(self.tr("保存账号", "Save credentials")).disabled(!lan_account.valid())
                            .on_click(cx.listener(|this, _, _, cx| {
                                let account = this.lan_account_input(cx);
                                this.modify_settings(cx, |s| s.lan_account = account);
                            })))))))
            .when(!settings.allow_lan, |card| card.child(div().text_sm().text_color(p.muted_fg)
                .child(self.tr("开启局域网连接后可设置独立端口和账号认证。", "Enable LAN connections to configure a separate port and authentication."))))
    }

    fn select_settings_tab(
        &mut self,
        tab: SettingsTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if tab != self.settings_tab {
            self.settings_tab = tab;
            self.settings_scroll.set_offset(point(px(0.), px(0.)));
        }
        self.settings_tab_focus[tab as usize].focus(window, cx);
        self.settings_tab_scroll.scroll_to_item(tab as usize);
        cx.notify();
    }

    pub(crate) fn page_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.settings_tab;
        let lang = self.lang();
        let focus_bg = palette(cx).accent;
        let content = match selected {
            SettingsTab::Appearance => self.settings_appearance_card(cx),
            SettingsTab::Core => self.settings_core_card(cx),
            SettingsTab::Network => self.settings_network_card(window, cx),
            SettingsTab::Startup => self.settings_startup_card(cx),
            SettingsTab::Updates => self.settings_updates_card(cx),
            SettingsTab::Helper => self.settings_helper_card(cx),
        };
        div()
            .flex()
            .flex_col()
            .gap_4()
            .size_full()
            .min_w(px(0.))
            .min_h(px(0.))
            .child(
                TabBar::new("settings-tabs")
                    .underline()
                    .track_scroll(&self.settings_tab_scroll)
                    .w_full()
                    .flex_shrink_0()
                    .selected_index(selected as usize)
                    .children(SettingsTab::ALL.map(|tab| {
                        Tab::new()
                            .label(tab.label(lang))
                            .track_focus(&self.settings_tab_focus[tab as usize])
                            .tab_stop(tab == selected)
                            .focus_visible(move |style| style.bg(focus_bg))
                            .on_key_down(cx.listener(
                                move |this, event: &KeyDownEvent, window, cx| {
                                    if let Some(next) = tab.keyboard_target(&event.keystroke.key) {
                                        this.select_settings_tab(next, window, cx);
                                        window.prevent_default();
                                        cx.stop_propagation();
                                    }
                                },
                            ))
                    }))
                    .on_click(cx.listener(|this, index: &usize, window, cx| {
                        if let Some(tab) = SettingsTab::ALL.get(*index) {
                            this.select_settings_tab(*tab, window, cx);
                        }
                    })),
            )
            .child(scroll_frame(
                "settings-scrollbar",
                &self.settings_scroll,
                div()
                    .id(("settings-panel", selected as usize))
                    .track_scroll(&self.settings_scroll)
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .w_full()
                    .overflow_y_scroll()
                    .child(content.flex_shrink_0()),
            ))
    }

    fn settings_appearance_card(&self, cx: &mut Context<Self>) -> Div {
        let p = palette(cx);
        let settings = self.snapshot.settings.clone();
        card(&p)
            .child(section_title(
                &p,
                self.tr("外观", "Appearance"),
                self.tr(
                    "语言、主题、窗口材质与动效",
                    "Language, theme, window material and motion",
                ),
            ))
            .child(setting_row(
                &p,
                self.tr("语言", "Language"),
                "",
                ChoiceGroup::new(
                    "language",
                    vec!["简体中文".into(), "English".into()],
                    usize::from(settings.language == Language::En),
                )
                .on_change(cx.listener(|this, index: &usize, window, cx| {
                    this.service.set_language(if *index == 0 {
                        Language::Zh
                    } else {
                        Language::En
                    });
                    this.sync(cx);
                    this.localize_inputs(window, cx);
                    crate::app_menu::sync(this.lang(), cx);
                })),
            ))
            .child(setting_row(
                &p,
                self.tr("主题", "Theme"),
                "",
                ChoiceGroup::new(
                    "theme",
                    vec![
                        self.tr("跟随系统", "System"),
                        self.tr("浅色", "Light"),
                        self.tr("深色", "Dark"),
                    ],
                    match settings.theme {
                        ThemeMode::System => 0,
                        ThemeMode::Light => 1,
                        ThemeMode::Dark => 2,
                    },
                )
                .on_change(cx.listener(|this, index: &usize, window, cx| {
                    let mode = [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark][*index];
                    this.service.set_theme(mode);
                    match mode {
                        ThemeMode::System => {
                            crate::window_effects::sync_system_appearance(window, cx)
                        }
                        _ => {
                            crate::window_effects::set_follow_system(false);
                            Theme::change(
                                if mode == ThemeMode::Light {
                                    ComponentThemeMode::Light
                                } else {
                                    ComponentThemeMode::Dark
                                },
                                Some(window),
                                cx,
                            );
                        }
                    }
                    crate::window_effects::make_surface_translucent(cx);
                    crate::window_effects::refresh_all(this.service.settings().backdrop, cx);
                    this.sync(cx);
                })),
            ))
            .when(backdrop_setting_available(), |card| {
                let choices = backdrop_choices();
                let selected = choices
                    .iter()
                    .position(|(style, _, _)| *style == settings.backdrop)
                    .unwrap_or(0);
                card.child(setting_row(
                    &p,
                    self.tr("窗口材质", "Backdrop"),
                    self.tr(
                        "同时应用到主窗口和浮窗",
                        "Applies to the main window and popup",
                    ),
                    ChoiceGroup::new(
                        "backdrop",
                        choices.iter().map(|(_, zh, en)| self.tr(zh, en)).collect(),
                        selected,
                    )
                    .on_change(cx.listener(
                        move |this, index: &usize, _, cx| {
                            this.service.set_backdrop(choices[*index].0);
                            crate::window_effects::refresh_all(
                                this.service.settings().backdrop,
                                cx,
                            );
                            this.sync(cx);
                        },
                    )),
                ))
            })
            .child(setting_row(
                &p,
                self.tr("界面动效", "Motion"),
                self.tr(
                    "控制展开过渡、弹窗与指示器动画；默认跟随系统减少动态效果设置",
                    "Controls expand transitions, dialogs and indicator motion; follows system reduced motion by default",
                ),
                ChoiceGroup::new(
                    "motion",
                    vec![
                        self.tr("跟随系统", "System"),
                        self.tr("开启", "On"),
                        self.tr("关闭", "Off"),
                    ],
                    match settings.motion {
                        MotionMode::System => 0,
                        MotionMode::On => 1,
                        MotionMode::Off => 2,
                    },
                )
                .on_change(cx.listener(|this, index: &usize, _, cx| {
                    let mode = [MotionMode::System, MotionMode::On, MotionMode::Off][*index];
                    this.service.set_motion(mode);
                    super::expand_motion::apply_motion_mode(mode, cx);
                    this.sync(cx);
                })),
            ))
    }

    fn settings_core_card(&self, cx: &mut Context<Self>) -> Div {
        let p = palette(cx);
        let lang = self.lang();
        let settings = self.snapshot.settings.clone();
        let binaries = self.snapshot.binaries.clone();
        let updates = self.snapshot.updates.clone();
        let is_installing = self.snapshot.busy.iter().any(|b| b == "install");
        let mut core_card = card(&p)
            .child(section_title(
                &p,
                self.tr("内核", "Core"),
                self.tr(
                    "内核选择、版本与运行选项",
                    "Core selection, versions and runtime options",
                ),
            ))
            .child(setting_row(
                &p,
                self.tr("内核", "Core"),
                reload_note(lang, settings.hot_reload),
                ChoiceGroup::new(
                    "core",
                    CoreKind::all()
                        .iter()
                        .map(|c| c.display_name().into())
                        .collect(),
                    usize::from(settings.core == CoreKind::SingBox),
                )
                .on_change(cx.listener(|this, index: &usize, _, cx| {
                    this.service.set_core(CoreKind::all()[*index]);
                    this.sync(cx);
                })),
            ))
            .child(setting_row(
                &p,
                self.tr("日志级别", "Log level"),
                reload_note(lang, settings.hot_reload),
                ChoiceGroup::new(
                    "log-level",
                    vec!["Debug".into(), "Info".into(), "Warn".into(), "Error".into()],
                    [
                        LogLevel::Debug,
                        LogLevel::Info,
                        LogLevel::Warn,
                        LogLevel::Error,
                    ]
                    .iter()
                    .position(|l| *l == settings.log_level)
                    .unwrap_or(1),
                )
                .on_change(cx.listener(|this, index: &usize, _, cx| {
                    this.modify_settings(cx, |s| {
                        s.log_level = [
                            LogLevel::Debug,
                            LogLevel::Info,
                            LogLevel::Warn,
                            LogLevel::Error,
                        ][*index]
                    })
                })),
            ))
            .child(setting_row(
                &p,
                self.tr("热重载", "Hot reload"),
                self.tr("保存网络配置后自动重载内核，连接可能短暂中断。应用更新后按需更新已安装的提权助手。", "Automatically reload saved network changes. Connections may be interrupted briefly. Update the installed privilege helper when an app update requires it."),
                toggle("hot-reload", self.tr("启用热重载", "Enable hot reload"), settings.hot_reload)
                    .on_change(cx.listener(|this, checked, _, cx| this.modify_settings(cx, |s| s.hot_reload = *checked))),
            ))
            .child(setting_row(
                &p,
                self.tr("故障恢复", "Recovery"),
                "",
                toggle(
                    "watchdog",
                    self.tr("崩溃自动重启", "Auto-restart on crash"),
                    settings.watchdog,
                )
                .on_change(cx.listener(|this, checked, _, cx| {
                    this.modify_settings(cx, |s| s.watchdog = *checked)
                })),
            ))
            .child(divider(&p));

        for (idx, core) in [CoreKind::SingBox, CoreKind::Xray].into_iter().enumerate() {
            if idx > 0 {
                core_card = core_card.child(divider(&p));
            }
            let is_engine = settings.core == core;
            let active_version = settings.active_version(core).cloned();
            let core_binaries: Vec<_> = binaries
                .iter()
                .filter(|b| b.core == core && b.matches_platform())
                .cloned()
                .collect();
            let core_title = core.display_name();

            let mut section = div().flex().flex_col().gap_2();
            section = section.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.fg)
                                    .child(core_title),
                            )
                            .when_some(
                                updates
                                    .cores
                                    .get(core.as_str())
                                    .and_then(|c| c.candidate.as_ref()),
                                |d, candidate| {
                                    d.child(pill(
                                        format!(
                                            "{} {}",
                                            self.tr("可更新至", "Update to"),
                                            candidate.version
                                        ),
                                        p.info,
                                        p.info.opacity(0.1),
                                    ))
                                },
                            )
                            .when(is_engine, |d| {
                                d.child(pill(
                                    self.tr("当前选用", "Selected"),
                                    p.primary_fg,
                                    p.primary,
                                ))
                            })
                            .child(div().text_xs().text_color(p.muted_fg).child(format!(
                                "{}: {}",
                                self.tr("当前来源", "Active source"),
                                if settings.binary_override(core).is_some() {
                                    self.tr("自定义路径", "Custom path").to_string()
                                } else {
                                    active_version.clone().unwrap_or_else(|| {
                                        self.tr("系统 PATH", "System PATH").to_string()
                                    })
                                }
                            ))),
                    )
                    .child(
                        Button::new(SharedString::from(format!("install-{core:?}")))
                            .compact()
                            .h(px(crate::ui::theme::CONTROL_HEIGHT))
                            .primary()
                            .label(self.tr("下载 / 更新", "Download / update"))
                            .loading(is_installing && settings.core == core)
                            .on_click(cx.listener(move |this, _, _, _| {
                                this.service.install_core_update(core);
                            })),
                    ),
            );

            section = section.child(self.local_binary_row(core, &p, cx));
            let busy = is_installing || self.snapshot.busy.iter().any(|key| key == "core");
            for record in core_binaries {
                let version = record.version;
                let selected = active_version.as_ref() == Some(&version);
                let title = format!("v{version}");
                let row = self.binary_source_row(
                    core,
                    Some(version.clone()),
                    selected,
                    !busy,
                    format!("{} {title}", core.display_name()).into(),
                    &p,
                    cx,
                );
                section = section.child(
                    row.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().flex().items_center().gap_2().child(title).child(pill(
                                format!("{}/{}", record.platform, record.arch),
                                p.muted_fg,
                                p.panel_alt,
                            )))
                            .child(div().text_xs().text_color(p.muted_fg).child(format!(
                                "{} · sha256 {}…",
                                xrose_core::util::format_bytes(record.size),
                                &record.sha256[..12.min(record.sha256.len())]
                            ))),
                    )
                    .child(
                        danger_icon_button(
                            SharedString::from(format!("remove-version-{core:?}-{version}")),
                            IconName::Trash,
                            self.tr("删除此版本", "Delete version"),
                            cx,
                        )
                        .disabled(busy)
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.run_command(
                                    super::menus::Command::DeleteVersion(core, version.clone()),
                                    false,
                                    window,
                                    cx,
                                );
                            },
                        )),
                    ),
                );
            }

            core_card = core_card.child(section);
        }

        core_card
    }

    fn binary_source_row(
        &self,
        core: CoreKind,
        version: Option<String>,
        selected: bool,
        enabled: bool,
        label: SharedString,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let id = match &version {
            Some(v) => format!("managed-{core:?}-{v}"),
            None => format!("local-{core:?}"),
        };
        let view = cx.entity().downgrade();
        let select = move |cx: &mut App| {
            let _ = view.update(cx, |this, cx| {
                let result = match &version {
                    Some(v) => this.service.set_active_version(core, v),
                    None => this.service.select_local_binary(core),
                };
                if let Err(error) = result {
                    this.service.toast(ToastLevel::Error, error.to_string());
                }
                this.sync(cx);
            });
        };
        let select_radio = select.clone();
        radio_row_surface(
            SharedString::from(format!("binary-{id}")),
            selected,
            enabled,
            p,
        )
        .when(enabled && !selected, |row| {
            row.on_click(move |_, _, cx| select(cx))
        })
        .child(
            row_radio(
                SharedString::from(format!("radio-{id}")),
                label,
                selected,
                enabled,
                p,
            )
            .on_change(move |_, _, _, cx| {
                cx.stop_propagation();
                select_radio(cx);
            }),
        )
    }

    fn local_binary_row(
        &self,
        core: CoreKind,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let settings = &self.snapshot.settings;
        let status = self
            .snapshot
            .local_binaries
            .get(core.as_str())
            .cloned()
            .unwrap_or_default();
        let custom = settings.local_binary_path(core);
        let selected = settings.active_version(core).is_none();
        let busy = self
            .snapshot
            .busy
            .iter()
            .any(|key| key == "core" || key == "install");
        let path = status.path.as_ref().map(|p| p.display().to_string());
        let path_text = path.clone().unwrap_or_else(|| {
            format!(
                "{} {}{}",
                self.tr("PATH 中未找到", "Not found on PATH:"),
                core.binary_name(),
                std::env::consts::EXE_SUFFIX
            )
        });
        let label = self.tr("自定义路径", "Custom path");
        let row = self.binary_source_row(
            core,
            None,
            selected,
            !busy && status.available,
            format!("{} · {label}", core.display_name()).into(),
            p,
            cx,
        );
        let mut title = div().flex().items_center().gap_2().child(label);
        if let Some(info) = status.info {
            title = title
                .child(
                    div().text_xs().text_color(p.muted_fg).child(
                        info.version
                            .map(|v| format!("v{v}"))
                            .unwrap_or_else(|| self.tr("版本未知", "Unknown version").to_string()),
                    ),
                )
                .child(pill(
                    info.architecture
                        .unwrap_or_else(|| self.tr("架构未知", "Unknown architecture").to_string()),
                    p.muted_fg,
                    p.panel_alt,
                ));
        } else if status.available {
            title = title.child(
                div()
                    .text_xs()
                    .text_color(p.muted_fg)
                    .child(self.tr("正在识别…", "Detecting…")),
            );
        }
        let browse = self.tr("选择可执行文件", "Choose executable file");
        let clear = self.tr(
            "清除自定义路径，恢复系统 PATH",
            "Clear custom path and use system PATH",
        );
        row.child(
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .child(title)
                .child(
                    div()
                        .id(SharedString::from(format!("binary-path-{core:?}")))
                        .truncate()
                        .text_xs()
                        .text_color(if status.available {
                            p.muted_fg
                        } else {
                            p.warn_text
                        })
                        .when_some(path, |d, path| {
                            d.tooltip(move |window, cx| {
                                Tooltip::new(path.clone()).build(window, cx)
                            })
                        })
                        .child(path_text),
                )
                .when(custom.is_some() && !status.available, |d| {
                    d.child(div().text_xs().text_color(p.warn_text).child(
                        self.tr("文件不存在或不可执行", "File is missing or not executable"),
                    ))
                }),
        )
        .child(
            div()
                .id(SharedString::from(format!("binary-file-actions-{core:?}")))
                .flex()
                .items_center()
                .gap_2()
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(
                    icon_button(
                        SharedString::from(format!("browse-binary-{core:?}")),
                        IconName::FolderOpen,
                        browse,
                        cx,
                    )
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.pick_core_binary(core, cx);
                    })),
                )
                .child(
                    icon_button(
                        SharedString::from(format!("clear-binary-{core:?}")),
                        IconName::Eraser,
                        clear,
                        cx,
                    )
                    .disabled(busy || custom.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        if let Err(err) = this.service.set_binary_override(core, None) {
                            this.service.toast(ToastLevel::Error, err.to_string());
                        }
                        this.sync(cx);
                    })),
                ),
        )
    }

    fn pick_core_binary(&self, core: CoreKind, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(
                format!(
                    "{} {}",
                    self.tr("选择可执行文件", "Choose executable for"),
                    core.display_name()
                )
                .into(),
            ),
        });
        let service = self.service.clone();
        let picker_error = self.tr(
            "无法打开文件选择器，请重试",
            "Could not open the file picker. Please retry.",
        );
        let encoding_error = self.tr(
            "请选择路径可用 UTF-8 表示的文件",
            "Choose a file with a UTF-8 path",
        );
        cx.spawn(async move |_, _| match prompt.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.first() {
                    match path.to_str() {
                        Some(path) => {
                            if let Err(error) = service.set_binary_override(core, Some(path.into()))
                            {
                                service.toast(ToastLevel::Error, error.to_string());
                            }
                        }
                        None => service.toast(ToastLevel::Error, encoding_error.to_string()),
                    }
                }
            }
            Ok(Ok(None)) => {}
            _ => service.toast(ToastLevel::Error, picker_error.to_string()),
        })
        .detach();
    }

    fn settings_network_card(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let p = palette(cx);
        let lang = self.lang();
        let settings = self.snapshot.settings.clone();
        let port_raw = self.port_input.read(cx).value().to_string();
        let proxy_busy = self.snapshot.busy.iter().any(|key| key == "proxy");
        let proxy_hint = self
            .snapshot
            .system_proxy_status
            .filter(|_| !proxy_busy)
            .and_then(|status| crate::i18n::proxy_status_label(self.lang(), status))
            .unwrap_or_else(|| {
                self.tr(
                    "供遵循系统代理设置的应用使用",
                    "For applications that use the system proxy settings",
                )
            });
        let port_dirty = port_raw.trim() != settings.mixed_port.to_string();
        let port_valid = valid_port(&port_raw);
        let open_proxy_settings = icon_button(
            "open-system-proxy-settings",
            IconName::ExternalLink,
            self.tr("打开系统代理设置", "Open system proxy settings"),
            cx,
        )
        .on_click(cx.listener(|this, _, _, _| this.service.open_system_proxy_settings()));
        let copy_proxy_env = icon_button(
            "copy-proxy-env",
            IconName::Copy,
            self.tr("复制代理环境变量命令", "Copy proxy environment commands"),
            cx,
        )
        .disabled(port_dirty)
        .when(port_dirty, |button| {
            button.tooltip(self.tr("请先应用端口修改", "Apply the port change first"))
        })
        .on_click(cx.listener(|this, _, _, cx| this.copy_proxy_env_commands(cx)));
        let advanced_sample = self.expand_motion.sample(
            "settings-network-advanced",
            self.network_advanced_open,
            window,
            cx,
        );
        let advanced_toggle = Button::new("network-advanced")
            .ghost()
            .compact()
            .label(self.tr("高级设置", "Advanced settings"))
            .accessibility_label(if self.network_advanced_open {
                self.tr("收起高级设置", "Collapse advanced settings")
            } else {
                self.tr("展开高级设置", "Expand advanced settings")
            })
            .icon(
                Icon::new(IconName::ChevronRight)
                    .rotate(percentage(0.25 * advanced_sample.progress)),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.network_advanced_open = !this.network_advanced_open;
                cx.notify();
            }));
        card(&p)
            .child(section_title(&p, self.tr("网络与代理", "Network & proxy"), self.tr("连接范围、流量接管与本地端口", "Access scope, traffic routing and local ports")))
            .when(self.snapshot.model.work_mode == xrose_core::model::WorkMode::Mesh, |card| {
                card.child(div().text_sm().text_color(p.muted_fg).child(self.tr("组网模式自动接管系统流量。代理模式的接管设置会保留。", "Mesh mode routes system traffic automatically. Proxy mode settings are preserved.")))
                    .child(setting_row(&p, self.tr("局域网访问", "LAN access"), self.tr("允许组网模式直接访问本机所在局域网", "Allow direct access to the local LAN in Mesh mode"),
                        toggle("mesh-allow-lan", self.tr("允许局域网访问", "Allow LAN access"), self.snapshot.model.mesh_mode.allow_lan)
                            .on_change(cx.listener(|this, checked, _, cx| {
                                if let Err(error) = this.service.set_mesh_allow_lan(*checked) {
                                    this.service.toast(xrose_core::service::ToastLevel::Error, error.to_string());
                                }
                                this.sync(cx);
                            }))))
            })
            .child(setting_row(&p, "TUN", with_reload_note(lang, settings.hot_reload, "接管系统网络流量，需要提权助手", "Route system traffic through the proxy (requires the privilege helper)"),
                toggle("tun", self.tr("启用 TUN", "Enable TUN"), settings.tun_enabled || self.snapshot.model.work_mode == xrose_core::model::WorkMode::Mesh).disabled(self.snapshot.model.work_mode == xrose_core::model::WorkMode::Mesh).on_change(cx.listener(|this, checked, _, cx| { this.service.set_tun_enabled(*checked); this.sync(cx); }))))
            .child(setting_row(&p, self.tr("严格路由", "Strict routing"), with_reload_note(lang, settings.hot_reload, "限制流量绕过 TUN", "Restrict traffic from bypassing TUN"),
                toggle("tun-strict-route", self.tr("启用严格路由", "Enable strict routing"), settings.tun_strict_route).on_change(cx.listener(|this, checked, _, cx| this.modify_settings(cx, |s| s.tun_strict_route = *checked)))))
            .child(setting_row(&p, self.tr("系统代理", "System proxy"), proxy_hint,
                div().flex().items_center().gap_2()
                    .child(toggle("system-proxy", self.tr("由 Xrose 设置系统代理", "Let Xrose manage the system proxy"), self.snapshot.system_proxy_active)
                        .disabled(proxy_busy || self.snapshot.model.work_mode == xrose_core::model::WorkMode::Mesh).on_change(cx.listener(|this, checked, _, cx| { this.service.set_system_proxy(*checked); this.sync(cx); })))
                    .child(open_proxy_settings)
                    .when(proxy_busy, |row| row.child(gpui_kit::component::spinner::Spinner::new()).child(div().text_xs().text_color(p.muted_fg).child(self.tr("正在应用…", "Applying…"))))))
            .child(setting_row(&p, "PAC", self.tr("根据路由规则自动选择代理", "Choose the proxy automatically using routing rules"),
                toggle("pac", self.tr("启用 PAC", "Enable PAC"), settings.pac_enabled).disabled(proxy_busy || self.snapshot.model.work_mode == xrose_core::model::WorkMode::Mesh).on_change(cx.listener(|this, checked, _, cx| { this.modify_settings(cx, |s| s.pac_enabled = *checked); this.service.reapply_system_proxy(); }))))
            .child(setting_row(&p, self.tr("混合端口", "Mixed port"), with_reload_note(lang, settings.hot_reload, "修改后应用", "Apply after editing"),
                div().flex().items_center().gap_2().child(div().w(px(super::theme::SETTING_LABEL_WIDTH)).child(Input::new(&self.port_input)))
                    .child(copy_proxy_env)
                    .when(port_dirty, |d| d.child(Button::new("save-ports").primary().label(self.tr("应用", "Apply")).disabled(port_valid.is_none())
                        .on_click(cx.listener(|this, _, _, cx| { if let Some(port) = valid_port(this.port_input.read(cx).value().as_ref()) { this.modify_settings(cx, |s| s.mixed_port = port); } }))))))
            .when(port_dirty && port_valid.is_none(), |d| d.child(div().text_sm().text_color(p.danger_text).child(self.tr("端口必须是 1–65535", "Enter a port from 1 to 65535"))))
            .when(settings.core == CoreKind::Xray, |d| d.child(div().text_xs().text_color(p.muted_fg).child(self.tr("Xray：SOCKS 使用此端口，HTTP 使用下一端口", "Xray: SOCKS uses this port, HTTP uses the next port"))))
            .child(setting_row(&p, self.tr("局域网", "LAN"), with_reload_note(lang, settings.hot_reload, "允许同一局域网的设备使用本机代理", "Allow devices on your LAN to use this proxy"),
                toggle("allow-lan", self.tr("允许局域网连接", "Allow LAN connections"), settings.allow_lan).on_change(cx.listener(|this, checked, _, cx| this.modify_settings(cx, |s| s.allow_lan = *checked)))))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .child(div().flex().child(advanced_toggle))
                    .when(advanced_sample.mounted, |section| {
                        section.child(advanced_sample.wrap(
                            "settings-network-advanced-reveal",
                            div()
                                .pt_3()
                                .w_full()
                                .child(self.advanced_network_settings(&p, cx)),
                        ))
                    }),
            )
    }

    fn settings_startup_card(&self, cx: &mut Context<Self>) -> Div {
        let p = palette(cx);
        let settings = self.snapshot.settings.clone();
        card(&p)
            .child(section_title(
                &p,
                self.tr("启动与窗口", "Startup & window"),
                self.tr(
                    "启动行为、常驻入口与通知",
                    "Startup behavior, background access and notifications",
                ),
            ))
            .child(setting_row(
                &p,
                self.tr("登录启动", "Login"),
                "",
                toggle(
                    "autostart",
                    self.tr("登录时启动应用", "Launch at login"),
                    settings.autostart,
                )
                .on_change(cx.listener(|this, checked, _, cx| {
                    this.modify_settings(cx, |s| s.autostart = *checked)
                })),
            ))
            .child(setting_row(
                &p,
                self.tr("启动代理", "Proxy startup"),
                "",
                toggle(
                    "autostart-proxy",
                    self.tr("应用启动时开启代理", "Start proxy on app launch"),
                    settings.startup_proxy,
                )
                .on_change(cx.listener(|this, checked, _, cx| {
                    this.modify_settings(cx, |s| s.startup_proxy = *checked)
                })),
            ))
            .children(
                self.snapshot
                    .mesh
                    .profiles
                    .iter()
                    .filter(|_| self.snapshot.mesh.supported)
                    .enumerate()
                    .map(|(index, profile)| {
                        let id = profile.id.clone();
                        let provider = profile.provider;
                        setting_row(
                            &p,
                            if index == 0 {
                                self.tr("启动组网", "Mesh startup")
                            } else {
                                SharedString::default()
                            },
                            "",
                            toggle(
                                SharedString::from(format!("login-mesh-{id}")),
                                format!("{} · {}", provider.label(), profile.name),
                                settings.mesh_startup_profile(provider) == Some(id.as_str()),
                            )
                            .on_change(cx.listener(
                                move |this, checked, _, cx| {
                                    this.modify_settings(cx, |s| {
                                        s.set_mesh_startup_profile(provider, &id, *checked)
                                    })
                                },
                            )),
                        )
                    }),
            )
            .child(setting_row(
                &p,
                self.tr("启动窗口", "Launch window"),
                "",
                toggle(
                    "start-minimized",
                    self.tr("启动时最小化", "Start minimized"),
                    settings.start_minimized,
                )
                .on_change(cx.listener(|this, checked, _, cx| {
                    this.modify_settings(cx, |s| s.start_minimized = *checked)
                })),
            ))
            .when(crate::tray::available(), |d| {
                d.child(setting_row(
                    &p,
                    self.tr("关闭窗口", "Close window"),
                    "",
                    toggle(
                        "close-to-tray",
                        self.tr(
                            if cfg!(target_os = "macos") {
                                "关闭到菜单栏"
                            } else {
                                "关闭到系统托盘"
                            },
                            if cfg!(target_os = "macos") {
                                "Close to menu bar"
                            } else {
                                "Close to system tray"
                            },
                        ),
                        settings.close_to_tray,
                    )
                    .on_change(cx.listener(|this, checked, _, cx| {
                        let was_enabled = this.service.settings().close_to_tray;
                        this.modify_settings(cx, |s| s.close_to_tray = *checked);
                        if !was_enabled && this.snapshot.settings.close_to_tray {
                            let message = if cfg!(target_os = "macos") {
                                this.tr("关闭窗口后，Xrose 将继续运行。可从菜单栏重新打开主窗口。", "Xrose will keep running when you close the window. Reopen it from the menu bar.")
                            } else {
                                this.tr("关闭窗口后，Xrose 将继续运行。可从系统托盘重新打开主窗口。", "Xrose will keep running when you close the window. Reopen it from the system tray.")
                            };
                            this.service.toast(xrose_core::service::ToastLevel::Info, message);
                        }
                    })),
                ))
                .child(setting_row(
                    &p,
                    self.tr("网速显示", "Traffic speed"),
                    "",
                    toggle(
                        "show-tray-speed",
                        self.tr(
                            if cfg!(target_os = "macos") {
                                "菜单栏显示网速"
                            } else {
                                "系统托盘显示网速"
                            },
                            if cfg!(target_os = "macos") {
                                "Show speed in menu bar"
                            } else {
                                "Show speed in system tray"
                            },
                        ),
                        settings.show_tray_speed,
                    )
                    .on_change(cx.listener(|this, checked, _, cx| {
                        this.modify_settings(cx, |s| s.show_tray_speed = *checked)
                    })),
                ))
            })
            .child(setting_row(
                &p,
                self.tr("通知", "Notifications"),
                "",
                toggle(
                    "system-notifications",
                    self.tr("系统通知", "System notifications"),
                    settings.system_notifications,
                )
                .on_change(cx.listener(|this, checked, _, cx| {
                    this.modify_settings(cx, |s| s.system_notifications = *checked)
                })),
            ))
    }

    fn settings_updates_card(&self, cx: &mut Context<Self>) -> Div {
        let p = palette(cx);
        let settings = self.snapshot.settings.clone();
        let updates = self.snapshot.updates.clone();
        let core_check = updates.cores.get(settings.core.as_str());
        let app_updates_available = self.service.project_repository().is_some();
        let startup_check_text = if app_updates_available {
            self.tr(
                "启动时检查应用和当前内核",
                "Check the app and selected core at startup",
            )
        } else {
            self.tr("启动时检查当前内核", "Check the selected core at startup")
        };
        let is_checking_updates = self.snapshot.busy.iter().any(|b| b.starts_with("updates-"));
        let input = self.update_proxy_input.read(cx).value();
        let proxy = xrose_core::updates::Socks5Proxy::parse(input.as_ref());
        let agent = self.update_agent_input.read(cx).value().trim().to_string();
        card(&p)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(section_title(
                            &p,
                            self.tr("更新与备份", "Updates & backups"),
                            self.tr(
                                "检查更新，以及备份、导出或导入配置",
                                "Check for updates, and back up, export, or import configuration",
                            ),
                        ))
                        .child(
                            Button::new("check-updates")
                                .label(self.tr("检查更新", "Check updates"))
                                .loading(is_checking_updates)
                                .on_click(
                                    cx.listener(|this, _, _, _| this.service.check_updates()),
                                ),
                        ),
                )
                .child(setting_row(&p, self.tr("自动检查更新", "Automatic update checks"), startup_check_text,
                    toggle("check-startup-updates", self.tr("启动时检查", "Check at startup"), settings.check_app_updates)
                        .on_change(cx.listener(|this, enabled, _, cx| this.modify_settings(cx, |s| s.check_app_updates = *enabled)))))
                .child(setting_row(&p, self.tr("节点订阅", "Subscriptions"), "",
                    toggle("proxy-sub-updates", self.tr("使用代理更新订阅", "Update subscriptions through the proxy"), settings.proxy_subscription_updates)
                        .on_change(cx.listener(|this, enabled, _, cx| this.modify_settings(cx, |s| s.proxy_subscription_updates = *enabled)))))
                .child({
                    let current = xrose_core::subscription::Subscription::clamp_interval_hours(
                        settings.subscription_update_hours,
                    );
                    let mut values = super::subscriptions::SUBSCRIPTION_INTERVAL_PRESETS.to_vec();
                    if !values.contains(&current) {
                        values.push(current);
                    }
                    let selected = values.iter().position(|&h| h == current);
                    let labels = values
                        .iter()
                        .map(|&h| super::subscriptions::interval_preset_label(h, self.lang()))
                        .collect();
                    setting_row(
                        &p,
                        self.tr("更新间隔", "Refresh interval"),
                        self.tr(
                            "开启自动更新的订阅默认按此间隔刷新；单条订阅可单独自定义覆盖。",
                            "Default interval for auto-updating subscriptions. Per-subscription overrides take precedence.",
                        ),
                        ChoiceGroup::new("settings-sub-interval", labels, selected)
                            .small()
                            .on_change(cx.listener(move |this, index: &usize, _, cx| {
                                if let Some(&hours) = values.get(*index) {
                                    this.modify_settings(cx, |s| {
                                        s.subscription_update_hours = hours;
                                    });
                                }
                            })),
                    )
                })
                .child(setting_row(&p, self.tr("资源下载", "Resources"),
                    self.tr("适用于版本检查、内核、规则集及组网组件", "Applies to release checks, cores, rule sets and mesh components."),
                    toggle("proxy-resource-updates", self.tr("使用代理更新资源", "Update resources through the proxy"), settings.proxy_resource_updates)
                        .on_change(cx.listener(|this, enabled, _, cx| this.modify_settings(cx, |s| s.proxy_resource_updates = *enabled)))))
            .child(setting_row(&p, self.tr("前置代理", "Front proxy"),
                self.tr("用于已启用代理的订阅和资源更新。留空使用运行中的内核。", "SOCKS5 for proxied subscription and resource updates. Leave blank to use the running core."),
                div().flex().items_center().gap_2()
                    .child(div().w(px(super::theme::SETTING_LABEL_WIDTH * 2.)).child(Input::new(&self.update_proxy_input)))
                    .when(input.trim() != settings.update_proxy.as_str(), |d| d.child(Button::new("save-update-proxy")
                        .label(self.tr("应用", "Apply")).disabled(proxy.is_err())
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Ok(proxy) = xrose_core::updates::Socks5Proxy::parse(this.update_proxy_input.read(cx).value().as_ref()) {
                                this.modify_settings(cx, |s| s.update_proxy = proxy);
                            }
                        }))))
                    .when(proxy.is_err(), |d| d.child(div().text_sm().text_color(p.danger_text)
                        .child(self.tr("请输入 SOCKS5 地址和端口", "Enter a SOCKS5 address and port"))))))
                .child(setting_row(&p, "User-Agent",
                    self.tr("用于订阅和资源请求，留空使用默认值", "Used for subscription and resource requests. Leave blank for the default."),
                    div().flex().items_center().gap_2().child(div().w(px(super::theme::SETTING_LABEL_WIDTH * 2.)).child(Input::new(&self.update_agent_input)))
                        .when(agent != settings.update_user_agent, |d| d.child(Button::new("save-agent").label(self.tr("应用", "Apply"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let agent = this.update_agent_input.read(cx).value().trim().to_string();
                                this.modify_settings(cx, |s| s.update_user_agent = agent);
                            }))))))
                .child(divider(&p))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(p.fg)
                                        .child(self.tr("内核更新状态", "Core update status")),
                                )
                                .child(match core_check.and_then(|c| c.candidate.as_ref()) {
                                    Some(candidate) => {
                                        let label = format!(
                                            "{} {}",
                                            candidate.core.display_name(),
                                            candidate.version
                                        );
                                        let core = candidate.core;
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(pill(label, p.primary_fg, p.success))
                                            .child(
                                                Button::new("install-candidate")
                                                    .compact()
                                                    .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                                    .primary()
                                                    .label(self.tr("安装", "Install"))
                                                    .on_click(cx.listener(move |this, _, _, _| {
                                                        this.service.install_core_update(core);
                                                    })),
                                            )
                                    }
                                    None => div()
                                        .text_xs()
                                        .text_color(p.muted_fg)
                                        .child(if core_check.is_some_and(|c| c.error.is_some()) {
                                            self.tr("检查失败", "Check failed")
                                        } else if core_check.is_none() {
                                            if self.snapshot.core_binary_available(settings.core) {
                                                self.tr("尚未检查", "Not checked")
                                            } else {
                                                self.tr("未安装", "Not installed")
                                            }
                                        } else { self.tr("已是最新版本", "Up to date") }),
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(div().text_sm().text_color(p.fg).child(
                                    self.tr("Xrose 更新状态", "Xrose update status"),
                                ))
                                .child(match &updates.app {
                                    Some(app) => {
                                        let url = app.url.clone();
                                        let version = app.version.clone();
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(pill(
                                                format!("App {version}"),
                                                p.primary_fg,
                                                p.info,
                                            ))
                                            .child(
                                                Button::new("open-release")
                                                    .compact()
                                                    .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                                    .label(self.tr("打开下载页", "Open download"))
                                                    .on_click(cx.listener(move |this, _, _, _| {
                                                        this.service.open_url(&url);
                                                    })),
                                            )
                                    }
                                    None => div()
                                        .text_xs()
                                        .text_color(p.muted_fg)
                                        .child(if !app_updates_available { self.tr("尚未开放", "Not yet available") }
                                        else if updates.error.is_some() { self.tr("检查失败", "Check failed") }
                                        else if updates.checked_at.is_none() { self.tr("尚未检查", "Not checked") }
                                        else { self.tr("已是最新版本", "Up to date") }),
                                }),
                        )
                        .when_some(core_check.and_then(|c| c.error.clone()), |d, error| {
                            d.child(div().text_xs().text_color(p.warn_text).child(error))
                        })
                        .when(updates.error.is_some(), |d| {
                            d.child(
                                div()
                                    .text_xs()
                                    .text_color(p.warning)
                                    .child(updates.error.clone().unwrap_or_default()),
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            Button::new("backup-db")
                                .compact()
                                .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                .label(self.tr("备份数据库", "Backup database"))
                                .on_click(cx.listener(|this, _, _, _| {
                                    let _ = this.service.backup_database();
                                })),
                        )
                        .child(
                            Button::new("export-backup")
                                .compact()
                                .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                .label(self.tr("导出 JSON 备份", "Export JSON"))
                                .on_click(cx.listener(|this, _, _, _| {
                                    let _ = this.service.export_backup();
                                })),
                        )
                        .child(
                            Button::new("import-config")
                                .compact()
                                .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                .label(self.tr("导入配置…", "Import config…"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_config_import_dialog(window, cx);
                                })),
                        )
                        .child(
                            Button::new("open-data")
                                .compact()
                                .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                .ghost()
                                .label(self.tr("打开数据目录", "Open data dir"))
                                .on_click(cx.listener(|this, _, _, _| {
                                    let path = this.snapshot.platform.data_dir.clone();
                                    this.service.open_url(&format!("file://{path}"));
                                })),
                        ),
                )
    }

    pub(super) fn settings_helper_card(&self, cx: &mut Context<Self>) -> Div {
        let p = palette(cx);
        let platform = self.snapshot.platform.clone();
        let doctor = self.snapshot.doctor.clone();
        let helper_busy = self.snapshot.busy.iter().any(|b| b == "helper");
        let helper_label = if platform.helper_needs_update {
            self.tr("需要更新", "Update required")
        } else if platform.helper_available {
            self.tr("运行中", "Running")
        } else if platform.helper_installed {
            self.tr("已安装，未运行", "Installed, not running")
        } else {
            self.tr("未安装", "Not installed")
        };
        let helper_color = if platform.helper_needs_update {
            p.warn_text
        } else if platform.helper_available {
            p.success
        } else if platform.helper_installed {
            p.warning
        } else {
            p.muted_fg
        };
        let doctor_busy = self.snapshot.busy.iter().any(|key| key == "doctor");
        card(&p)
                .child(
                    div()
                        .flex()
                        .items_start()
                        .justify_between()
                        .child(section_title(
                            &p,
                            self.tr("提权助手与环境检查", "Privilege helper & environment checks"),
                            self.tr(
                                "后台提权服务管理，以及 TUN、端口、权限与系统代理环境诊断",
                                "Manage the background privilege helper and inspect TUN, ports, permissions, and proxy backend",
                            ),
                        ))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(dot(helper_color))
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.fg)
                                        .child(helper_label),
                                )
                                .child(pill(
                                    if platform.helper_supported {
                                        self.tr("受支持", "Supported")
                                    } else {
                                        self.tr("不支持", "Unsupported")
                                    },
                                    p.muted_fg,
                                    p.panel_alt,
                                )),
                        ),
                )
                .child({
                    let mut items: Vec<String> = vec![platform.helper_detail.clone()];
                    if !platform.elevated {
                        items.push(
                            self.tr(
                                "管理提权助手时，系统可能要求管理员授权",
                                "Managing the helper may require administrator approval",
                            )
                            .to_string(),
                        );
                    }
                    items.push(
                        self.tr(
                            "TUN 模式需要安装并启动提权助手代启内核",
                            "TUN mode requires the privilege helper to launch the core",
                        )
                        .to_string(),
                    );
                    if platform.helper_supported && platform.helper_installed {
                        items.push(self.tr(
                            "更新会重启提权助手，并中断由它管理的连接",
                            "Updating restarts the helper and interrupts connections it manages",
                        ).to_string());
                    }
                    if cfg!(windows) {
                        items.push(
                            self.tr(
                                "Windows 下 Wintun 驱动会在启动前自动校验部署",
                                "On Windows, Wintun is verified and deployed automatically before startup",
                            )
                            .to_string(),
                        );
                    }
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_xs()
                        .text_color(p.muted_fg)
                        .children(items.into_iter().map(|item| {
                            div()
                                .flex()
                                .items_start()
                                .gap_2()
                                .child(div().flex_shrink_0().child("•"))
                                .child(div().flex_1().min_w(px(0.)).child(item))
                        }))
                })
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .when(platform.helper_supported && !platform.helper_installed, |d| {
                            d.child(
                                Button::new("install-helper")
                                    .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                    .primary()
                                    .label(self.tr("安装并启动", "Install & start"))
                                    .loading(helper_busy).disabled(helper_busy)
                                    .on_click(cx.listener(|this, _, _, _| {
                                        this.service.install_helper();
                                    })),
                            )
                        })
                        .when(
                            platform.helper_supported
                                && platform.helper_installed
                                && !platform.helper_available,
                            |d| {
                                d.child(
                                    Button::new("start-helper")
                                        .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                        .primary()
                                        .label(self.tr("启动", "Start"))
                                        .loading(helper_busy).disabled(helper_busy)
                                        .on_click(cx.listener(|this, _, _, _| {
                                            this.service.start_helper();
                                        })),
                                )
                            },
                        )
                        .when(platform.helper_supported && platform.helper_installed, |d| {
                            d.child(
                                Button::new("restart-helper")
                                    .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                    .label(self.tr("重启", "Restart"))
                                    .loading(helper_busy).disabled(helper_busy)
                                    .on_click(cx.listener(|this, _, _, _| {
                                        this.service.restart_helper();
                                    })),
                            )
                            .child(
                                Button::new("uninstall-helper")
                                    .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                    .danger()
                                    .label(self.tr("卸载", "Uninstall"))
                                    .loading(helper_busy).disabled(helper_busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.run_command(super::menus::Command::UninstallHelper, false, window, cx);
                                    })),
                            )
                        })
                        .child(
                            Button::new("refresh-doctor")
                                .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                .loading(doctor_busy || helper_busy)
                                .label(self.tr("重新检查", "Re-check"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.service.refresh_helper_status();
                                    this.sync(cx);
                                })),
                        )
                        .when(platform.helper_supported && platform.helper_installed, |d| {
                            d.child(
                                Button::new("update-helper")
                                    .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                    .label(self.tr("更新助手", "Update helper"))
                                    .loading(helper_busy).disabled(helper_busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.service.update_helper();
                                        this.sync(cx);
                                    })),
                            )
                        }),
                )
                .when(!platform.helper_supported, |d| {
                    d.child(
                        div().text_xs().text_color(p.muted_fg).child(self.tr(
                            "该平台暂不支持提权助手；TUN 仍由内核自身权限处理",
                            "The helper is not available on this platform; TUN relies on the core's own rights",
                        )),
                    )
                })
                .child(divider(&p))
                .child({
                    let lang = self.snapshot.settings.language;
                    let colon = self.tr("：", ": ");
                    let privacy = self.snapshot.settings.privacy_mode;
                    let mut env_items = vec![
                        format!(
                            "{}{colon}{}",
                            self.tr("系统代理后端", "Proxy backend"),
                            platform.proxy_backend
                        ),
                        format!(
                            "{}{colon}{}",
                            self.tr("TUN 助手协议版本", "TUN helper protocol"),
                            xrose_core::system::privilege::format_protocol_version(
                                platform.helper_tun_protocol,
                                platform.helper_available,
                                platform.helper_needs_update,
                                platform.helper_running_tun_protocol,
                                lang,
                            )
                        ),
                        format!(
                            "{}{colon}{}",
                            self.tr("Mesh 协议版本", "Mesh protocol"),
                            xrose_core::system::privilege::format_protocol_version(
                                platform.mesh_protocol,
                                platform.helper_available,
                                platform.helper_needs_update,
                                platform.helper_running_mesh_protocol,
                                lang,
                            )
                        ),
                        format!(
                            "{}{colon}v{}",
                            self.tr("数据库版本", "Database schema"),
                            platform.database_schema,
                        ),
                    ];
                    if let Some(path) = &platform.helper_path {
                        env_items.push(format!(
                            "{}{colon}{}",
                            self.tr("可执行程序", "Executable"),
                            if privacy { "•••" } else { path.as_str() }
                        ));
                    }
                    env_items.push(format!(
                        "{}{colon}{}",
                        self.tr("日志目录", "Log dir"),
                        if privacy { "•••" } else { platform.log_dir.as_str() }
                    ));
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_xs()
                        .text_color(p.muted_fg)
                        .children(env_items.into_iter().map(|item| {
                            div()
                                .flex()
                                .items_start()
                                .gap_2()
                                .child(div().flex_shrink_0().child("•"))
                                .child(div().flex_1().min_w(px(0.)).child(item))
                        }))
                })
                .children(doctor.iter().map(|check| {
                    let color = match check.level {
                        CheckLevel::Ok => p.success,
                        CheckLevel::Warn => p.warning,
                        CheckLevel::Error => p.danger,
                    };
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .child(dot(color))
                        .child(
                            div()
                                .w(px(110.))
                                .flex_shrink_0()
                                .text_color(p.fg)
                                .child(check.name.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .text_color(p.muted_fg)
                                .child(check.detail.clone()),
                        )
                }))
    }
}

/// A single settings row, with descriptions below the aligned control.
fn setting_row(
    p: &super::theme::Palette,
    label: impl Into<SharedString>,
    description: impl Into<SharedString>,
    control: impl IntoElement,
) -> Div {
    let description = description.into();
    div()
        .flex()
        .items_start()
        .gap_3()
        .py_1()
        .child(super::widgets::form_label_cell(
            super::theme::SETTING_LABEL_WIDTH,
            18.,
            label,
            |d| d.text_sm().text_color(p.muted_fg),
        ))
        .child(
            div()
                .flex()
                .flex_col()
                .items_start()
                .gap_1()
                .flex_1()
                .min_w(px(0.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .min_h(px(super::theme::CONTROL_HEIGHT))
                        .min_w(px(0.))
                        .max_w_full()
                        .child(control),
                )
                .when(!description.is_empty(), |d| {
                    d.child(div().text_xs().text_color(p.muted_fg).child(description))
                }),
        )
}

fn valid_port(raw: &str) -> Option<u16> {
    raw.trim().parse::<u16>().ok().filter(|port| *port > 0)
}

fn reload_note(lang: Language, hot_reload: bool) -> &'static str {
    match (lang, hot_reload) {
        (Language::Zh, true) => "将自动重载内核",
        (Language::Zh, false) => "需重载内核",
        (Language::En, true) => "Core reloads automatically",
        (Language::En, false) => "Core reload required",
    }
}

fn with_reload_note(lang: Language, hot_reload: bool, zh: &str, en: &str) -> String {
    let note = reload_note(lang, hot_reload);
    match lang {
        Language::Zh => format!("{zh}；{note}"),
        Language::En => format!("{en}; {}", note.to_ascii_lowercase()),
    }
}

/// Offer native materials only on the platforms that implement them.
fn backdrop_setting_available() -> bool {
    #[cfg(target_os = "linux")]
    return crate::wayland::desktop().blur;
    #[cfg(not(target_os = "linux"))]
    return cfg!(any(windows, target_os = "macos"));
}

fn backdrop_choices() -> Vec<(BackdropStyle, &'static str, &'static str)> {
    let mut choices = vec![(BackdropStyle::Auto, "自动", "Auto")];
    if cfg!(windows) {
        choices.extend([
            (BackdropStyle::Mica, "Mica", "Mica"),
            (BackdropStyle::MicaAlt, "Mica Alt", "Mica Alt"),
            (BackdropStyle::Acrylic, "Acrylic", "Acrylic"),
        ]);
    }
    if cfg!(target_os = "macos") {
        choices.push((BackdropStyle::Vibrancy, "毛玻璃", "Vibrancy"));
    }
    #[cfg(target_os = "linux")]
    choices.push((BackdropStyle::Vibrancy, "毛玻璃", "Blur"));
    choices.push((BackdropStyle::Opaque, "纯色", "Solid"));
    choices
}

#[cfg(test)]
mod tests {
    use xrose_core::settings::Language;

    #[test]
    fn settings_tab_keyboard_navigation_wraps_and_supports_endpoints() {
        use super::SettingsTab;
        assert!(SettingsTab::Appearance.keyboard_target("left") == Some(SettingsTab::Helper));
        assert!(SettingsTab::Helper.keyboard_target("right") == Some(SettingsTab::Appearance));
        assert!(SettingsTab::Appearance.keyboard_target("right") == Some(SettingsTab::Startup));
        assert!(SettingsTab::Startup.keyboard_target("right") == Some(SettingsTab::Core));
        assert!(SettingsTab::Core.keyboard_target("right") == Some(SettingsTab::Network));
        assert!(SettingsTab::Network.keyboard_target("home") == Some(SettingsTab::Appearance));
        assert!(SettingsTab::Network.keyboard_target("end") == Some(SettingsTab::Helper));
        assert!(SettingsTab::Startup.keyboard_target("enter") == Some(SettingsTab::Startup));
        assert!(SettingsTab::Updates.keyboard_target("space") == Some(SettingsTab::Updates));
        assert!(SettingsTab::Network.keyboard_target("tab").is_none());
    }

    #[test]
    fn port_validation_rejects_empty_zero_and_overflow() {
        for raw in ["", "0", "65536", "-1", "abc"] {
            assert_eq!(super::valid_port(raw), None);
        }
        assert_eq!(super::valid_port(" 7890 "), Some(7890));
        assert_eq!(super::valid_port("65535"), Some(65535));
    }

    #[test]
    fn reload_notes_reflect_hot_reload_preference() {
        assert_eq!(super::reload_note(Language::Zh, true), "将自动重载内核");
        assert_eq!(super::reload_note(Language::Zh, false), "需重载内核");
        assert_eq!(
            super::reload_note(Language::En, true),
            "Core reloads automatically"
        );
        assert_eq!(
            super::reload_note(Language::En, false),
            "Core reload required"
        );

        assert_eq!(
            super::with_reload_note(Language::Zh, true, "修改后应用", "Apply after editing"),
            "修改后应用；将自动重载内核"
        );
        assert_eq!(
            super::with_reload_note(Language::Zh, false, "修改后应用", "Apply after editing"),
            "修改后应用；需重载内核"
        );
        assert_eq!(
            super::with_reload_note(Language::En, true, "修改后应用", "Apply after editing"),
            "Apply after editing; core reloads automatically"
        );
        assert_eq!(
            super::with_reload_note(Language::En, false, "修改后应用", "Apply after editing"),
            "Apply after editing; core reload required"
        );
    }
}
