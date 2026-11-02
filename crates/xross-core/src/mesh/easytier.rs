//! The statically linked EasyTier backend. Initialized only in mesh-worker.
use super::{MeshPeer, MeshPhase, MeshProfile, MeshProvider, MeshRuntimeView};
use crate::{Error, Result};
use easytier::{
    common::config::{
        ConfigFileControl, ConfigLoader, NetworkIdentity, PeerConfig, TomlConfigLoader,
    },
    instance_manager::NetworkInstanceManager,
};

pub fn configuration(p: &MeshProfile) -> Result<TomlConfigLoader> {
    p.validate()?;
    let c = TomlConfigLoader::default();
    c.set_id(uuid::Uuid::parse_str(&p.id).map_err(|_| Error::config("组网 ID 无效"))?);
    c.set_inst_name(p.name.clone());
    c.set_hostname(Some(p.hostname.clone()));
    c.set_network_identity(NetworkIdentity::new(
        p.network_name.clone(),
        p.network_secret.0.clone(),
    ));
    c.set_dhcp(p.ipv4.is_empty());
    if !p.ipv4.is_empty() {
        c.set_ipv4(Some(
            p.ipv4.parse().map_err(|_| Error::config("虚拟地址无效"))?,
        ));
    }
    c.set_peers(
        p.peers
            .iter()
            .map(|p| {
                Ok(PeerConfig {
                    uri: url::Url::parse(p).map_err(|_| Error::config("入口地址无效"))?,
                    peer_public_key: None,
                })
            })
            .collect::<Result<Vec<_>>>()?,
    );
    c.set_listeners(Vec::new());
    c.set_exit_nodes(Vec::new());
    c.clear_proxy_cidrs();
    // Some(empty) is intentional: upstream uses it to suppress automatic
    // installation of every remotely advertised subnet, including /0.
    c.set_routes(Some(
        p.remote_subnets
            .iter()
            .map(|n| {
                n.parse()
                    .map_err(|_| Error::config("EasyTier 首期仅支持 IPv4 远端网段"))
            })
            .collect::<Result<Vec<_>>>()?,
    ));
    let mut flags = c.get_flags();
    flags.dev_name = format!("xross-mesh-et-{}", &p.id[..8]);
    flags.accept_dns = false;
    flags.enable_exit_node = false;
    flags.proxy_forward_by_system = false;
    flags.disable_upnp = true;
    flags.enable_udp_broadcast_relay = false;
    flags.enable_ipv6 = false;
    flags.relay_network_whitelist = String::new();
    flags.bind_device = true;
    c.set_flags(flags);
    Ok(c)
}
pub struct Backend {
    manager: NetworkInstanceManager,
    profile: MeshProfile,
    started: std::time::Instant,
}
impl Backend {
    pub fn start(profile: MeshProfile) -> Result<Self> {
        let manager = NetworkInstanceManager::new();
        manager
            .run_network_instance(
                configuration(&profile)?,
                true,
                ConfigFileControl::STATIC_CONFIG,
            )
            .map_err(|_| Error::other("EasyTier 启动失败，请检查虚拟网卡驱动、权限和网络配置"))?;
        Ok(Self {
            manager,
            profile,
            started: std::time::Instant::now(),
        })
    }
    pub async fn snapshot(&self) -> Result<MeshRuntimeView> {
        let infos = self
            .manager
            .collect_network_infos()
            .await
            .map_err(|_| Error::other("无法读取 EasyTier 状态"))?;
        let Some(info) = infos.values().next() else {
            if self.started.elapsed().as_secs() < 30 {
                return Ok(MeshRuntimeView {
                    profile_id: self.profile.id.clone(),
                    provider: MeshProvider::EasyTier,
                    phase: MeshPhase::Starting,
                    ..Default::default()
                });
            }
            return Err(Error::other(
                "EasyTier 实例启动超时，请检查虚拟网卡驱动和权限",
            ));
        };
        if info.error_msg.is_some() || (!info.running && self.started.elapsed().as_secs() > 30) {
            return Err(Error::other(
                "EasyTier 实例未就绪，请检查网络密钥、入口节点及虚拟网卡驱动",
            ));
        }
        let mut state = MeshRuntimeView {
            profile_id: self.profile.id.clone(),
            provider: MeshProvider::EasyTier,
            phase: MeshPhase::Starting,
            interface: info.dev_name.clone(),
            routes: self.profile.remote_subnets.clone(),
            updated_at: crate::util::now_unix(),
            ..Default::default()
        };
        if let Some(v4) = info
            .my_node_info
            .as_ref()
            .and_then(|n| n.virtual_ipv4.as_ref())
        {
            if let Some(address) = &v4.address {
                state.addresses.push(address.to_string());
            }
            if let Ok(net) = v4.to_string().parse::<ipnet::IpNet>() {
                state.routes.push(net.trunc().to_string());
            }
            state.phase = MeshPhase::Connected;
        }
        state.peers = info
            .routes
            .iter()
            .filter(|route| route.cost > 0)
            .take(1024)
            .map(|r| MeshPeer {
                id: r.peer_id.to_string(),
                name: r.hostname.clone(),
                addresses: r
                    .ipv4_addr
                    .as_ref()
                    .and_then(|a| a.address.as_ref())
                    .map(|a| vec![a.to_string()])
                    .unwrap_or_default(),
                online: true,
                latency_ms: None,
                connection: Some(if r.cost <= 1 { "direct" } else { "relay" }.into()),
            })
            .collect();
        if state.peers.is_empty() {
            state.phase = MeshPhase::Starting;
            state.warning = Some(
                "正在等待已认证的组网设备；长时间没有设备时请检查网络名称、密钥和入口节点".into(),
            );
        }
        state.routes.sort();
        state.routes.dedup();
        Ok(state)
    }
    pub fn stop(&self) {
        let _ = self.manager.delete_network_instance(vec![
            uuid::Uuid::parse_str(&self.profile.id).expect("validated ID")
        ]);
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_exit_dns_or_unsolicited_remote_routes() {
        let p = MeshProfile {
            network_name: "test".into(),
            network_secret: super::super::MeshSecret("fake-test-key".into()),
            peers: vec!["tcp://127.0.0.1:11010".into()],
            ..Default::default()
        };
        let c = configuration(&p).unwrap();
        assert_eq!(c.get_routes(), Some(vec![]));
        assert!(c.get_exit_nodes().is_empty());
        assert!(c.get_proxy_cidrs().is_empty());
        assert!(!c.get_flags().accept_dns);
        assert!(!c.get_flags().enable_exit_node);
        assert!(c.get_flags().disable_upnp);
    }
}
