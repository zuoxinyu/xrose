use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::{Theme, ThemeMode as ComponentThemeMode};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xross_core::model::CoreKind;
use xross_core::settings::{Language, LogLevel, Settings, ThemeMode};
use xross_core::system::CheckLevel;

use super::theme::palette;
use super::widgets::*;
use super::AppView;

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
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.service.set_language(language);
                                                this.sync(cx);
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
                                                    ThemeMode::Light => Theme::change(
                                                        ComponentThemeMode::Light,
                                                        Some(window),
                                                        cx,
                                                    ),
                                                    ThemeMode::Dark => Theme::change(
                                                        ComponentThemeMode::Dark,
                                                        Some(window),
                                                        cx,
                                                    ),
                                                    ThemeMode::System => {
                                                        Theme::sync_system_appearance(
                                                            Some(window),
                                                            cx,
                                                        )
                                                    }
                                                }
                                                this.sync(cx);
                                            }))
                                            .child(label)
                                    }),
                                ),
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
                        .flex_wrap()
                        .gap_6()
                        .child(
                            toggle(
                                "allow-lan",
                                self.tr("允许局域网连接", "Allow LAN"),
                                settings.allow_lan,
                            )
                            .on_change(cx.listener(
                                |this, checked, _, cx| {
                                    this.modify_settings(cx, |s| s.allow_lan = *checked);
                                },
                            )),
                        )
                        .child(
                            toggle(
                                "autostart",
                                self.tr("开机自启", "Autostart"),
                                settings.autostart,
                            )
                            .on_change(cx.listener(
                                |this, checked, _, cx| {
                                    this.modify_settings(cx, |s| s.autostart = *checked);
                                },
                            )),
                        )
                        .child(
                            toggle(
                                "watchdog",
                                self.tr("watchdog 自动拉起", "Watchdog"),
                                settings.watchdog,
                            )
                            .on_change(cx.listener(
                                |this, checked, _, cx| {
                                    this.modify_settings(cx, |s| s.watchdog = *checked);
                                },
                            )),
                        )
                        .child(
                            toggle("tun", "TUN", settings.tun_enabled).on_change(cx.listener(
                                |this, checked, _, cx| {
                                    this.modify_settings(cx, |s| s.tun_enabled = *checked);
                                },
                            )),
                        )
                        .child(
                            toggle(
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
                            toggle("pac", "PAC", settings.pac_enabled).on_change(cx.listener(
                                |this, checked, _, cx| {
                                    this.modify_settings(cx, |s| s.pac_enabled = *checked);
                                    this.service.reapply_system_proxy();
                                    cx.notify();
                                },
                            )),
                        ),
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
                                .child(self.tr("混合端口", "Mixed port")),
                        )
                        .child(div().w(px(120.)).child(Input::new(&self.port_input)))
                        .child(
                            Button::new("save-ports")
                                .compact()
                                .primary()
                                .label(self.tr("保存端口", "Save port"))
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
                                "Xray 没有 mixed：SOCKS 用此端口，HTTP 自动使用 +1",
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

        // Core binaries -------------------------------------------------
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
                            self.tr("Core 管理", "Core binaries"),
                            self.tr(
                                "下载与 App 分离；支持多版本共存、SHA-256 校验、平台/架构选择",
                                "Independent from the app: multiple versions, SHA-256 verification, platform/arch selection",
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
                .child(
                    div()
                        .text_xs()
                        .text_color(p.muted_fg)
                        .child(format!(
                            "{}：{} / {} · {} {}",
                            self.tr("当前平台", "Platform"),
                            platform.os,
                            platform.arch,
                            self.tr("当前版本", "active version"),
                            active_version.clone().unwrap_or_else(|| self.tr("未安装", "not installed").to_string())
                        )),
                )
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
                                        .child(div().text_sm().text_color(p.fg).child(format!("v{version}")))
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
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(p.muted_fg)
                                        .child(format!(
                                            "{} · sha256 {}…",
                                            xross_core::util::format_bytes(record.size),
                                            &record.sha256[..12.min(record.sha256.len())]
                                        )),
                                ),
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
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.service.set_active_version(core, &version);
                                            this.sync(cx);
                                        })),
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
                .child(divider(&p))
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

        // Updates -------------------------------------------------------
        body = body.child(
            card(&p)
                .child(section_title(
                    &p,
                    self.tr("更新", "Updates"),
                    self.tr(
                        "App、core 与规则集分别更新，互不阻塞",
                        "App, core and rule-sets update independently",
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
                                .justify_between()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(p.fg)
                                        .child(self.tr("Core", "Core")),
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
                                .child(div().text_sm().text_color(p.fg).child("Xross App"))
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

        // Diagnostics ---------------------------------------------------
        body = body.child(
            card(&p)
                .child(section_title(
                    &p,
                    self.tr("运行环境检查", "Environment checks"),
                    self.tr(
                        "TUN、端口、权限、协议兼容性；系统代理后端与特权助手状态",
                        "TUN, ports, permissions, protocol support, proxy backend and helper",
                    ),
                ))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_3()
                        .text_xs()
                        .text_color(p.muted_fg)
                        .child(format!(
                            "{}：{}",
                            self.tr("系统代理后端", "Proxy backend"),
                            platform.proxy_backend
                        ))
                        .child(format!(
                            "{}：{}",
                            self.tr("提权", "Elevated"),
                            if platform.elevated { "yes" } else { "no" }
                        ))
                        .child(format!(
                            "{}：{}",
                            self.tr("特权助手", "Helper"),
                            if platform.helper_available {
                                "available"
                            } else {
                                "not installed"
                            }
                        ))
                        .child(format!(
                            "{}：{}",
                            self.tr("日志目录", "Log dir"),
                            platform.log_dir
                        )),
                )
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
                }))
                .child(
                    Button::new("refresh-doctor")
                        .compact()
                        .label(self.tr("重新检查", "Re-check"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.service.refresh_doctor();
                            this.sync(cx);
                        })),
                ),
        );

        // About ---------------------------------------------------------
        body = body.child(
            card(&p)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(img(self.logo_badge.clone()).w(px(52.)).h(px(52.)))
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_lg()
                                                .font_weight(FontWeight::BOLD)
                                                .text_color(p.fg)
                                                .child("Xross"),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(p.muted_fg)
                                                .child(format!(
                                                    "v{} · Rust + GPUI Kit · {} / {}",
                                                    self.snapshot.app_version,
                                                    platform.os,
                                                    platform.arch
                                                )),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    Button::new("about-github")
                                        .compact()
                                        .ghost()
                                        .label("GitHub")
                                        .on_click(cx.listener(|this, _, _, _| {
                                            this.service
                                                .open_url("https://github.com/xross-app/xross");
                                        })),
                                )
                                .child(
                                    Button::new("about-gpui")
                                        .compact()
                                        .ghost()
                                        .label("GPUI Kit")
                                        .on_click(cx.listener(|this, _, _, _| {
                                            this.service.open_url("https://gpui-kit.com");
                                        })),
                                ),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(p.muted_fg)
                        .child(self.tr(
                            "核心与 App 分离；配置存于本地 SQLite（敏感字段加密）；不会上传任何节点信息。",
                            "Core binaries are separate from the app; configs live in local encrypted SQLite. No node data is uploaded.",
                        )),
                ),
        );

        let _ = (lang, &window);
        body
    }
}
