//! The status capsule's middle traffic-takeover summary and Popover controls.
use gpui_base::Disableable;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonRounded, ButtonVariants};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    Anchor, App, Context, Div, IntoElement, ParentElement, SharedString, Styled, div, px,
};
use xrose_core::model::{CoreKind, WorkMode};
use xrose_core::service::UiSnapshot;
use xrose_core::settings::Language;
use xrose_core::system::proxy::SystemProxyStatus;

use crate::i18n::{proxy_status_label, tr};

use super::widgets::{divider, icon_button, pill};
use super::{AppView, theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TrafficTriggerTone {
    Accent,
    Warning,
    Neutral,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TrafficModeSummary {
    pub(super) icon: IconName,
    pub(super) label: SharedString,
    pub(super) tooltip: SharedString,
    pub(super) tone: TrafficTriggerTone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TrafficPopoverState {
    pub(super) mesh: bool,
    pub(super) running: bool,
    pub(super) hide_ips: bool,
    pub(super) core: CoreKind,
    pub(super) core_busy: bool,
    pub(super) proxy_active: bool,
    pub(super) proxy_busy: bool,
    pub(super) proxy_status: Option<SystemProxyStatus>,
    pub(super) pac_enabled: bool,
    pub(super) tun_enabled: bool,
    pub(super) tun_supported: bool,
    pub(super) helper_available: bool,
    pub(super) elevated: bool,
    pub(super) socks_port: u16,
    pub(super) active_proxy_addresses: Vec<String>,
    pub(super) active_tun_interface: Option<String>,
}

impl TrafficPopoverState {
    pub(super) fn from_snapshot(snapshot: &UiSnapshot, hide_ips: bool) -> Self {
        let mesh = snapshot.model.work_mode == WorkMode::Mesh;
        Self {
            mesh,
            running: snapshot.process.phase.is_alive(),
            hide_ips,
            core: snapshot.settings.core,
            core_busy: snapshot
                .busy
                .iter()
                .any(|key| key == "core" || key == "mesh-exit"),
            proxy_active: snapshot.system_proxy_active,
            proxy_busy: snapshot.busy.iter().any(|key| key == "proxy"),
            proxy_status: snapshot.system_proxy_status,
            pac_enabled: snapshot.settings.pac_enabled,
            tun_enabled: mesh || snapshot.settings.effective_tun(),
            tun_supported: snapshot.settings.core.capabilities().native_tun,
            helper_available: snapshot.platform.helper_available,
            elevated: snapshot.platform.elevated,
            socks_port: snapshot.settings.mixed_port,
            active_proxy_addresses: snapshot.active_proxy_addresses.clone(),
            active_tun_interface: snapshot.active_tun_interface.clone(),
        }
    }

    fn http_port(&self) -> Option<u16> {
        (self.core == CoreKind::Xray && self.socks_port > 0 && self.socks_port < u16::MAX)
            .then(|| self.socks_port + 1)
    }

    fn proxy_issue_badge(&self, lang: Language) -> Option<SharedString> {
        if self.mesh || self.proxy_busy {
            return None;
        }
        match self.proxy_status? {
            SystemProxyStatus::External => Some(tr(lang, "外部设置", "External")),
            SystemProxyStatus::Partial => Some(tr(lang, "部分接管", "Partial")),
            SystemProxyStatus::Unknown => Some(tr(lang, "状态未知", "Unknown")),
            SystemProxyStatus::Off | SystemProxyStatus::Xrose => None,
        }
    }

    fn proxy_issue_detail(&self, lang: Language) -> Option<SharedString> {
        self.proxy_status
            .filter(|_| !self.mesh && !self.proxy_busy)
            .and_then(|status| proxy_status_label(lang, status))
    }

    fn append_runtime_details(&self, tip: &mut String, lang: Language) {
        if !self.mesh {
            if self.running && !self.hide_ips && !self.active_proxy_addresses.is_empty() {
                tip.push('\n');
                tip.push_str(&format!(
                    "{}: {}",
                    tr(lang, "监听地址", "Listen"),
                    self.active_proxy_addresses.join(", ")
                ));
            } else if self.socks_port > 0 {
                tip.push('\n');
                if let Some(http_port) = self.http_port() {
                    tip.push_str(&format!("SOCKS5 :{} · HTTP :{http_port}", self.socks_port));
                } else {
                    tip.push_str(&format!(
                        "{} :{} (HTTP / SOCKS5)",
                        tr(lang, "混合端口", "Mixed port"),
                        self.socks_port
                    ));
                }
            }
        }
        if self.running
            && let Some(interface) = self.active_tun_interface.as_deref()
        {
            tip.push('\n');
            tip.push_str(&format!(
                "{}: {interface}",
                tr(lang, "TUN 网卡", "TUN interface")
            ));
        }
    }
}

pub(super) fn summarize_traffic_mode(
    state: &TrafficPopoverState,
    lang: Language,
) -> TrafficModeSummary {
    let finalize = |mut tip: String| -> SharedString {
        state.append_runtime_details(&mut tip, lang);
        tip.into()
    };

    if state.mesh {
        let tip = tr(
            lang,
            "组网模式已自动接管系统流量；代理模式的接管设置已保留",
            "Mesh mode routes system traffic automatically; proxy-mode takeover settings are preserved",
        )
        .to_string();
        return TrafficModeSummary {
            icon: IconName::Network,
            label: tr(lang, "组网接管", "Mesh routing"),
            tooltip: finalize(tip),
            tone: TrafficTriggerTone::Accent,
        };
    }

    if state.proxy_busy {
        let label = if state.tun_enabled {
            tr(lang, "切换代理 · TUN", "Proxy… · TUN")
        } else {
            tr(lang, "切换代理…", "Updating…")
        };
        return TrafficModeSummary {
            icon: IconName::Globe,
            label,
            tooltip: finalize(
                tr(
                    lang,
                    "正在应用系统代理设置…",
                    "Applying system proxy settings…",
                )
                .to_string(),
            ),
            tone: TrafficTriggerTone::Accent,
        };
    }

    if let Some(issue_detail) = state.proxy_issue_detail(lang) {
        let label = if state.tun_enabled {
            tr(lang, "代理异常 · TUN", "Proxy issue · TUN")
        } else {
            tr(lang, "代理异常", "Proxy issue")
        };
        let tip = format!(
            "{issue_detail}\n{}",
            tr(
                lang,
                "点击展开并可重新由 Xrose 接管系统代理",
                "Click to inspect or reclaim system proxy management",
            )
        );
        return TrafficModeSummary {
            icon: IconName::TriangleAlert,
            label,
            tooltip: finalize(tip),
            tone: TrafficTriggerTone::Warning,
        };
    }

    if state.proxy_active && state.tun_enabled {
        let label = if state.pac_enabled {
            tr(lang, "PAC · TUN", "PAC · TUN")
        } else {
            tr(lang, "代理 · TUN", "Proxy · TUN")
        };
        let tip = if state.pac_enabled {
            tr(
                lang,
                "PAC 系统代理与 TUN 虚拟网卡均已启用",
                "Both PAC system proxy and TUN are active",
            )
        } else {
            tr(
                lang,
                "系统代理与 TUN 虚拟网卡均已启用",
                "Both system proxy and TUN are active",
            )
        }
        .to_string();
        return TrafficModeSummary {
            icon: IconName::Shield,
            label,
            tooltip: finalize(tip),
            tone: TrafficTriggerTone::Accent,
        };
    }

    if state.proxy_active {
        let (label, tip) = if state.pac_enabled {
            (
                tr(lang, "PAC 代理", "PAC proxy"),
                tr(
                    lang,
                    "已通过 PAC 规则设置系统代理，点击快捷管理接管与端口",
                    "System proxy configured via PAC rules; click for quick controls",
                ),
            )
        } else {
            (
                tr(lang, "系统代理", "System proxy"),
                tr(
                    lang,
                    "系统代理已由 Xrose 接管，点击快捷管理接管与端口",
                    "System proxy is managed by Xrose; click for quick controls",
                ),
            )
        };
        return TrafficModeSummary {
            icon: IconName::Globe,
            label,
            tooltip: finalize(tip.to_string()),
            tone: TrafficTriggerTone::Accent,
        };
    }

    if state.tun_enabled {
        let tip = tr(
            lang,
            "TUN 已启用，接管系统全部网络流量",
            "TUN is enabled for system-wide traffic",
        )
        .to_string();
        return TrafficModeSummary {
            icon: IconName::Shield,
            label: "TUN".into(),
            tooltip: finalize(tip),
            tone: TrafficTriggerTone::Accent,
        };
    }

    let port = state.socks_port;
    let tip = if state.running {
        tr(
            lang,
            "系统代理与 TUN 均已关闭，仅本机端口监听中",
            "System proxy and TUN are off; local listener is active",
        )
    } else {
        tr(
            lang,
            "系统代理与 TUN 均已关闭（内核未启动）",
            "System proxy and TUN are off (Core stopped)",
        )
    }
    .to_string();
    TrafficModeSummary {
        icon: IconName::Plug,
        label: SharedString::from(format!("Mixed :{port}")),
        tooltip: finalize(tip),
        tone: TrafficTriggerTone::Neutral,
    }
}

fn proxy_hint(state: &TrafficPopoverState, lang: Language) -> SharedString {
    if state.mesh {
        return tr(
            lang,
            "组网模式下保留设置（不可切换）",
            "Preserved in Mesh mode (locked)",
        );
    }
    if state.proxy_busy {
        return tr(
            lang,
            "正在应用系统代理设置…",
            "Applying system proxy settings…",
        );
    }
    if let Some(issue) = state.proxy_issue_detail(lang) {
        return issue;
    }
    if state.proxy_active {
        return if state.pac_enabled {
            tr(
                lang,
                "已按 PAC 规则接管系统代理",
                "Managed via PAC routing rules",
            )
        } else {
            tr(
                lang,
                "已接管系统 HTTP / SOCKS 代理",
                "Managing system HTTP / SOCKS proxy",
            )
        };
    }
    tr(
        lang,
        "供遵循系统代理设置的应用使用",
        "For applications that use system proxy settings",
    )
}

fn tun_hint(state: &TrafficPopoverState, lang: Language) -> SharedString {
    if state.mesh {
        return tr(
            lang,
            "组网模式已自动接管虚拟网卡",
            "Mesh mode manages TUN automatically",
        );
    }
    if !state.tun_supported {
        return tr(
            lang,
            "当前内核不支持 TUN，可在设置切换内核",
            "Unsupported by current core; switch core in Settings",
        );
    }
    if state.running
        && let Some(interface) = state.active_tun_interface.as_deref()
    {
        return SharedString::from(format!(
            "{}: {interface}",
            tr(lang, "当前网卡", "Interface")
        ));
    }
    if state.tun_enabled {
        return tr(
            lang,
            "已接管系统全部网络流量",
            "Routing all system traffic via TUN",
        );
    }
    if state.helper_available || state.elevated {
        return tr(
            lang,
            "接管系统全部网络流量",
            "Route all system traffic via TUN",
        );
    }
    tr(
        lang,
        "接管系统网络流量，需要提权助手",
        "Route system traffic (requires privilege helper)",
    )
}

fn mixed_port_subtitle(state: &TrafficPopoverState, lang: Language) -> SharedString {
    if let Some(http_port) = state.http_port() {
        SharedString::from(format!("SOCKS5 :{} · HTTP :{http_port}", state.socks_port))
    } else {
        tr(
            lang,
            "HTTP 与 SOCKS5 共用此端口",
            "HTTP and SOCKS5 share this port",
        )
    }
}

fn trigger_variant(tone: TrafficTriggerTone, p: &theme::Palette, cx: &App) -> ButtonCustomVariant {
    match tone {
        TrafficTriggerTone::Accent => ButtonCustomVariant::new(cx)
            .color(p.accent)
            .foreground(p.primary)
            .hover(p.primary.opacity(theme::SEMANTIC_ALPHA + 0.06))
            .active(p.primary.opacity(theme::SEMANTIC_ALPHA + 0.12)),
        TrafficTriggerTone::Warning => ButtonCustomVariant::new(cx)
            .color(p.warning.opacity(theme::SEMANTIC_ALPHA))
            .foreground(p.warn_text)
            .hover(p.warning.opacity(theme::SEMANTIC_ALPHA + 0.08))
            .active(p.warning.opacity(theme::SEMANTIC_ALPHA + 0.14)),
        TrafficTriggerTone::Neutral => ButtonCustomVariant::new(cx)
            .foreground(p.muted_fg)
            .hover(p.neutral_hover())
            .active(p.neutral_active()),
    }
}

impl AppView {
    pub(super) fn traffic_mode_popover(&self, cx: &mut Context<Self>) -> Popover {
        let p = theme::palette(cx);
        let lang = self.lang();
        let state = TrafficPopoverState::from_snapshot(&self.snapshot, self.hide_ips);
        let summary = summarize_traffic_mode(&state, lang);
        let open = self.traffic_popover_open;
        let view = cx.weak_entity();
        let state_for_content = state.clone();

        let trigger = Button::new("header-traffic-mode")
            .compact()
            .h(px(theme::STATUS_HEIGHT - 2.))
            .px_2p5()
            .flex_shrink_0()
            .rounded(ButtonRounded::None)
            .custom(trigger_variant(summary.tone, &p, cx))
            .accessibility_label(format!(
                "{}: {}",
                self.tr("流量接管", "Traffic takeover"),
                summary.label
            ))
            .tooltip(summary.tooltip.clone())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(if state.proxy_busy {
                        Spinner::new().small().color(p.primary).into_any_element()
                    } else {
                        Icon::new(summary.icon)
                            .size(px(theme::ICON_SIZE - 2.))
                            .flex_shrink_0()
                            .into_any_element()
                    })
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .child(summary.label),
                    )
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size(px(theme::ICON_SIZE - 2.))
                            .flex_shrink_0()
                            .opacity(0.75),
                    ),
            );

        Popover::new("header-traffic-menu")
            .anchor(Anchor::TopCenter)
            .open(open)
            .trigger(trigger)
            .on_open_change(cx.listener(|this, next_open, _, cx| {
                if *next_open {
                    this.dismiss_action_menu(cx);
                    this.refresh_snapshot();
                    this.traffic_popover_open = true;
                } else {
                    this.traffic_popover_open = false;
                }
                cx.notify();
            }))
            .content(move |_, _, cx| {
                Self::traffic_popover_content(&state_for_content, lang, view.clone(), cx)
            })
    }

    fn traffic_popover_content(
        state: &TrafficPopoverState,
        lang: Language,
        view: gpui_kit::WeakEntity<Self>,
        cx: &App,
    ) -> Div {
        let p = theme::palette(cx);
        let mono = cx.theme().mono_font_family.clone();
        let issue_badge = state.proxy_issue_badge(lang);
        let proxy_sub = proxy_hint(state, lang);
        let tun_sub = tun_hint(state, lang);
        let mixed_sub = mixed_port_subtitle(state, lang);

        let view_proxy = view.clone();
        let view_open_settings = view.clone();
        let view_tun = view.clone();
        let view_copy = view;

        let open_proxy_settings = icon_button(
            "header-open-proxy-settings",
            IconName::ExternalLink,
            tr(lang, "打开系统代理设置", "Open system proxy settings"),
            cx,
        )
        .on_click(move |_, _, cx| {
            let _ = view_open_settings.update(cx, |this, _| {
                this.service.open_system_proxy_settings();
            });
        });

        let copy_proxy_env = icon_button(
            "header-copy-proxy-env",
            IconName::Copy,
            tr(
                lang,
                "复制代理环境变量命令",
                "Copy proxy environment commands",
            ),
            cx,
        )
        .disabled(state.socks_port == 0)
        .on_click(move |_, _, cx| {
            let _ = view_copy.update(cx, |this, cx| this.copy_proxy_env_commands(cx));
        });

        div()
            .w(px(theme::HEADER_TRAFFIC_POPOVER_WIDTH))
            .flex()
            .flex_col()
            .gap_2p5()
            .when(state.mesh, |d| {
                d.child(
                    div()
                        .px_2p5()
                        .py_1p5()
                        .rounded_md()
                        .bg(p.accent)
                        .text_xs()
                        .text_color(p.primary)
                        .child(tr(
                            lang,
                            "组网模式已接管系统流量；代理模式的接管设置会保留。",
                            "Mesh mode routes system traffic; proxy-mode settings are preserved.",
                        )),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .min_w(px(0.))
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                                            .text_color(p.fg)
                                            .child(tr(lang, "系统代理", "System proxy")),
                                    )
                                    .when(state.pac_enabled && !state.mesh, |d| {
                                        d.child(pill("PAC", p.muted_fg, p.panel_alt))
                                    })
                                    .when_some(issue_badge, |d, badge| {
                                        d.child(pill(
                                            badge,
                                            p.warn_text,
                                            p.warning.opacity(theme::SEMANTIC_ALPHA),
                                        ))
                                    }),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(if state.proxy_issue_detail(lang).is_some() {
                                        p.warn_text
                                    } else {
                                        p.muted_fg
                                    })
                                    .child(proxy_sub),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .flex_shrink_0()
                            .when(state.proxy_busy, |d| {
                                d.child(Spinner::new().small().color(p.muted_fg))
                            })
                            .child(open_proxy_settings)
                            .child(
                                Switch::new("header-proxy-switch")
                                    .checked(state.proxy_active)
                                    .disabled(state.proxy_busy || state.mesh)
                                    .accessibility_label(tr(lang, "系统代理", "System proxy"))
                                    .on_change(move |checked, _, cx| {
                                        let checked = *checked;
                                        let _ = view_proxy.update(cx, |this, cx| {
                                            this.service.set_system_proxy(checked);
                                            this.sync(cx);
                                        });
                                    }),
                            ),
                    ),
            )
            .child(divider(&p))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .min_w(px(0.))
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                                            .text_color(p.fg)
                                            .child("TUN"),
                                    )
                                    .when(!state.tun_supported && !state.mesh, |d| {
                                        d.child(pill(
                                            tr(lang, "内核不支持", "Unsupported"),
                                            p.warn_text,
                                            p.warning.opacity(theme::SEMANTIC_ALPHA),
                                        ))
                                    }),
                            )
                            .child(div().text_xs().text_color(p.muted_fg).child(tun_sub)),
                    )
                    .child(
                        Switch::new("header-tun-switch")
                            .checked(state.tun_enabled)
                            .disabled(!state.tun_supported || state.core_busy || state.mesh)
                            .accessibility_label("TUN")
                            .on_change(move |checked, _, cx| {
                                let checked = *checked;
                                let _ = view_tun.update(cx, |this, cx| {
                                    this.service.set_tun_enabled(checked);
                                    this.sync(cx);
                                });
                            }),
                    ),
            )
            .child(divider(&p))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .min_w(px(0.))
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                                            .text_color(p.fg)
                                            .child(tr(lang, "混合端口", "Mixed port")),
                                    )
                                    .child(
                                        div()
                                            .px_1p5()
                                            .py_0p5()
                                            .rounded_sm()
                                            .bg(p.panel_alt)
                                            .font_family(mono)
                                            .text_xs()
                                            .text_color(p.fg)
                                            .child(format!(":{}", state.socks_port)),
                                    ),
                            )
                            .child(div().text_xs().text_color(p.muted_fg).child(mixed_sub)),
                    )
                    .child(copy_proxy_env),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        TrafficPopoverState, TrafficTriggerTone, mixed_port_subtitle, proxy_hint,
        summarize_traffic_mode, tun_hint,
    };
    use gpui_kit::assets::IconName;
    use xrose_core::model::CoreKind;
    use xrose_core::settings::Language;
    use xrose_core::system::proxy::SystemProxyStatus;

    fn base_state() -> TrafficPopoverState {
        TrafficPopoverState {
            mesh: false,
            running: true,
            hide_ips: true,
            core: CoreKind::SingBox,
            core_busy: false,
            proxy_active: false,
            proxy_busy: false,
            proxy_status: Some(SystemProxyStatus::Off),
            pac_enabled: false,
            tun_enabled: false,
            tun_supported: true,
            helper_available: true,
            elevated: false,
            socks_port: 7890,
            active_proxy_addresses: Vec::new(),
            active_tun_interface: None,
        }
    }

    #[test]
    fn both_off_shows_socks_port_summary() {
        let mut state = base_state();
        let zh = summarize_traffic_mode(&state, Language::Zh);
        assert!(matches!(zh.icon, IconName::Plug));
        assert_eq!(zh.label, "Mixed :7890");
        assert_eq!(zh.tone, TrafficTriggerTone::Neutral);
        assert!(zh.tooltip.contains(":7890"));

        let en = summarize_traffic_mode(&state, Language::En);
        assert_eq!(en.label, "Mixed :7890");
        assert!(en.tooltip.contains("Mixed port :7890"));

        state.hide_ips = false;
        state.active_proxy_addresses = vec!["127.0.0.1:7890".into()];
        assert!(
            summarize_traffic_mode(&state, Language::Zh)
                .tooltip
                .contains("127.0.0.1:7890")
        );
    }

    #[test]
    fn active_takeover_modes_select_expected_labels_and_icons() {
        let mut proxy_only = base_state();
        proxy_only.proxy_active = true;
        proxy_only.proxy_status = Some(SystemProxyStatus::Xrose);
        let summary = summarize_traffic_mode(&proxy_only, Language::Zh);
        assert!(matches!(summary.icon, IconName::Globe));
        assert_eq!(summary.label, "系统代理");
        assert_eq!(summary.tone, TrafficTriggerTone::Accent);

        proxy_only.pac_enabled = true;
        assert_eq!(
            summarize_traffic_mode(&proxy_only, Language::Zh).label,
            "PAC 代理"
        );

        let mut both = proxy_only.clone();
        both.pac_enabled = false;
        both.tun_enabled = true;
        let summary = summarize_traffic_mode(&both, Language::En);
        assert!(matches!(summary.icon, IconName::Shield));
        assert_eq!(summary.label, "Proxy · TUN");

        let mut tun_only = base_state();
        tun_only.tun_enabled = true;
        let summary = summarize_traffic_mode(&tun_only, Language::Zh);
        assert!(matches!(summary.icon, IconName::Shield));
        assert_eq!(summary.label, "TUN");
    }

    #[test]
    fn external_proxy_and_mesh_override_take_precedence() {
        let mut external = base_state();
        external.proxy_status = Some(SystemProxyStatus::External);
        external.tun_enabled = true;
        let summary = summarize_traffic_mode(&external, Language::Zh);
        assert!(matches!(summary.icon, IconName::TriangleAlert));
        assert_eq!(summary.label, "代理异常 · TUN");
        assert_eq!(summary.tone, TrafficTriggerTone::Warning);
        assert_eq!(
            external.proxy_issue_badge(Language::En).as_deref(),
            Some("External")
        );

        let mut mesh = external.clone();
        mesh.mesh = true;
        mesh.active_tun_interface = Some("utun7".into());
        let summary = summarize_traffic_mode(&mesh, Language::Zh);
        assert!(matches!(summary.icon, IconName::Network));
        assert_eq!(summary.label, "组网接管");
        assert_eq!(summary.tone, TrafficTriggerTone::Accent);
        assert!(proxy_hint(&mesh, Language::En).contains("Mesh mode"));
        assert!(tun_hint(&mesh, Language::Zh).contains("组网模式"));
    }

    #[test]
    fn xray_mixed_port_subtitle_mentions_socks_and_http_ports() {
        let mut state = base_state();
        state.core = CoreKind::Xray;
        state.socks_port = 7890;
        let sub = mixed_port_subtitle(&state, Language::Zh);
        assert!(sub.contains("SOCKS5 :7890"));
        assert!(sub.contains("HTTP :7891"));
    }
}
