//! Titlebar status triggers and quick-control popovers for EasyTier and Tailscale.
use gpui_base::Disableable;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    Anchor, App, ClipboardItem, Context, Div, FontWeight, IntoElement, ParentElement, SharedString,
    Styled, WeakEntity, div, px,
};
use xrose_core::mesh::{MeshPhase, MeshProfileView, MeshProvider, MeshRuntimeView};
use xrose_core::model::WorkMode;
use xrose_core::service::{ToastLevel, UiSnapshot};
use xrose_core::settings::Language;

use crate::i18n::tr;

use super::widgets::{divider, icon_button, pill};
use super::{AppView, Page, theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HeaderMeshTone {
    Connected,
    Info,
    Warning,
    Danger,
    Neutral,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HeaderMeshProfileItem {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) active: bool,
    pub(super) selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HeaderMeshState {
    pub(super) provider: MeshProvider,
    pub(super) phase: MeshPhase,
    pub(super) retrying: bool,
    pub(super) busy: bool,
    pub(super) supported: bool,
    pub(super) hide_ips: bool,
    pub(super) is_active_exit: bool,
    pub(super) selected_profile_id: String,
    pub(super) selected_profile_name: String,
    pub(super) profiles: Vec<HeaderMeshProfileItem>,
    pub(super) virtual_ip_raw: Option<String>,
    pub(super) virtual_ip_display: Option<String>,
    pub(super) peers_online: usize,
    pub(super) peers_total: usize,
    pub(super) dns_suffix_display: Option<String>,
    pub(super) auth_action: Option<(SharedString, String)>,
    pub(super) detail_notice: Option<(bool, SharedString)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HeaderMeshTriggerSummary {
    pub(super) tone: HeaderMeshTone,
    pub(super) label: SharedString,
    pub(super) show_exit_badge: bool,
    pub(super) tooltip: SharedString,
    pub(super) accessibility_label: SharedString,
}

fn is_active_phase(phase: MeshPhase) -> bool {
    matches!(
        phase,
        MeshPhase::Starting
            | MeshPhase::NeedsLogin
            | MeshPhase::NeedsApproval
            | MeshPhase::Connected
            | MeshPhase::Stopping
    )
}

fn mesh_phase_status_label(
    phase: MeshPhase,
    retrying: bool,
    has_login_url: bool,
    lang: Language,
) -> SharedString {
    if retrying {
        return tr(lang, "等待重连", "Waiting to reconnect");
    }
    match phase {
        MeshPhase::Disconnected => tr(lang, "未连接", "Disconnected"),
        MeshPhase::Starting => tr(lang, "连接中", "Connecting"),
        MeshPhase::NeedsLogin if has_login_url => tr(lang, "待登录", "Sign-in required"),
        MeshPhase::NeedsLogin => tr(lang, "待登录", "Waiting for sign-in"),
        MeshPhase::NeedsApproval => tr(lang, "待批准", "Approval required"),
        MeshPhase::Connected => tr(lang, "已连接", "Connected"),
        MeshPhase::Stopping => tr(lang, "断开中", "Disconnecting"),
        MeshPhase::Failed => tr(lang, "异常", "Failed"),
    }
}

impl HeaderMeshState {
    pub(super) fn from_snapshot(
        snapshot: &UiSnapshot,
        provider: MeshProvider,
        hide_ips: bool,
        selected_override: Option<&str>,
        lang: Language,
    ) -> Option<Self> {
        let provider_profiles: Vec<&MeshProfileView> = snapshot
            .mesh
            .profiles
            .iter()
            .filter(|p| p.provider == provider)
            .collect();
        if provider_profiles.is_empty() {
            return None;
        }

        let net = MeshRuntimeView::for_provider(&snapshot.mesh.networks, provider);
        let active_net = net.filter(|n| {
            is_active_phase(n.phase) && provider_profiles.iter().any(|p| p.id == n.profile_id)
        });
        let retrying_id = provider_profiles
            .iter()
            .find(|p| snapshot.mesh_reconnecting.contains(&p.id))
            .map(|p| p.id.as_str());
        let valid_override =
            selected_override.filter(|id| provider_profiles.iter().any(|p| p.id == *id));
        let net_profile_id = net
            .map(|n| n.profile_id.as_str())
            .filter(|id| provider_profiles.iter().any(|p| p.id == *id));
        let preferred_id = xrose_core::service::preferred_mesh_profile_id(
            &snapshot.mesh.profiles,
            &snapshot.settings,
            provider,
        );

        let selected_profile_id = active_net
            .map(|n| n.profile_id.as_str())
            .or(retrying_id)
            .or(valid_override)
            .or(net_profile_id)
            .or(preferred_id.as_deref())
            .unwrap_or_else(|| provider_profiles[0].id.as_str());

        let selected_profile = provider_profiles
            .iter()
            .copied()
            .find(|p| p.id == selected_profile_id)?;

        let matching_net = net.filter(|n| n.profile_id == selected_profile.id);
        let raw_phase = matching_net
            .map(|n| n.phase)
            .unwrap_or(MeshPhase::Disconnected);
        let retrying = snapshot.mesh_reconnecting.contains(&selected_profile.id);
        let connection_error = snapshot
            .mesh_connection_errors
            .get(&selected_profile.id)
            .cloned()
            .or_else(|| matching_net.and_then(|n| n.error.clone()));
        let phase = if !is_active_phase(raw_phase) && !retrying && connection_error.is_some() {
            MeshPhase::Failed
        } else {
            raw_phase
        };

        let is_active_exit = phase == MeshPhase::Connected
            && snapshot.model.work_mode == WorkMode::Mesh
            && snapshot
                .model
                .mesh_mode
                .target
                .as_ref()
                .is_some_and(|t| t.provider == provider);

        let connected_net = matching_net.filter(|_| phase == MeshPhase::Connected);
        let virtual_ip_raw = connected_net
            .and_then(|n| n.addresses.first())
            .map(|addr| addr.split('/').next().unwrap_or(addr).to_string());
        let virtual_ip_display = virtual_ip_raw.as_deref().map(|ip| {
            if hide_ips {
                xrose_core::privacy::mask_ip_or_cidr(ip)
            } else {
                ip.to_string()
            }
        });
        let peers_total = connected_net.map(|n| n.peers.len()).unwrap_or(0);
        let peers_online = connected_net
            .map(|n| n.peers.iter().filter(|peer| peer.online).count())
            .unwrap_or(0);
        let dns_suffix_display = connected_net
            .and_then(|n| n.dns_suffix.as_deref())
            .filter(|suffix| !suffix.is_empty())
            .map(|suffix| {
                if hide_ips {
                    xrose_core::privacy::mask_magic_dns_suffix(suffix)
                } else {
                    suffix.to_string()
                }
            });

        let official_control = selected_profile.control_url.is_empty()
            || selected_profile.control_url.trim_end_matches('/')
                == "https://controlplane.tailscale.com";
        let auth_action = if phase == MeshPhase::NeedsApproval && official_control {
            Some((
                tr(lang, "打开设备列表", "Open Machines"),
                "https://login.tailscale.com/admin/machines".to_owned(),
            ))
        } else if phase == MeshPhase::NeedsLogin {
            matching_net
                .and_then(|n| n.login_url.clone())
                .map(|url| (tr(lang, "在浏览器登录", "Sign in with browser"), url))
        } else {
            None
        };

        let detail_notice = if retrying && snapshot.platform.helper_needs_update {
            Some((
                false,
                tr(
                    lang,
                    "更新提权助手后将自动重连。",
                    "Reconnection will resume after the helper is updated.",
                ),
            ))
        } else if retrying && !snapshot.platform.helper_available {
            Some((
                false,
                tr(
                    lang,
                    "提权助手就绪后将自动重连。",
                    "Reconnection will resume when the helper is ready.",
                ),
            ))
        } else if retrying {
            Some((
                false,
                tr(
                    lang,
                    "连接中断，正在自动退避重连…",
                    "Connection dropped, reconnecting automatically…",
                ),
            ))
        } else if phase == MeshPhase::Failed {
            connection_error.map(|err| (true, err.into()))
        } else if phase == MeshPhase::NeedsApproval && official_control {
            Some((
                false,
                tr(
                    lang,
                    "请管理员在 Machines 页面批准此设备。",
                    "Ask an administrator to approve this device in Machines.",
                ),
            ))
        } else if phase == MeshPhase::NeedsApproval {
            Some((
                false,
                tr(
                    lang,
                    "请联系控制服务器管理员批准此设备。",
                    "Ask the control server administrator to approve this device.",
                ),
            ))
        } else if phase == MeshPhase::NeedsLogin && auth_action.is_some() {
            Some((
                false,
                tr(
                    lang,
                    "需要在浏览器完成账号认证。",
                    "Sign in with your browser to connect.",
                ),
            ))
        } else if phase == MeshPhase::NeedsLogin {
            Some((
                false,
                tr(
                    lang,
                    "正在获取登录链接；若长时间未出现，请检查控制服务器与网络。",
                    "Waiting for sign-in link; check control server and network if delayed.",
                ),
            ))
        } else {
            None
        };

        let profiles = provider_profiles
            .iter()
            .map(|p| HeaderMeshProfileItem {
                id: p.id.clone(),
                name: p.name.clone(),
                active: active_net.is_some_and(|n| n.profile_id == p.id),
                selected: p.id == selected_profile.id,
            })
            .collect();

        Some(Self {
            provider,
            phase,
            retrying,
            busy: snapshot
                .busy
                .iter()
                .any(|key| key == "mesh" || key == "mesh-exit"),
            supported: snapshot.mesh.supported,
            hide_ips,
            is_active_exit,
            selected_profile_id: selected_profile.id.clone(),
            selected_profile_name: selected_profile.name.clone(),
            profiles,
            virtual_ip_raw,
            virtual_ip_display,
            peers_online,
            peers_total,
            dns_suffix_display,
            auth_action,
            detail_notice,
        })
    }

    fn is_active_or_retrying(&self) -> bool {
        is_active_phase(self.phase) || self.retrying
    }
}

pub(super) fn summarize_mesh_trigger(
    state: &HeaderMeshState,
    compact: bool,
    lang: Language,
) -> HeaderMeshTriggerSummary {
    let full = state.provider.label();
    let short = match state.provider {
        MeshProvider::EasyTier => "ET",
        MeshProvider::Tailscale => "TS",
    };

    let tone = if state.retrying
        || matches!(
            state.phase,
            MeshPhase::NeedsLogin | MeshPhase::NeedsApproval
        ) {
        HeaderMeshTone::Warning
    } else if state.phase == MeshPhase::Failed {
        HeaderMeshTone::Danger
    } else if state.phase == MeshPhase::Connected {
        HeaderMeshTone::Connected
    } else if matches!(state.phase, MeshPhase::Starting | MeshPhase::Stopping) {
        HeaderMeshTone::Info
    } else {
        HeaderMeshTone::Neutral
    };

    let label: SharedString = if compact {
        short.into()
    } else if state.retrying {
        format!("{full} · {}", tr(lang, "重连中", "Reconnecting")).into()
    } else {
        match state.phase {
            MeshPhase::NeedsLogin => format!("{full} · {}", tr(lang, "待登录", "Sign-in")).into(),
            MeshPhase::NeedsApproval => {
                format!("{full} · {}", tr(lang, "待批准", "Approval")).into()
            }
            MeshPhase::Failed => format!("{full} · {}", tr(lang, "异常", "Failed")).into(),
            _ => full.into(),
        }
    };

    let status_text = mesh_phase_status_label(
        state.phase,
        state.retrying,
        state.auth_action.is_some(),
        lang,
    );

    let mut tip = format!("{full} ({}) · {status_text}", state.selected_profile_name);
    if state.phase == MeshPhase::Connected {
        if let Some(ip) = state.virtual_ip_display.as_deref() {
            tip.push('\n');
            tip.push_str(&match lang {
                Language::Zh => format!(
                    "虚拟 IP: {ip} · 设备 {}/{} 在线",
                    state.peers_online, state.peers_total
                ),
                Language::En => format!(
                    "Virtual IP: {ip} · {}/{} online",
                    state.peers_online, state.peers_total
                ),
            });
        } else {
            tip.push('\n');
            tip.push_str(&match lang {
                Language::Zh => format!("设备 {}/{} 在线", state.peers_online, state.peers_total),
                Language::En => format!("{}/{} online", state.peers_online, state.peers_total),
            });
        }
    }
    if let Some((_, notice)) = &state.detail_notice {
        tip.push('\n');
        tip.push_str(notice);
    }

    HeaderMeshTriggerSummary {
        tone,
        label,
        show_exit_badge: !compact && state.is_active_exit,
        tooltip: tip.into(),
        accessibility_label: format!("{full}: {status_text}").into(),
    }
}

fn trigger_variant(tone: HeaderMeshTone, p: &theme::Palette, cx: &App) -> ButtonCustomVariant {
    match tone {
        HeaderMeshTone::Connected | HeaderMeshTone::Info => ButtonCustomVariant::new(cx)
            .color(p.header_control_bg())
            .foreground(p.fg)
            .hover(p.header_control_hover())
            .active(p.neutral_active()),
        HeaderMeshTone::Warning => ButtonCustomVariant::new(cx)
            .color(p.warning.opacity(theme::SEMANTIC_ALPHA))
            .foreground(p.warn_text)
            .hover(p.warning.opacity(theme::SEMANTIC_ALPHA + 0.08))
            .active(p.warning.opacity(theme::SEMANTIC_ALPHA + 0.14)),
        HeaderMeshTone::Danger => ButtonCustomVariant::new(cx)
            .color(p.danger.opacity(theme::SEMANTIC_ALPHA))
            .foreground(p.danger_text)
            .hover(p.danger.opacity(theme::SEMANTIC_ALPHA + 0.08))
            .active(p.danger.opacity(theme::SEMANTIC_ALPHA + 0.14)),
        HeaderMeshTone::Neutral => ButtonCustomVariant::new(cx)
            .foreground(p.muted_fg)
            .hover(p.neutral_hover())
            .active(p.neutral_active()),
    }
}

fn status_badge_colors(
    tone: HeaderMeshTone,
    p: &theme::Palette,
) -> (gpui_kit::Hsla, gpui_kit::Hsla) {
    match tone {
        HeaderMeshTone::Connected => (p.ok_text, p.success.opacity(theme::SEMANTIC_ALPHA)),
        HeaderMeshTone::Info => (p.primary, p.accent),
        HeaderMeshTone::Warning => (p.warn_text, p.warning.opacity(theme::SEMANTIC_ALPHA)),
        HeaderMeshTone::Danger => (p.danger_text, p.danger.opacity(theme::SEMANTIC_ALPHA)),
        HeaderMeshTone::Neutral => (p.muted_fg, p.panel_alt),
    }
}

fn provider_icon(provider: MeshProvider) -> IconName {
    match provider {
        MeshProvider::EasyTier => IconName::Network,
        MeshProvider::Tailscale => IconName::Globe,
    }
}

impl AppView {
    pub(super) fn header_mesh_popover(
        &self,
        provider: MeshProvider,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> Option<Popover> {
        let lang = self.lang();
        let selected_override = self.header_mesh_selected.get(&provider).map(String::as_str);
        let state = HeaderMeshState::from_snapshot(
            &self.snapshot,
            provider,
            self.hide_ips,
            selected_override,
            lang,
        )?;
        let p = theme::palette(cx);
        let summary = summarize_mesh_trigger(&state, compact, lang);
        let open = self.header_mesh_popover == Some(provider);
        let view = cx.weak_entity();
        let state_for_content = state.clone();

        let indicator = match summary.tone {
            HeaderMeshTone::Info => Spinner::new().small().color(p.primary).into_any_element(),
            HeaderMeshTone::Warning | HeaderMeshTone::Danger => Icon::new(IconName::TriangleAlert)
                .size(px(13.))
                .flex_shrink_0()
                .into_any_element(),
            HeaderMeshTone::Connected => div()
                .size(px(7.))
                .rounded_full()
                .flex_shrink_0()
                .bg(p.success)
                .into_any_element(),
            HeaderMeshTone::Neutral => div()
                .size(px(7.))
                .rounded_full()
                .flex_shrink_0()
                .bg(p.muted_fg.opacity(0.65))
                .into_any_element(),
        };

        let trigger = Button::new(SharedString::from(format!(
            "header-mesh-{}",
            provider.key()
        )))
        .compact()
        .h(px(theme::CONTROL_HEIGHT))
        .px_2p5()
        .flex_shrink_0()
        .custom(trigger_variant(summary.tone, &p, cx))
        .accessibility_label(summary.accessibility_label)
        .tooltip(summary.tooltip)
        .child(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .child(indicator)
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .child(summary.label),
                )
                .when(summary.show_exit_badge, |d| {
                    d.child(pill(tr(lang, "出口", "Exit"), p.primary, p.accent))
                }),
        );

        Some(
            Popover::new(SharedString::from(format!(
                "header-mesh-menu-{}",
                provider.key()
            )))
            .anchor(Anchor::TopRight)
            .open(open)
            .trigger(trigger)
            .on_open_change(cx.listener(move |this, next_open, _, cx| {
                if *next_open {
                    this.dismiss_action_menu(cx);
                    this.refresh_snapshot();
                    this.header_mesh_popover = Some(provider);
                } else if this.header_mesh_popover == Some(provider) {
                    this.header_mesh_popover = None;
                }
                cx.notify();
            }))
            .content(move |_, _, cx| {
                Self::mesh_popover_content(&state_for_content, lang, view.clone(), cx)
            }),
        )
    }

    fn mesh_popover_content(
        state: &HeaderMeshState,
        lang: Language,
        view: WeakEntity<Self>,
        cx: &App,
    ) -> Div {
        let p = theme::palette(cx);
        let mono = cx.theme().mono_font_family.clone();
        let provider = state.provider;
        let summary = summarize_mesh_trigger(state, false, lang);
        let (badge_fg, badge_bg) = status_badge_colors(summary.tone, &p);
        let status_label = mesh_phase_status_label(
            state.phase,
            state.retrying,
            state.auth_action.is_some(),
            lang,
        );

        let stopping = state.phase == MeshPhase::Stopping;
        let active = is_active_phase(state.phase);
        let action_disabled = (state.busy && !state.retrying) || !state.supported || stopping;
        let (action_icon, action_label, action_variant) = if state.retrying {
            (
                IconName::X,
                tr(lang, "取消重连", "Cancel reconnect"),
                ButtonCustomVariant::new(cx)
                    .color(p.danger.opacity(theme::SEMANTIC_ALPHA))
                    .foreground(p.danger_text)
                    .hover(p.danger.opacity(theme::SEMANTIC_ALPHA + 0.08))
                    .active(p.danger.opacity(theme::SEMANTIC_ALPHA + 0.14)),
            )
        } else if stopping {
            (
                IconName::Square,
                tr(lang, "断开中", "Disconnecting"),
                ButtonCustomVariant::new(cx)
                    .color(p.panel_alt)
                    .foreground(p.muted_fg),
            )
        } else if active {
            (
                IconName::Square,
                tr(lang, "断开", "Disconnect"),
                ButtonCustomVariant::new(cx)
                    .color(p.danger.opacity(theme::SEMANTIC_ALPHA))
                    .foreground(p.danger_text)
                    .hover(p.danger.opacity(theme::SEMANTIC_ALPHA + 0.08))
                    .active(p.danger.opacity(theme::SEMANTIC_ALPHA + 0.14)),
            )
        } else if state.phase == MeshPhase::Failed {
            (
                IconName::RotateCw,
                tr(lang, "重试", "Retry"),
                ButtonCustomVariant::new(cx)
                    .color(p.accent)
                    .foreground(p.primary)
                    .hover(p.primary.opacity(theme::SEMANTIC_ALPHA + 0.06))
                    .active(p.primary.opacity(theme::SEMANTIC_ALPHA + 0.12)),
            )
        } else {
            (
                IconName::Play,
                tr(lang, "连接", "Connect"),
                ButtonCustomVariant::new(cx)
                    .color(p.accent)
                    .foreground(p.primary)
                    .hover(p.primary.opacity(theme::SEMANTIC_ALPHA + 0.06))
                    .active(p.primary.opacity(theme::SEMANTIC_ALPHA + 0.12)),
            )
        };

        let profile_id_for_action = state.selected_profile_id.clone();
        let is_retrying = state.retrying;
        let view_action = view.clone();
        let action_button = Button::new(SharedString::from(format!(
            "header-mesh-action-{}",
            provider.key()
        )))
        .compact()
        .h(px(26.))
        .px_2p5()
        .custom(action_variant)
        .disabled(action_disabled)
        .icon(action_icon)
        .label(action_label)
        .on_click(move |_, _, cx| {
            let _ = view_action.update(cx, |this, cx| {
                if is_retrying {
                    this.service.cancel_mesh_reconnect(&profile_id_for_action);
                } else if active {
                    this.service.mesh_disconnect(provider);
                } else {
                    this.service.mesh_connect(&profile_id_for_action);
                }
                this.sync(cx);
            });
        });

        let badge_dot = if matches!(state.phase, MeshPhase::Starting | MeshPhase::Stopping) {
            Spinner::new().small().color(badge_fg).into_any_element()
        } else {
            div()
                .size(px(6.))
                .rounded_full()
                .flex_shrink_0()
                .bg(badge_fg)
                .into_any_element()
        };

        let status_pill = div()
            .flex_shrink_0()
            .px_2()
            .py_0p5()
            .rounded_full()
            .bg(badge_bg)
            .flex()
            .items_center()
            .gap_1()
            .child(badge_dot)
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(badge_fg)
                    .child(status_label),
            );

        let profile_section = if state.profiles.len() <= 1 {
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .text_xs()
                .text_color(p.muted_fg)
                .child(tr(lang, "配置:", "Profile:"))
                .child(
                    div()
                        .min_w(px(0.))
                        .truncate()
                        .text_color(p.fg)
                        .child(state.selected_profile_name.clone()),
                )
        } else {
            let active_or_retrying = state.is_active_or_retrying();
            let chips = state.profiles.iter().map(|item| {
                let id = item.id.clone();
                let selected = item.selected;
                let view_chip = view.clone();
                let variant = if selected {
                    ButtonCustomVariant::new(cx)
                        .color(p.accent)
                        .foreground(p.primary)
                        .hover(p.primary.opacity(theme::SEMANTIC_ALPHA + 0.06))
                        .active(p.primary.opacity(theme::SEMANTIC_ALPHA + 0.12))
                } else {
                    ButtonCustomVariant::new(cx)
                        .foreground(p.muted_fg)
                        .hover(p.neutral_hover())
                        .active(p.neutral_active())
                };
                Button::new(SharedString::from(format!(
                    "header-mesh-profile-{}",
                    item.id
                )))
                .compact()
                .h(px(26.))
                .px_2()
                .border_1()
                .border_color(if selected {
                    p.primary.opacity(0.55)
                } else {
                    p.border
                })
                .custom(variant)
                .disabled(state.busy && active_or_retrying)
                .when(selected, |btn| btn.icon(IconName::Check))
                .label(item.name.clone())
                .on_click(move |_, _, cx| {
                    let _ = view_chip.update(cx, |this, cx| {
                        this.header_mesh_selected.insert(provider, id.clone());
                        if active_or_retrying && !selected {
                            this.service.mesh_connect(&id);
                        }
                        this.sync(cx);
                    });
                })
            });

            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap_1p5()
                .child(
                    div()
                        .text_xs()
                        .text_color(p.muted_fg)
                        .child(tr(lang, "配置:", "Profiles:")),
                )
                .children(chips)
        };

        let peers_summary: SharedString = match lang {
            Language::Zh => format!("设备 {}/{} 在线", state.peers_online, state.peers_total),
            Language::En => format!("{}/{} peers online", state.peers_online, state.peers_total),
        }
        .into();

        let footer_caption = if state.is_active_exit {
            tr(lang, "当前组网出口", "Active mesh exit")
        } else {
            match provider {
                MeshProvider::EasyTier => tr(lang, "EasyTier 异地组网", "EasyTier mesh"),
                MeshProvider::Tailscale => tr(lang, "Tailscale 异地组网", "Tailscale mesh"),
            }
        };

        let view_manage = view.clone();

        div()
            .w(px(theme::HEADER_MESH_POPOVER_WIDTH))
            .flex()
            .flex_col()
            .gap_2p5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .min_w(px(0.))
                            .flex_1()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Icon::new(provider_icon(provider))
                                    .size(px(15.))
                                    .text_color(p.muted_fg)
                                    .flex_shrink_0(),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.fg)
                                    .child(provider.label()),
                            )
                            .child(status_pill),
                    )
                    .child(action_button),
            )
            .child(profile_section)
            .when(state.phase == MeshPhase::Connected, |d| {
                let ip_display = state
                    .virtual_ip_display
                    .clone()
                    .unwrap_or_else(|| "—".to_string());
                let raw_ip = state.virtual_ip_raw.clone();
                let view_copy = view.clone();
                d.child(
                    div()
                        .rounded_md()
                        .bg(p.row_bg())
                        .px_2p5()
                        .py_2()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
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
                                        .gap_2()
                                        .child(div().text_xs().text_color(p.muted_fg).child(tr(
                                            lang,
                                            "虚拟 IP",
                                            "Virtual IP",
                                        )))
                                        .child(
                                            div()
                                                .min_w(px(0.))
                                                .truncate()
                                                .font_family(mono.clone())
                                                .text_xs()
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(p.fg)
                                                .child(ip_display),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .text_xs()
                                        .text_color(p.muted_fg)
                                        .child(peers_summary)
                                        .when_some(
                                            state.dns_suffix_display.clone(),
                                            |row, suffix| {
                                                row.child(
                                                    div()
                                                        .min_w(px(0.))
                                                        .truncate()
                                                        .child(format!("· {suffix}")),
                                                )
                                            },
                                        ),
                                ),
                        )
                        .when_some(raw_ip, |row, ip| {
                            row.child(
                                icon_button(
                                    SharedString::from(format!(
                                        "header-mesh-copy-ip-{}",
                                        provider.key()
                                    )),
                                    IconName::Copy,
                                    tr(lang, "复制虚拟 IP", "Copy virtual IP"),
                                    cx,
                                )
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(ip.clone()));
                                    let _ = view_copy.update(cx, |this, _| {
                                        this.service.toast(
                                            ToastLevel::Success,
                                            this.tr("已复制", "Copied"),
                                        );
                                    });
                                }),
                            )
                        }),
                )
            })
            .when(
                state.auth_action.is_some() || state.detail_notice.is_some(),
                |d| {
                    let is_danger = state
                        .detail_notice
                        .as_ref()
                        .is_some_and(|(danger, _)| *danger);
                    let (fg, bg) = if is_danger {
                        (p.danger_text, p.danger.opacity(theme::SEMANTIC_ALPHA))
                    } else {
                        (p.warn_text, p.warning.opacity(theme::SEMANTIC_ALPHA))
                    };
                    let notice_text = state.detail_notice.as_ref().map(|(_, text)| text.clone());
                    let auth = state.auth_action.clone();
                    let view_auth = view.clone();
                    d.child(
                        div()
                            .rounded_md()
                            .bg(bg)
                            .px_2p5()
                            .py_2()
                            .flex()
                            .flex_col()
                            .gap_1p5()
                            .when_some(notice_text, |box_div, text| {
                                box_div.child(
                                    div()
                                        .text_xs()
                                        .text_color(fg)
                                        .whitespace_normal()
                                        .child(text),
                                )
                            })
                            .when_some(auth, |box_div, (label, url)| {
                                box_div.child(
                                    div().flex().justify_end().child(
                                        Button::new(SharedString::from(format!(
                                            "header-mesh-auth-{}",
                                            provider.key()
                                        )))
                                        .compact()
                                        .h(px(26.))
                                        .px_2()
                                        .border_1()
                                        .border_color(p.control_border())
                                        .custom(
                                            ButtonCustomVariant::new(cx)
                                                .color(p.panel)
                                                .foreground(p.fg)
                                                .hover(p.neutral_hover())
                                                .active(p.neutral_active()),
                                        )
                                        .icon(IconName::ExternalLink)
                                        .label(label)
                                        .on_click(
                                            move |_, _, cx| {
                                                let _ = view_auth.update(cx, |this, _| {
                                                    this.service.open_url(&url);
                                                });
                                            },
                                        ),
                                    ),
                                )
                            }),
                    )
                },
            )
            .child(divider(&p))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(div().text_xs().text_color(p.muted_fg).child(footer_caption))
                    .child(
                        Button::new(SharedString::from(format!(
                            "header-mesh-manage-{}",
                            provider.key()
                        )))
                        .compact()
                        .ghost()
                        .h(px(26.))
                        .px_2()
                        .text_color(p.primary)
                        .on_click(move |_, _, cx| {
                            let _ = view_manage.update(cx, |this, cx| {
                                this.header_mesh_popover = None;
                                this.navigate(Page::Mesh, cx);
                            });
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .child(tr(lang, "前往组网管理", "Manage mesh"))
                                .child(Icon::new(IconName::ChevronRight).size(px(13.))),
                        ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{HeaderMeshState, HeaderMeshTone, summarize_mesh_trigger};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use xrose_core::mesh::{MeshPeer, MeshPhase, MeshProfile, MeshProvider, MeshRuntimeView};
    use xrose_core::model::{MeshExitTarget, WorkMode};
    use xrose_core::paths::AppPaths;
    use xrose_core::service::AppService;
    use xrose_core::settings::Language;

    struct TempRoot(PathBuf);

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn test_service() -> (TempRoot, AppService) {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(1);
        let dir = std::env::temp_dir().join(format!(
            "xrose-mesh-popover-test-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let service = AppService::with_paths(AppPaths::from_root(&dir)).unwrap();
        (TempRoot(dir), service)
    }

    fn sample_profile(id: &str, name: &str, provider: MeshProvider) -> MeshProfile {
        MeshProfile {
            id: id.to_string(),
            name: name.to_string(),
            provider,
            ..Default::default()
        }
    }

    #[test]
    fn unconfigured_provider_returns_none() {
        let (_dir, service) = test_service();
        let snapshot = service.snapshot();
        assert!(
            HeaderMeshState::from_snapshot(
                &snapshot,
                MeshProvider::EasyTier,
                false,
                None,
                Language::Zh
            )
            .is_none()
        );
        assert!(
            HeaderMeshState::from_snapshot(
                &snapshot,
                MeshProvider::Tailscale,
                false,
                None,
                Language::En
            )
            .is_none()
        );
    }

    #[test]
    fn phases_and_compact_labels_map_to_expected_tones_and_actions() {
        let (_dir, service) = test_service();
        let mut snapshot = service.snapshot();
        snapshot.mesh.profiles = vec![
            sample_profile("et-1", "Home-Lab", MeshProvider::EasyTier).summary(),
            sample_profile("ts-1", "Personal-Tailnet", MeshProvider::Tailscale).summary(),
        ];

        // Disconnected
        let et_idle = HeaderMeshState::from_snapshot(
            &snapshot,
            MeshProvider::EasyTier,
            false,
            None,
            Language::Zh,
        )
        .unwrap();
        let et_sum = summarize_mesh_trigger(&et_idle, false, Language::Zh);
        assert_eq!(et_sum.tone, HeaderMeshTone::Neutral);
        assert_eq!(et_sum.label, "EasyTier");
        assert_eq!(
            summarize_mesh_trigger(&et_idle, true, Language::Zh).label,
            "ET"
        );

        // Starting
        snapshot.mesh.networks = vec![MeshRuntimeView {
            profile_id: "et-1".into(),
            provider: MeshProvider::EasyTier,
            phase: MeshPhase::Starting,
            ..Default::default()
        }];
        let et_starting = HeaderMeshState::from_snapshot(
            &snapshot,
            MeshProvider::EasyTier,
            false,
            None,
            Language::Zh,
        )
        .unwrap();
        assert_eq!(
            summarize_mesh_trigger(&et_starting, false, Language::Zh).tone,
            HeaderMeshTone::Info
        );

        // Stopping
        snapshot.mesh.networks[0].phase = MeshPhase::Stopping;
        let et_stopping = HeaderMeshState::from_snapshot(
            &snapshot,
            MeshProvider::EasyTier,
            false,
            None,
            Language::En,
        )
        .unwrap();
        assert_eq!(
            summarize_mesh_trigger(&et_stopping, false, Language::En).tone,
            HeaderMeshTone::Info
        );

        // Retrying
        snapshot.mesh.networks.clear();
        snapshot.mesh_reconnecting.insert("et-1".into());
        let et_retrying = HeaderMeshState::from_snapshot(
            &snapshot,
            MeshProvider::EasyTier,
            false,
            None,
            Language::Zh,
        )
        .unwrap();
        let retry_sum = summarize_mesh_trigger(&et_retrying, false, Language::Zh);
        assert_eq!(retry_sum.tone, HeaderMeshTone::Warning);
        assert_eq!(retry_sum.label, "EasyTier · 重连中");
        assert_eq!(
            summarize_mesh_trigger(&et_retrying, true, Language::Zh).label,
            "ET"
        );

        // Tailscale NeedsLogin with login_url
        snapshot.mesh.networks = vec![MeshRuntimeView {
            profile_id: "ts-1".into(),
            provider: MeshProvider::Tailscale,
            phase: MeshPhase::NeedsLogin,
            login_url: Some("https://login.tailscale.com/a/example".into()),
            ..Default::default()
        }];
        let ts_login = HeaderMeshState::from_snapshot(
            &snapshot,
            MeshProvider::Tailscale,
            false,
            None,
            Language::Zh,
        )
        .unwrap();
        assert_eq!(
            ts_login.auth_action.as_ref().map(|(_, u)| u.as_str()),
            Some("https://login.tailscale.com/a/example")
        );
        let login_sum = summarize_mesh_trigger(&ts_login, false, Language::Zh);
        assert_eq!(login_sum.tone, HeaderMeshTone::Warning);
        assert_eq!(login_sum.label, "Tailscale · 待登录");

        // NeedsApproval on official control server
        snapshot.mesh.networks[0].phase = MeshPhase::NeedsApproval;
        snapshot.mesh.networks[0].login_url = None;
        let ts_approval = HeaderMeshState::from_snapshot(
            &snapshot,
            MeshProvider::Tailscale,
            false,
            None,
            Language::En,
        )
        .unwrap();
        assert_eq!(
            ts_approval.auth_action.as_ref().map(|(_, u)| u.as_str()),
            Some("https://login.tailscale.com/admin/machines")
        );
        let approval_sum = summarize_mesh_trigger(&ts_approval, false, Language::En);
        assert_eq!(approval_sum.tone, HeaderMeshTone::Warning);
        assert_eq!(approval_sum.label, "Tailscale · Approval");

        // Failed via mesh_connection_errors
        snapshot.mesh.networks.clear();
        snapshot
            .mesh_connection_errors
            .insert("ts-1".into(), "control plane timeout".into());
        let ts_failed = HeaderMeshState::from_snapshot(
            &snapshot,
            MeshProvider::Tailscale,
            false,
            None,
            Language::Zh,
        )
        .unwrap();
        let fail_sum = summarize_mesh_trigger(&ts_failed, false, Language::Zh);
        assert_eq!(fail_sum.tone, HeaderMeshTone::Danger);
        assert_eq!(fail_sum.label, "Tailscale · 异常");
        assert_eq!(
            summarize_mesh_trigger(&ts_failed, true, Language::Zh).label,
            "TS"
        );
    }

    #[test]
    fn privacy_mode_masks_ip_and_magic_dns_while_preserving_raw_ip() {
        let (_dir, service) = test_service();
        let mut snapshot = service.snapshot();
        snapshot.mesh.profiles =
            vec![sample_profile("ts-1", "Personal-Tailnet", MeshProvider::Tailscale).summary()];
        snapshot.mesh.networks = vec![MeshRuntimeView {
            profile_id: "ts-1".into(),
            provider: MeshProvider::Tailscale,
            phase: MeshPhase::Connected,
            addresses: vec!["100.64.0.12/32".into()],
            dns_suffix: Some("tail-demo.ts.net".into()),
            peers: vec![
                MeshPeer {
                    id: "p1".into(),
                    online: true,
                    ..Default::default()
                },
                MeshPeer {
                    id: "p2".into(),
                    online: false,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }];

        let visible = HeaderMeshState::from_snapshot(
            &snapshot,
            MeshProvider::Tailscale,
            false,
            None,
            Language::Zh,
        )
        .unwrap();
        assert_eq!(visible.virtual_ip_raw.as_deref(), Some("100.64.0.12"));
        assert_eq!(visible.virtual_ip_display.as_deref(), Some("100.64.0.12"));
        assert_eq!(
            visible.dns_suffix_display.as_deref(),
            Some("tail-demo.ts.net")
        );
        assert_eq!(visible.peers_online, 1);
        assert_eq!(visible.peers_total, 2);

        let masked = HeaderMeshState::from_snapshot(
            &snapshot,
            MeshProvider::Tailscale,
            true,
            None,
            Language::Zh,
        )
        .unwrap();
        assert_eq!(masked.virtual_ip_raw.as_deref(), Some("100.64.0.12"));
        assert_eq!(masked.virtual_ip_display.as_deref(), Some("100.*.*.*"));
        assert_eq!(masked.dns_suffix_display.as_deref(), Some("***.ts.net"));
        let tip = summarize_mesh_trigger(&masked, false, Language::Zh).tooltip;
        assert!(!tip.contains("100.64.0.12"));
        assert!(tip.contains("100.*.*.*"));
    }

    #[test]
    fn profile_selection_priority_and_exit_badge_behave_as_specified() {
        let (_dir, service) = test_service();
        let mut snapshot = service.snapshot();
        snapshot.mesh.profiles = vec![
            sample_profile("et-1", "First", MeshProvider::EasyTier).summary(),
            sample_profile("et-2", "Startup", MeshProvider::EasyTier).summary(),
            sample_profile("et-3", "Override", MeshProvider::EasyTier).summary(),
        ];
        snapshot.settings.startup_easytier = Some("et-2".into());

        // Startup preferred beats first profile
        let preferred = HeaderMeshState::from_snapshot(
            &snapshot,
            MeshProvider::EasyTier,
            false,
            None,
            Language::Zh,
        )
        .unwrap();
        assert_eq!(preferred.selected_profile_id, "et-2");

        // UI override beats startup preferred
        let overridden = HeaderMeshState::from_snapshot(
            &snapshot,
            MeshProvider::EasyTier,
            false,
            Some("et-3"),
            Language::Zh,
        )
        .unwrap();
        assert_eq!(overridden.selected_profile_id, "et-3");

        // Retrying beats UI override
        snapshot.mesh_reconnecting.insert("et-2".into());
        let retrying = HeaderMeshState::from_snapshot(
            &snapshot,
            MeshProvider::EasyTier,
            false,
            Some("et-3"),
            Language::Zh,
        )
        .unwrap();
        assert_eq!(retrying.selected_profile_id, "et-2");

        // Active runtime beats retrying and UI override, and active exit badge only shows when !compact
        snapshot.mesh.networks = vec![MeshRuntimeView {
            profile_id: "et-1".into(),
            provider: MeshProvider::EasyTier,
            phase: MeshPhase::Connected,
            addresses: vec!["10.126.126.2/24".into()],
            ..Default::default()
        }];
        snapshot.model.work_mode = WorkMode::Mesh;
        snapshot.model.mesh_mode.target = Some(MeshExitTarget {
            profile_id: "et-1".into(),
            provider: MeshProvider::EasyTier,
            peer_id: "peer-1".into(),
            name: "Home-Gateway".into(),
            address: "10.126.126.1".into(),
            gateway_id: "mesh-exit-et-1".into(),
        });
        let active = HeaderMeshState::from_snapshot(
            &snapshot,
            MeshProvider::EasyTier,
            false,
            Some("et-3"),
            Language::Zh,
        )
        .unwrap();
        assert_eq!(active.selected_profile_id, "et-1");
        assert!(active.is_active_exit);
        assert!(summarize_mesh_trigger(&active, false, Language::Zh).show_exit_badge);
        assert!(!summarize_mesh_trigger(&active, true, Language::Zh).show_exit_badge);
    }
}
