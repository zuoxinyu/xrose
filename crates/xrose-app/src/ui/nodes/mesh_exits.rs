//! Discovered exits select the host mesh mode independently of proxy-group membership.
use super::*;
use crate::ui::tr;
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
    pub(super) fn row_key(&self) -> String {
        xrose_core::service::HttpTestTarget::MeshExit {
            profile: self.profile.clone(),
            peer: self.peer.clone(),
        }
        .key()
    }

    pub(super) fn matches(&self, query: &str, hide_ips: bool) -> bool {
        self.name.to_lowercase().contains(query)
            || self.network.to_lowercase().contains(query)
            || self.provider.key().contains(query)
            || (!hide_ips && self.address.to_string().contains(query))
    }

    pub(super) fn detail(&self, hide_ips: bool) -> String {
        let address = if hide_ips {
            xrose_core::privacy::mask_ip(self.address)
        } else {
            self.address.to_string()
        };
        format!("{} · {}", self.network, address)
    }

    pub(super) fn row(
        &self,
        disabled: bool,
        hide_ips: bool,
        _lang: Language,
        p: &Palette,
        view: &AppView,
        window: &mut Window,
        cx: &mut Context<AppView>,
    ) -> impl IntoElement + use<> {
        let target = xrose_core::service::HttpTestTarget::MeshExit {
            profile: self.profile.clone(),
            peer: self.peer.clone(),
        };
        let mut node = Node::new(
            self.name.clone(),
            xrose_core::model::Protocol::Socks,
            self.address.to_string(),
            0,
        );
        node.id = target.key();
        let detail = self.detail(hide_ips);
        render_virtual_node_row(
            &node,
            VirtualExit::Mesh {
                profile: self.profile.clone(),
                peer: self.peer.clone(),
                detail,
                online: self.online,
                provider: self.provider,
                resumable: self.resumable,
            },
            self.selected,
            disabled || (!self.online && !self.resumable),
            view,
            p,
            window,
            cx,
        )
    }
}

pub(super) fn choices(
    mesh: &xrose_core::mesh::MeshSnapshot,
    model: &xrose_core::model::AppModel,
    hide_ips: bool,
    lang: Language,
) -> Vec<Choice> {
    let suffixes = mesh.magic_dns_suffixes();
    let mut choices = Vec::new();
    for (profile, peer, address) in mesh.exits() {
        let gateway = format!("mesh-exit-{}", profile.id);
        let name = if peer.name.is_empty() {
            tr(lang, "出口设备", "Exit device").to_string()
        } else if hide_ips {
            xrose_core::privacy::mask_magic_dns_name(&peer.name, &suffixes)
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
                xrose_core::privacy::mask_server_in_label(
                    &target.name,
                    &target.address,
                    "•••",
                    &suffixes,
                )
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
        assert_eq!(offline.name, "192.*.*.*");
        assert_eq!(offline.detail(true), "异地组网 · 192.*.*.*");
        assert!(!offline.matches("192.0.2.6", true));
        assert!(offline.matches("easytier", true));
    }

    #[test]
    fn privacy_mode_masks_magic_dns_and_ip_in_exit_choices() {
        let profile = MeshProfile {
            name: "Office net".into(),
            provider: xrose_core::mesh::MeshProvider::Tailscale,
            ..Default::default()
        }
        .summary();
        let peer = MeshPeer {
            id: "ts-a".into(),
            name: "desktop.alpha-bravo.ts.net".into(),
            offers_exit: true,
            online: true,
            addresses: vec!["100.64.0.9".into()],
            ..Default::default()
        };
        let mesh = MeshSnapshot {
            profiles: vec![profile.clone()],
            networks: vec![MeshRuntimeView {
                profile_id: profile.id.clone(),
                provider: profile.provider,
                phase: MeshPhase::Connected,
                dns_suffix: Some("alpha-bravo.ts.net".into()),
                peers: vec![peer],
                ..Default::default()
            }],
            ..Default::default()
        };
        let model = AppModel::default();
        let hidden = choices(&mesh, &model, true, Language::En);
        assert_eq!(hidden.len(), 1);
        assert_eq!(hidden[0].name, "desktop.***.ts.net");
        assert_eq!(hidden[0].detail(true), "Office net · 100.*.*.*");
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
