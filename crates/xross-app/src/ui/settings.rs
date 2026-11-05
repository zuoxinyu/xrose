use gpui_base::Disableable;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::{Theme, ThemeMode as ComponentThemeMode};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xross_core::model::CoreKind;
use xross_core::settings::{BackdropStyle, Language, LogLevel, Settings, ThemeMode};
use xross_core::system::CheckLevel;

use super::AppView;
use super::theme::palette;
use super::widgets::*;

impl AppView {
    fn modify_settings(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Settings)) {
        let mut settings = self.snapshot.settings.clone();
        f(&mut settings);
        self.service.update_settings(settings);
        self.sync(cx);
    }

    pub(crate) fn page_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = palette(cx);
        let lang = self.lang();
        let settings = self.snapshot.settings.clone();
        let binaries = self.snapshot.binaries.clone();
        let updates = self.snapshot.updates.clone();
        let platform = self.snapshot.platform.clone();
        let doctor = self.snapshot.doctor.clone();
        let helper_busy = self.snapshot.busy.iter().any(|b| b == "helper");

        let mut body = div().flex().flex_col().gap_4().w_full();

        // Appearance uses controlled radio groups and keeps native material handling.
        let appearance = card(&p)
            .child(section_title(
                &p,
                self.tr("外观", "Appearance"),
                self.tr(
                    "语言、主题与窗口材质",
                    "Language, theme and window material",
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
            .when(cfg!(any(windows, target_os = "macos")), |card| {
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
            });
        body = body.child(appearance);

        let port_raw = self.port_input.read(cx).value().to_string();
        let port_dirty = port_raw.trim() != settings.mixed_port.to_string();
        let port_valid = valid_port(&port_raw);
        let network_card = card(&p)
            .child(section_title(&p, self.tr("网络与代理", "Network & proxy"), self.tr("连接范围、流量接管与本地端口", "Access scope, traffic routing and local ports")))
            .child(setting_row(&p, self.tr("局域网", "LAN"), self.tr("允许同一局域网的设备使用本机代理；需重启", "Allow devices on your LAN to use this proxy; restart required"),
                toggle("allow-lan", self.tr("允许局域网连接", "Allow LAN connections"), settings.allow_lan).on_change(cx.listener(|this, checked, _, cx| this.modify_settings(cx, |s| s.allow_lan = *checked)))))
            .child(setting_row(&p, "TUN", self.tr("接管系统网络流量，需要提权助手；需重启", "Route system traffic through the proxy; requires the privilege helper and restart"),
                toggle("tun", self.tr("启用 TUN", "Enable TUN"), settings.tun_enabled).on_change(cx.listener(|this, checked, _, cx| { this.service.set_tun_enabled(*checked); this.sync(cx); }))))
            .child(setting_row(&p, self.tr("严格路由", "Strict routing"), self.tr("限制流量绕过 TUN；需重启", "Restrict traffic from bypassing TUN; restart required"),
                toggle("tun-strict-route", self.tr("启用严格路由", "Enable strict routing"), settings.tun_strict_route).on_change(cx.listener(|this, checked, _, cx| this.modify_settings(cx, |s| s.tun_strict_route = *checked)))))
            .child(setting_row(&p, self.tr("系统代理", "System proxy"), self.tr("供遵循系统代理设置的应用使用", "For applications that use the system proxy settings"),
                toggle("system-proxy", self.tr("启用系统代理", "Enable system proxy"), settings.system_proxy).on_change(cx.listener(|this, checked, _, _| this.service.set_system_proxy(*checked)))))
            .child(setting_row(&p, "PAC", self.tr("根据路由规则自动选择代理", "Choose the proxy automatically using routing rules"),
                toggle("pac", self.tr("启用 PAC", "Enable PAC"), settings.pac_enabled).on_change(cx.listener(|this, checked, _, cx| { this.modify_settings(cx, |s| s.pac_enabled = *checked); this.service.reapply_system_proxy(); }))))
            .child(setting_row(&p, self.tr("混合端口", "Mixed port"), self.tr("修改后应用；需重启", "Apply after editing; restart required"),
                div().flex().items_center().gap_2().child(div().w(px(super::theme::SETTING_LABEL_WIDTH)).child(Input::new(&self.port_input)))
                    .when(port_dirty, |d| d.child(Button::new("save-ports").primary().label(self.tr("应用", "Apply")).disabled(port_valid.is_none())
                        .on_click(cx.listener(|this, _, _, cx| { if let Some(port) = valid_port(this.port_input.read(cx).value().as_ref()) { this.modify_settings(cx, |s| s.mixed_port = port); } }))))))
            .when(port_dirty && port_valid.is_none(), |d| d.child(div().text_sm().text_color(p.danger_text).child(self.tr("端口必须是 1–65535", "Enter a port from 1 to 65535"))))
            .when(settings.core == CoreKind::Xray, |d| d.child(div().text_xs().text_color(p.muted_fg).child(self.tr("Xray：SOCKS 使用此端口，HTTP 使用下一端口", "Xray: SOCKS uses this port, HTTP uses the next port"))));

        let startup_card = card(&p)
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
                    .map(|profile| {
                        let id = profile.id.clone();
                        let provider = profile.provider;
                        setting_row(
                            &p,
                            self.tr("启动组网", "Mesh startup"),
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
                    self.tr("最小化", "Minimize"),
                    "",
                    toggle(
                        "minimize-to-tray",
                        self.tr(
                            if cfg!(target_os = "macos") {
                                "最小化到菜单栏"
                            } else {
                                "最小化到系统托盘"
                            },
                            if cfg!(target_os = "macos") {
                                "Minimize to menu bar"
                            } else {
                                "Minimize to system tray"
                            },
                        ),
                        settings.minimize_to_tray,
                    )
                    .on_change(cx.listener(|this, checked, _, cx| {
                        this.modify_settings(cx, |s| s.minimize_to_tray = *checked)
                    })),
                ))
                .child(setting_row(
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
                        this.modify_settings(cx, |s| s.close_to_tray = *checked)
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
            ));

        // Core & updates ------------------------------------------------
        let is_installing = self.snapshot.busy.iter().any(|b| b == "install");
        let is_checking_updates = self.snapshot.busy.iter().any(|b| b == "updates");

        let mut core_card = card(&p)
            .child(section_title(
                &p,
                self.tr("Core", "Core"),
                self.tr(
                    "内核选择、版本与运行选项",
                    "Core selection, versions and runtime options",
                ),
            ))
            .child(setting_row(
                &p,
                self.tr("内核", "Core"),
                self.tr("需重启", "Restart required"),
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
                self.tr("需重启", "Restart required"),
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
                .filter(|b| b.core == core)
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
                            .when(is_engine, |d| {
                                d.child(pill(
                                    self.tr("当前选用", "Selected"),
                                    p.primary_fg,
                                    p.primary,
                                ))
                            })
                            .child(div().text_xs().text_color(p.muted_fg).child(format!(
                                "{}: {}",
                                self.tr("当前版本", "Active"),
                                active_version.clone().unwrap_or_else(|| {
                                    self.tr("未安装", "Not installed").to_string()
                                })
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

            if core_binaries.is_empty() {
                section = section.child(empty_hint(
                    &p,
                    self.tr(
                        "尚未安装该 Core，点击“下载 / 更新”自动获取最新版本",
                        "This core is not installed — use Download / update",
                    ),
                ));
            } else {
                for record in core_binaries {
                    let version = record.version.clone();
                    let delete_version = record.version.clone();
                    let is_active = active_version.as_deref() == Some(record.version.as_str());
                    section = section.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .p_2()
                            .rounded_md()
                            .bg(p.bg)
                            .border_1()
                            .border_color(if is_active { p.primary } else { p.border })
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .flex_1()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .text_color(p.fg)
                                                    .child(format!("v{version}")),
                                            )
                                            .when(is_active, |d| {
                                                d.child(pill(
                                                    self.tr("使用中", "active"),
                                                    p.primary_fg,
                                                    p.primary,
                                                ))
                                            })
                                            .child(pill(
                                                format!("{}/{}", record.platform, record.arch),
                                                p.muted_fg,
                                                p.panel_alt,
                                            )),
                                    )
                                    .child(div().text_xs().text_color(p.muted_fg).child(format!(
                                        "{} · sha256 {}…",
                                        xross_core::util::format_bytes(record.size),
                                        &record.sha256[..12.min(record.sha256.len())]
                                    ))),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .when(!is_active, |d| {
                                        d.child(
                                            Button::new(SharedString::from(format!(
                                                "use-version-{core:?}-{version}"
                                            )))
                                            .compact()
                                            .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                            .label(self.tr("使用", "Use"))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.service.set_active_version(core, &version);
                                                this.sync(cx);
                                            })),
                                        )
                                    })
                                    .child(
                                        danger_icon_button(
                                            SharedString::from(format!(
                                                "remove-version-{core:?}-{delete_version}"
                                            )),
                                            IconName::Trash,
                                            self.tr("删除此版本", "Delete version"),
                                            cx,
                                        )
                                        .on_click(
                                            cx.listener(move |this, _, window, cx| {
                                                this.run_command(
                                                    super::menus::Command::DeleteVersion(
                                                        core,
                                                        delete_version.clone(),
                                                    ),
                                                    false,
                                                    window,
                                                    cx,
                                                );
                                            }),
                                        ),
                                    ),
                            ),
                    );
                }
            }

            core_card = core_card.child(section);
        }

        body = body.child(
            core_card.child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().child(Input::new(&self.endpoint_input)))
                    .child(
                        Button::new("use-custom-path")
                            .compact()
                            .h(px(crate::ui::theme::CONTROL_HEIGHT))
                            .label(self.tr("使用自定义路径", "Use custom path"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let path = this.endpoint_input.read(cx).value().trim().to_string();
                                let core = this.snapshot.settings.core;
                                if path.is_empty() {
                                    this.service.set_binary_override(core, None);
                                } else {
                                    this.service.set_binary_override(core, Some(path));
                                }
                                this.sync(cx);
                            })),
                    )
                    .child(
                        Button::new("clear-custom-path")
                            .compact()
                            .h(px(crate::ui::theme::CONTROL_HEIGHT))
                            .ghost()
                            .label(self.tr("清除覆盖", "Clear override"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let core = this.snapshot.settings.core;
                                this.service.set_binary_override(core, None);
                                this.sync(cx);
                            })),
                    ),
            ),
        );
        body = body.child(network_card).child(startup_card);
        body = body.child(
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
                                "检查更新与保存配置备份",
                                "Check for updates and back up configuration",
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
                                        .child(self.tr("Core 更新状态", "Core update status")),
                                )
                                .child(match &updates.core {
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
                                        .child(self.tr("没有可用更新", "No update found")),
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(div().text_sm().text_color(p.fg).child(
                                    self.tr("Xross App 更新状态", "Xross App update status"),
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
                                        .child(self.tr("没有可用更新", "No update found")),
                                }),
                        )
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
                ),
        );

        // Privilege helper & diagnostics --------------------------------
        let helper_label = if platform.helper_available {
            self.tr("运行中", "Running")
        } else if platform.helper_installed {
            self.tr("已安装，未运行", "Installed, not running")
        } else {
            self.tr("未安装", "Not installed")
        };
        let helper_color = if platform.helper_available {
            p.success
        } else if platform.helper_installed {
            p.warning
        } else {
            p.muted_fg
        };
        let doctor_busy = self.snapshot.busy.iter().any(|key| key == "doctor");
        body = body.child(
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
                                        self.tr("受支持", "supported")
                                    } else {
                                        self.tr("不支持", "unsupported")
                                    },
                                    p.muted_fg,
                                    p.panel_alt,
                                )),
                        ),
                )
                .child({
                    let mut items: Vec<String> = vec![platform.helper_detail.clone()];
                    if let Some(path) = &platform.helper_path {
                        items.push(format!("{}: {path}", self.tr("可执行文件", "Executable")));
                    }
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
                                    .loading(helper_busy)
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
                                        .loading(helper_busy)
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
                                    .loading(helper_busy)
                                    .on_click(cx.listener(|this, _, _, _| {
                                        this.service.restart_helper();
                                    })),
                            )
                            .child(
                                Button::new("uninstall-helper")
                                    .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                    .danger()
                                    .label(self.tr("卸载", "Uninstall"))
                                    .loading(helper_busy)
                                    .on_click(cx.listener(|this, _, _, _| {
                                        this.service.uninstall_helper();
                                    })),
                            )
                        })
                        .child(
                            Button::new("refresh-doctor")
                                .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                .loading(doctor_busy || helper_busy)
                                .label(self.tr("重新检查", "Re-check"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.service.refresh_doctor();
                                    this.service.refresh_helper_status();
                                    this.sync(cx);
                                })),
                        )
                        .when(platform.helper_supported && platform.helper_installed, |d| {
                            d.child(
                                Button::new("update-helper")
                                    .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                    .label(self.tr("更新助手", "Update helper"))
                                    .loading(helper_busy)
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
                            "该平台暂不支持提权助手；TUN 仍由 core 自身权限处理",
                            "The helper is not available on this platform; TUN relies on the core's own rights",
                        )),
                    )
                })
                .child(divider(&p))
                .child({
                    let env_items = [
                        format!(
                            "{}：{}",
                            self.tr("系统代理后端", "Proxy backend"),
                            platform.proxy_backend
                        ),
                        format!(
                            "{}：{}",
                            self.tr("提权状态", "Elevated"),
                            if platform.elevated {
                                self.tr("已提权", "yes")
                            } else {
                                self.tr("未提权", "no")
                            }
                        ),
                        format!(
                            "{}：{}",
                            self.tr("日志目录", "Log dir"),
                            platform.log_dir
                        ),
                    ];
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
                        .child(div().w(px(110.)).text_color(p.fg).child(check.name.clone()))
                        .child(
                            div()
                                .flex_1()
                                .text_color(p.muted_fg)
                                .child(check.detail.clone()),
                        )
                })),
        );

        let _ = (lang, &window);
        body
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
        .child(
            div()
                .w(px(super::theme::SETTING_LABEL_WIDTH))
                .h(px(super::theme::CONTROL_HEIGHT))
                .flex()
                .items_center()
                .flex_shrink_0()
                .text_sm()
                .text_color(p.muted_fg)
                .child(label.into()),
        )
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

#[cfg(test)]
mod tests {
    #[test]
    fn port_validation_rejects_empty_zero_and_overflow() {
        for raw in ["", "0", "65536", "-1", "abc"] {
            assert_eq!(super::valid_port(raw), None);
        }
        assert_eq!(super::valid_port(" 7890 "), Some(7890));
        assert_eq!(super::valid_port("65535"), Some(65535));
    }
}

/// Offer native materials only on the platforms that implement them.
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
    choices.push((BackdropStyle::Opaque, "纯色", "Solid"));
    choices
}
