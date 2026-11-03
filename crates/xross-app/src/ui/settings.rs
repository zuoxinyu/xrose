use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Theme, ThemeMode as ComponentThemeMode};
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
        let running = self.snapshot.process.phase.is_alive();
        let helper_busy = self.snapshot.busy.iter().any(|b| b == "helper");

        let mut body = div().flex().flex_col().gap_4().w_full();

        // General ------------------------------------------------------
        body = body.child(
            card(&p)
                .child(section_title(
                    &p,
                    self.tr("通用", "General"),
                    self.tr(
                        "语言、主题、Core 与启动行为",
                        "Language, theme, core and startup",
                    ),
                ))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .w(px(120.))
                                        .text_sm()
                                        .text_color(p.muted_fg)
                                        .child(self.tr("语言", "Language")),
                                )
                                .children([Language::Zh, Language::En].into_iter().map(
                                    |language| {
                                        let selected = settings.language == language;
                                        div()
                                            .id(SharedString::from(format!("lang-{:?}", language)))
                                            .px_2()
                                            .py_1()
                                            .rounded_md()
                                            .text_xs()
                                            .border_1()
                                            .border_color(if selected {
                                                p.primary
                                            } else {
                                                p.border
                                            })
                                            .bg(if selected { p.accent } else { p.bg })
                                            .cursor_pointer()
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.service.set_language(language);
                                                this.sync(cx);
                                                this.localize_inputs(window, cx);
                                            }))
                                            .child(language.label())
                                    },
                                )),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .w(px(120.))
                                        .text_sm()
                                        .text_color(p.muted_fg)
                                        .child(self.tr("主题", "Theme")),
                                )
                                .children(
                                    [
                                        (ThemeMode::System, self.tr("跟随系统", "System")),
                                        (ThemeMode::Light, self.tr("浅色", "Light")),
                                        (ThemeMode::Dark, self.tr("深色", "Dark")),
                                    ]
                                    .into_iter()
                                    .map(|(mode, label)| {
                                        let selected = settings.theme == mode;
                                        div()
                                            .id(SharedString::from(format!("theme-{:?}", mode)))
                                            .px_2()
                                            .py_1()
                                            .rounded_md()
                                            .text_xs()
                                            .border_1()
                                            .border_color(if selected {
                                                p.primary
                                            } else {
                                                p.border
                                            })
                                            .bg(if selected { p.accent } else { p.bg })
                                            .cursor_pointer()
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.service.set_theme(mode);
                                                match mode {
                                                    ThemeMode::Light => {
                                                        crate::window_effects::set_follow_system(
                                                            false,
                                                        );
                                                        Theme::change(
                                                            ComponentThemeMode::Light,
                                                            Some(window),
                                                            cx,
                                                        );
                                                    }
                                                    ThemeMode::Dark => {
                                                        crate::window_effects::set_follow_system(
                                                            false,
                                                        );
                                                        Theme::change(
                                                            ComponentThemeMode::Dark,
                                                            Some(window),
                                                            cx,
                                                        );
                                                    }
                                                    ThemeMode::System => {
                                                        crate::window_effects::sync_system_appearance(
                                                            window,
                                                            cx,
                                                        );
                                                    }
                                                }
                                                // Applying a theme rewrites the root surface
                                                // color; keep it clear so the backdrop shows.
                                                crate::window_effects::refresh_all(this.service.settings().backdrop, cx);
                                                this.sync(cx);
                                            }))
                                            .child(label)
                                    }),
                                ),
                        )
                        .child({
                            let backdrop_tip = self.tr(
                                "同时应用到主窗口和托盘浮窗；系统不支持时使用纯色背景。",
                                "Applies to both windows. Uses a solid background when unavailable.",
                            );
                            let label_tip = backdrop_tip.clone();
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .id("backdrop-label")
                                        .w(px(120.))
                                        .flex_shrink_0()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .text_sm()
                                        .text_color(p.muted_fg)
                                        .tooltip(move |window, cx| {
                                            Tooltip::new(label_tip.clone()).build(window, cx)
                                        })
                                        .child(self.tr("窗口材质", "Backdrop"))
                                        .child(
                                            Icon::new(IconName::Info)
                                                .size(px(12.))
                                                .text_color(p.muted_fg.opacity(0.75)),
                                        ),
                                )
                                .children(backdrop_choices().into_iter().map(
                                    |(style, zh, en)| {
                                        let selected = settings.backdrop == style;
                                        let item_tip = backdrop_tip.clone();
                                        div()
                                            .id(SharedString::from(format!("backdrop-{style:?}")))
                                            .px_2()
                                            .py_1()
                                            .rounded_md()
                                            .text_xs()
                                            .border_1()
                                            .border_color(if selected {
                                                p.primary
                                            } else {
                                                p.border
                                            })
                                            .bg(if selected { p.accent } else { p.bg })
                                            .cursor_pointer()
                                            .tooltip(move |window, cx| {
                                                Tooltip::new(item_tip.clone()).build(window, cx)
                                            })
                                            .child(self.tr(zh, en))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.service.set_backdrop(style);
                                                crate::window_effects::refresh_all(
                                                    this.service.settings().backdrop,
                                                    cx,
                                                );
                                                this.sync(cx);
                                            }))
                                    },
                                ))
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .w(px(120.))
                                        .text_sm()
                                        .text_color(p.muted_fg)
                                        .child(self.tr("Core", "Core")),
                                )
                                .children(CoreKind::all().into_iter().map(|core| {
                                    let selected = settings.core == core;
                                    div()
                                        .id(SharedString::from(format!("core-{:?}", core)))
                                        .px_3()
                                        .py_1()
                                        .rounded_md()
                                        .text_xs()
                                        .border_1()
                                        .border_color(if selected { p.primary } else { p.border })
                                        .bg(if selected { p.accent } else { p.bg })
                                        .cursor_pointer()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.service.set_core(core);
                                            this.sync(cx);
                                        }))
                                        .child(core.display_name())
                                }))
                                .when(running, |d| {
                                    d.child(
                                        div()
                                            .text_xs()
                                            .text_color(p.warning)
                                            .child(self.tr("修改后需重启", "restart required")),
                                    )
                                }),
                        ),
                )
                .child(divider(&p))
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap_3()
                        .child(
                            div()
                                .w(px(120.))
                                .h(px(32.))
                                .flex()
                                .items_center()
                                .flex_shrink_0()
                                .text_sm()
                                .text_color(p.muted_fg)
                                .child(self.tr("网络", "Network")),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .flex_1()
                                .min_w(px(0.))
                                .items_center()
                                .gap_x_6()
                                .gap_y_2()
                                .child(
                                    settings_toggle(
                                        "allow-lan",
                                        self.tr("允许局域网连接", "Allow LAN"),
                                        settings.allow_lan,
                                    )
                                    .on_change(cx.listener(
                                        |this, checked, _, cx| {
                                            this.modify_settings(cx, |s| {
                                                s.allow_lan = *checked
                                            });
                                        },
                                    )),
                                )
                                .child(
                                    settings_toggle("tun", "TUN", settings.tun_enabled).on_change(
                                        cx.listener(
                                            |this, checked, _, cx| {
                                                this.service.set_tun_enabled(*checked);
                                                this.sync(cx);
                                            },
                                        ),
                                    ),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap_3()
                        .child(
                            div()
                                .w(px(120.))
                                .h(px(32.))
                                .flex()
                                .items_center()
                                .flex_shrink_0()
                                .text_sm()
                                .text_color(p.muted_fg)
                                .child(self.tr("代理", "Proxy")),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .flex_1()
                                .min_w(px(0.))
                                .items_center()
                                .gap_x_6()
                                .gap_y_2()
                                .child(
                                    settings_toggle(
                                        "system-proxy",
                                        self.tr("系统代理", "System proxy"),
                                        settings.system_proxy,
                                    )
                                    .on_change(cx.listener(
                                        |this, checked, _, _| {
                                            this.service.set_system_proxy(*checked);
                                        },
                                    )),
                                )
                                .child(
                                    settings_toggle("pac", "PAC", settings.pac_enabled).on_change(
                                        cx.listener(
                                            |this, checked, _, cx| {
                                                this.modify_settings(cx, |s| {
                                                    s.pac_enabled = *checked
                                                });
                                                this.service.reapply_system_proxy();
                                                cx.notify();
                                            },
                                        ),
                                    ),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap_3()
                        .child(
                            div()
                                .w(px(120.))
                                .h(px(32.))
                                .flex()
                                .items_center()
                                .flex_shrink_0()
                                .text_sm()
                                .text_color(p.muted_fg)
                                .child(self.tr("启动", "Startup")),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .flex_1()
                                .min_w(px(0.))
                                .items_center()
                                .gap_x_6()
                                .gap_y_2()
                                .child(
                                    settings_toggle(
                                        "autostart",
                                        self.tr("开机自启", "Autostart"),
                                        settings.autostart,
                                    )
                                    .on_change(cx.listener(
                                        |this, checked, _, cx| {
                                            this.modify_settings(cx, |s| {
                                                s.autostart = *checked
                                            });
                                        },
                                    )),
                                )
                                .child(
                                    settings_toggle(
                                        "watchdog",
                                        self.tr("崩溃自动重启", "Auto-restart on crash"),
                                        settings.watchdog,
                                    )
                                    .on_change(cx.listener(
                                        |this, checked, _, cx| {
                                            this.modify_settings(cx, |s| {
                                                s.watchdog = *checked
                                            });
                                        },
                                    )),
                                )
                                .child(
                                    settings_toggle(
                                        "start-minimized",
                                        self.tr("启动时最小化", "Start minimized"),
                                        settings.start_minimized,
                                    )
                                    .on_change(cx.listener(
                                        |this, checked, _, cx| {
                                            this.modify_settings(cx, |s| {
                                                s.start_minimized = *checked
                                            });
                                        },
                                    )),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap_3()
                        .child(
                            div()
                                .w(px(120.))
                                .h(px(32.))
                                .flex()
                                .items_center()
                                .flex_shrink_0()
                                .text_sm()
                                .text_color(p.muted_fg)
                                .child(self.tr("窗口", "Window")),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .flex_1()
                                .min_w(px(0.))
                                .items_center()
                                .gap_x_6()
                                .gap_y_2()
                                .child(
                                    settings_toggle(
                                        "minimize-to-tray",
                                        self.tr("最小化到托盘", "Minimize to tray"),
                                        settings.minimize_to_tray,
                                    )
                                    .on_change(cx.listener(
                                        |this, checked, _, cx| {
                                            this.modify_settings(cx, |s| {
                                                s.minimize_to_tray = *checked
                                            });
                                        },
                                    )),
                                )
                                .child(
                                    settings_toggle(
                                        "close-to-tray",
                                        self.tr("关闭到托盘", "Close to tray"),
                                        settings.close_to_tray,
                                    )
                                    .on_change(cx.listener(
                                        |this, checked, _, cx| {
                                            this.modify_settings(cx, |s| {
                                                s.close_to_tray = *checked
                                            });
                                        },
                                    )),
                                )
                                .child(
                                    settings_toggle(
                                        "show-tray-speed",
                                        self.tr(
                                            if cfg!(target_os = "macos") {
                                                "菜单栏显示网速"
                                            } else {
                                                "托盘显示网速"
                                            },
                                            if cfg!(target_os = "macos") {
                                                "Show speed in menu bar"
                                            } else {
                                                "Show speed in tray"
                                            },
                                        ),
                                        settings.show_tray_speed,
                                    )
                                    .on_change(cx.listener(
                                        |this, checked, _, cx| {
                                            this.modify_settings(cx, |s| {
                                                s.show_tray_speed = *checked
                                            });
                                        },
                                    )),
                                ),
                        ),
                )
                .when(!crate::tray::available(), |d| {
                    d.child(div().text_xs().text_color(p.warning).child(self.tr(
                        "当前环境不支持系统托盘，关闭窗口将直接退出",
                        "System tray is unavailable in the current environment; closing the window will quit the app",
                    )))
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .w(px(120.))
                                .text_sm()
                                .text_color(p.muted_fg)
                                .child(self.tr("混合端口", "Mixed port")),
                        )
                        .child(div().w(px(120.)).child(Input::new(&self.port_input)))
                        .child(
                            Button::new("save-ports")
                                .compact()
                                .primary()
                                .label(self.tr("应用", "Apply"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let raw = this.port_input.read(cx).value().to_string();
                                    match raw.trim().parse::<u16>() {
                                        Ok(port) if port > 0 => {
                                            this.modify_settings(cx, |s| s.mixed_port = port);
                                        }
                                        _ => this.service.toast(
                                            xross_core::service::ToastLevel::Warning,
                                            "端口必须是 1-65535",
                                        ),
                                    }
                                })),
                        )
                        .when(settings.core == CoreKind::Xray, |d| {
                            d.child(div().text_xs().text_color(p.muted_fg).child(self.tr(
                                "Xray 模式下：SOCKS 监听此端口，HTTP 监听端口 +1",
                                "Xray has no mixed inbound: SOCKS uses this port, HTTP uses +1",
                            )))
                        }),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .w(px(120.))
                                .text_sm()
                                .text_color(p.muted_fg)
                                .child(self.tr("日志级别", "Log level")),
                        )
                        .children(
                            [
                                (LogLevel::Debug, "Debug"),
                                (LogLevel::Info, "Info"),
                                (LogLevel::Warn, "Warn"),
                                (LogLevel::Error, "Error"),
                            ]
                            .into_iter()
                            .map(|(level, label)| {
                                let selected = settings.log_level == level;
                                div()
                                    .id(SharedString::from(format!("set-log-{label}")))
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .border_1()
                                    .border_color(if selected { p.primary } else { p.border })
                                    .bg(if selected { p.accent } else { p.bg })
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.modify_settings(cx, |s| s.log_level = level);
                                    }))
                                    .child(label)
                            }),
                        ),
                ),
        );

        // Core & updates ------------------------------------------------
        let current_core = settings.core;
        let active_version = settings.active_version(current_core).cloned();
        let core_binaries: Vec<_> = binaries
            .iter()
            .filter(|b| b.core == current_core)
            .cloned()
            .collect();
        body = body.child(
            card(&p)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(section_title(
                            &p,
                            self.tr("Core 与更新", "Core & updates"),
                            self.tr(
                                "管理已安装的内核版本、检查组件更新与数据备份",
                                "Manage installed core versions, check updates, and back up data",
                            ),
                        ))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    Button::new("install-core")
                                        .compact()
                                        .primary()
                                        .label(self.tr("下载 / 更新", "Download / update"))
                                        .loading(self.snapshot.busy.iter().any(|b| b == "install"))
                                        .on_click(cx.listener(|this, _, _, _| {
                                            let core = this.snapshot.settings.core;
                                            this.service.install_core_update(core);
                                        })),
                                )
                                .child(
                                    Button::new("check-updates")
                                        .compact()
                                        .label(self.tr("检查更新", "Check updates"))
                                        .loading(self.snapshot.busy.iter().any(|b| b == "updates"))
                                        .on_click(cx.listener(|this, _, _, _| {
                                            this.service.check_updates();
                                        })),
                                ),
                        ),
                )
                .child(div().text_xs().text_color(p.muted_fg).child(format!(
                        "{}：{} / {} · {} {}",
                        self.tr("当前平台", "Platform"),
                        platform.os,
                        platform.arch,
                        self.tr("当前版本", "active version"),
                        active_version
                            .clone()
                            .unwrap_or_else(|| self.tr("未安装", "not installed").to_string())
                    )))
                .when(core_binaries.is_empty(), |d| {
                    d.child(empty_hint(
                        &p,
                        self.tr(
                            "尚未安装该 core，点击“下载 / 更新”自动获取最新版本",
                            "This core is not installed — use Download / update",
                        ),
                    ))
                })
                .children(core_binaries.into_iter().map(|record| {
                    let version = record.version.clone();
                    let core = record.core;
                    let delete_version = record.version.clone();
                    let is_active = active_version.as_deref() == Some(record.version.as_str());
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
                                .gap_2()
                                .when(!is_active, |d| {
                                    d.child(
                                        Button::new(SharedString::from(format!(
                                            "use-version-{version}"
                                        )))
                                        .compact()
                                        .label(self.tr("使用", "Use"))
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.service.set_active_version(core, &version);
                                                this.sync(cx);
                                            }),
                                        ),
                                    )
                                })
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "remove-version-{delete_version}"
                                    )))
                                    .compact()
                                    .danger()
                                    .label("✕")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.service.remove_binary(core, &delete_version);
                                        this.sync(cx);
                                    })),
                                ),
                        )
                }))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .items_center()
                        .child(div().flex_1().child(Input::new(&self.endpoint_input)))
                        .child(
                            Button::new("use-custom-path")
                                .compact()
                                .label(self.tr("使用自定义路径", "Use custom path"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let path =
                                        this.endpoint_input.read(cx).value().trim().to_string();
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
                                .ghost()
                                .label(self.tr("清除覆盖", "Clear override"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let core = this.snapshot.settings.core;
                                    this.service.set_binary_override(core, None);
                                    this.sync(cx);
                                })),
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
                                .label(self.tr("备份数据库", "Backup database"))
                                .on_click(cx.listener(|this, _, _, _| {
                                    let _ = this.service.backup_database();
                                })),
                        )
                        .child(
                            Button::new("export-backup")
                                .compact()
                                .label(self.tr("导出 JSON 备份", "Export JSON"))
                                .on_click(cx.listener(|this, _, _, _| {
                                    let _ = this.service.export_backup();
                                })),
                        )
                        .child(
                            Button::new("open-data")
                                .compact()
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
                                "安装、启动与卸载会弹出系统管理员授权（UAC / osascript / pkexec）",
                                "Install, start and uninstall go through the OS elevation prompt (UAC / osascript / pkexec)",
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
                                    .compact()
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
                                        .compact()
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
                                    .compact()
                                    .label(self.tr("重启", "Restart"))
                                    .loading(helper_busy)
                                    .on_click(cx.listener(|this, _, _, _| {
                                        this.service.restart_helper();
                                    })),
                            )
                            .child(
                                Button::new("uninstall-helper")
                                    .compact()
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
                                .compact()
                                .loading(doctor_busy || helper_busy)
                                .label(self.tr("重新检查", "Re-check"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.service.refresh_doctor();
                                    this.service.refresh_helper_status();
                                    this.sync(cx);
                                })),
                        ),
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

/// Keep each setting in an aligned column while allowing whole switches to wrap.
fn settings_toggle(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    checked: bool,
) -> gpui_kit::component::switch::Switch {
    toggle(id, label, checked)
        .w(px(216.))
        .min_h(px(32.))
        .flex_shrink_0()
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
