//! Discovered exits select the host mesh mode independently of proxy-group membership.
use super::*;
use crate::ui::{
    theme::{DISABLED_OPACITY, NODE_ROW_HEIGHT},
    tr,
};
use std::net::IpAddr;
use xrose_core::mesh::MeshProvider;

pub(super) struct Choice {
    profile: String,
    peer: String,
    name: String,
    network: String,
    provider: MeshProvider,
    address: IpAddr,
    online: bool,
    selected: bool,
    resumable: bool,
    pub(super) gateway: String,
}

impl Choice {
    pub(super) fn matches(&self, query: &str, hide_ips: bool) -> bool {
        self.name.to_lowercase().contains(query)
            || self.network.to_lowercase().contains(query)
            || self.provider.key().contains(query)
            || (!hide_ips && self.address.to_string().contains(query))
    }

    pub(super) fn row(
        &self,
        disabled: bool,
        hide_ips: bool,
        lang: Language,
        p: &Palette,
        cx: &mut Context<AppView>,
    ) -> impl IntoElement + use<> {
        let profile = self.profile.clone();
        let peer = self.peer.clone();
        let disabled = disabled || (!self.online && !self.resumable);
        let selected = self.selected;
        let primary = p.primary;
        let mut detail = format!("{} · {}", self.network, self.provider.label());
        if !hide_ips {
            detail.push_str(&format!(" · {}", self.address));
        }
        let hover = p.panel_alt;
        let select = cx.listener(move |this, _: &ClickEvent, _, cx| {
            if !disabled {
                this.service
                    .select_mesh_exit_mode(profile.clone(), peer.clone());
                this.sync(cx);
            }
        });
        gpui_base::Radio::new(SharedString::from(format!(
            "exit-{}-{}",
            self.profile, self.peer
        )))
        .checked(selected)
        .disabled(disabled)
        .accessibility_label(self.name.clone())
        .on_change(move |_, event, window, cx| select(event, window, cx))
        .tooltip(move |window, cx| {
            Tooltip::new(tr(
                lang,
                "进入组网模式，由此出口接管系统流量。",
                "Use this exit for system traffic in Mesh mode.",
            ))
            .build(window, cx)
        })
        .relative()
        .flex()
        .items_center()
        .gap_3()
        .pl_4()
        .pr_3()
        .py_2()
        .min_h(px(NODE_ROW_HEIGHT))
        .border_1()
        .border_color(if selected {
            p.border.opacity(0.)
        } else {
            p.border
        })
        .rounded_lg()
        .bg(if selected { p.accent } else { p.bg })
        .focus_visible(move |row| row.border_color(primary))
        .when(selected, |row| row.child(selection_bar(primary)))
        .when(disabled, |d| d.opacity(DISABLED_OPACITY))
        .when(!disabled && !selected, |d| {
            d.cursor_pointer().hover(move |d| d.bg(hover))
        })
        .child(div().w(px(14.)).flex_shrink_0())
        .child(radio_indicator(
            selected,
            if selected { primary } else { p.muted_fg },
            primary,
        ))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
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
                                .truncate()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(self.name.clone()),
                        )
                        .child(pill(tr(lang, "组网出口", "Mesh exit"), p.primary, p.panel)),
                )
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(p.muted_fg)
                        .child(detail),
                ),
        )
        .when(!self.online, |d| {
            d.child(pill(tr(lang, "离线", "Offline"), p.muted_fg, p.panel_alt))
        })
    }
}

pub(super) fn choices(
    mesh: &xrose_core::mesh::MeshSnapshot,
    model: &xrose_core::model::AppModel,
    hide_ips: bool,
    lang: Language,
) -> Vec<Choice> {
    let mut choices = Vec::new();
    for (profile, peer, address) in mesh.exits() {
        let gateway = format!("mesh-exit-{}", profile.id);
        let name = if peer.name.is_empty() || (hide_ips && peer.name.parse::<IpAddr>().is_ok()) {
            tr(lang, "出口设备", "Exit device").to_string()
        } else {
            peer.name.clone()
        };
        let target = model
            .mesh_mode
            .target
            .as_ref()
            .is_some_and(|target| target.profile_id == profile.id && target.peer_id == peer.id);
        choices.push(Choice {
            profile: profile.id.clone(),
            peer: peer.id.clone(),
            name,
            network: profile.name.clone(),
            provider: profile.provider,
            address,
            online: peer.online,
            selected: target && model.work_mode == xrose_core::model::WorkMode::Mesh,
            resumable: target,
            gateway,
        });
    }
    if let Some(target) = model.mesh_mode.target.as_ref().filter(|target| {
        !choices
            .iter()
            .any(|choice| choice.profile == target.profile_id && choice.peer == target.peer_id)
    }) {
        if let (Some(profile), Ok(address)) = (
            mesh.profiles
                .iter()
                .find(|profile| profile.id == target.profile_id),
            target.address.parse(),
        ) {
            let name = if hide_ips {
                target.name.replace(&target.address, "•••")
            } else {
                target.name.clone()
            };
            choices.push(Choice {
                profile: target.profile_id.clone(),
                peer: target.peer_id.clone(),
                name,
                network: profile.name.clone(),
                provider: target.provider,
                address,
                online: false,
                selected: model.work_mode == xrose_core::model::WorkMode::Mesh,
                resumable: true,
                gateway: target.gateway_id.clone(),
            });
        }
    }
    choices.sort_by(|a, b| (&a.network, &a.name, a.address).cmp(&(&b.network, &b.name, b.address)));
    choices
}

#[cfg(test)]
mod tests {
    use super::choices;
    use xrose_core::{
        mesh::{MeshPeer, MeshPhase, MeshProfile, MeshRuntimeView, MeshSnapshot},
        model::{AppModel, Node},
        settings::Language,
    };

    #[test]
    fn discovered_exits_remain_independent_of_saved_proxy_gateways() {
        let profile = MeshProfile::default().summary();
        let selected = MeshPeer {
            id: "a".into(),
            name: "exit-a".into(),
            offers_exit: true,
            exit_selected: true,
            online: true,
            addresses: vec!["192.0.2.5".into()],
            ..Default::default()
        };
        let other = MeshPeer {
            id: "b".into(),
            name: "192.0.2.6".into(),
            exit_selected: false,
            online: false,
            addresses: vec!["192.0.2.6".into()],
            ..selected.clone()
        };
        let mesh = MeshSnapshot {
            profiles: vec![profile.clone()],
            networks: vec![MeshRuntimeView {
                profile_id: profile.id.clone(),
                phase: MeshPhase::Connected,
                peers: vec![selected, other],
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut model = AppModel::default();
        assert_eq!(choices(&mesh, &model, false, Language::En).len(), 2);
        let mut gateway = Node::new(
            "gateway",
            xrose_core::model::Protocol::Socks,
            "127.0.0.1",
            19081,
        );
        gateway.id = format!("mesh-exit-{}", profile.id);
        model.add_nodes([gateway]);
        let options = choices(&mesh, &model, true, Language::En);
        assert_eq!(options.len(), 2);
        let offline = options.iter().find(|choice| choice.peer == "b").unwrap();
        assert!(!offline.online);
        assert!(!offline.matches("192.0.2.6", true));
        assert!(offline.matches("easytier", true));
    }

    #[test]
    fn disconnected_saved_exit_remains_visible_and_resumable() {
        let profile = MeshProfile::default().summary();
        let mut model = AppModel::default();
        model.work_mode = xrose_core::model::WorkMode::Mesh;
        model.mesh_mode.target = Some(xrose_core::model::MeshExitTarget {
            profile_id: profile.id.clone(),
            peer_id: "fixture-peer".into(),
            provider: profile.provider,
            name: "Fixture exit".into(),
            address: "192.0.2.5".into(),
            gateway_id: format!("mesh-exit-{}", profile.id),
        });
        let mesh = MeshSnapshot {
            profiles: vec![profile],
            ..Default::default()
        };
        let options = choices(&mesh, &model, false, Language::En);
        assert_eq!(options.len(), 1);
        assert!(options[0].selected);
        assert!(options[0].resumable);
        assert!(!options[0].online);
    }
}
