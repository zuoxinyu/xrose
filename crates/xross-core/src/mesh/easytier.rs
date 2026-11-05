//! The statically linked EasyTier backend. Initialized only in mesh-worker.
use super::{MeshPeer, MeshPhase, MeshProfile, MeshProvider, MeshRuntimeView};
use crate::{Error, Result};
use easytier::{
    common::config::{ConfigLoader, NetworkIdentity, PeerConfig, TomlConfigLoader},
    common::global_ctx::GlobalCtxEvent,
    instance::instance::Instance,
    proto::acl::{Acl, AclV1, GroupIdentity, GroupInfo},
};

// EasyTier 2.6 does not publish enable_exit_node in route capabilities.
// Announce Xross exits via a network-scoped, signed group membership.
const EXIT_GROUP: &str = "xross-exit-node-v1";

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
    c.set_exit_nodes(if p.exit_node.is_empty() {
        Vec::new()
    } else {
        vec![
            p.exit_node
                .parse()
                .map_err(|_| Error::config("出口节点地址无效"))?,
        ]
    });
    c.clear_proxy_cidrs();
    for subnet in &p.advertised_subnets {
        c.add_proxy_cidr(
            subnet.parse().map_err(|_| Error::config("发布子网无效"))?,
            None,
        )
        .map_err(|_| Error::config("发布子网无效"))?;
    }
    // Some(empty) is intentional: upstream uses it to suppress automatic
    // installation of every remotely advertised subnet, including /0.
    c.set_routes(Some(
        p.remote_subnets
            .iter()
            .map(|n| {
                n.parse()
                    .map_err(|_| Error::config("EasyTier 远端网段仅支持 IPv4"))
            })
            .collect::<Result<Vec<_>>>()?,
    ));
    let mut flags = c.get_flags();
    flags.dev_name = p.interface_name();
    flags.accept_dns = false;
    flags.enable_exit_node = p.advertise_exit_node;
    let options = &p.easytier;
    let local = &options.device;
    flags.enable_encryption = !options.disable_encryption;
    flags.latency_first = options.latency_first;
    flags.disable_p2p = options.p2p == super::P2pPolicy::Disabled;
    flags.lazy_p2p = options.p2p == super::P2pPolicy::OnDemand;
    flags.need_p2p = options.request_p2p;
    flags.p2p_only = options.p2p_only;
    flags.enable_kcp_proxy = options.tcp_acceleration == super::TcpAcceleration::Kcp;
    flags.enable_quic_proxy = options.tcp_acceleration == super::TcpAcceleration::Quic;
    if let Some(mtu) = options.mtu {
        flags.mtu = u32::from(mtu);
    }
    flags.use_smoltcp = local.userspace_stack;
    flags.disable_kcp_input = !local.accept_kcp;
    flags.disable_quic_input = !local.accept_quic;
    flags.disable_tcp_hole_punching = !local.tcp_hole_punching;
    flags.disable_udp_hole_punching = !local.udp_hole_punching;
    flags.disable_sym_hole_punching = !local.symmetric_nat_punching;
    flags.multi_thread = local.multi_thread;
    flags.private_mode = local.private_mode;
    flags.relay_all_peer_rpc = local.relay_peer_rpc;
    flags.proxy_forward_by_system = local.system_forwarding;
    flags.disable_upnp = !local.port_mapping;
    flags.enable_udp_broadcast_relay = local.udp_broadcast_relay;
    flags.enable_ipv6 = options.ipv6_transport;
    flags.relay_network_whitelist = String::new();
    flags.bind_device = local.bind_physical;
    c.set_flags(flags);
    let exit_group = GroupIdentity {
        group_name: EXIT_GROUP.into(),
        group_secret: crate::util::sha256_bytes(
            format!("{EXIT_GROUP}:{}", p.network_secret.0).as_bytes(),
        ),
    };
    c.set_acl(Some(Acl {
        acl_v1: Some(AclV1 {
            group: Some(GroupInfo {
                declares: vec![exit_group],
                members: if p.advertise_exit_node {
                    vec![EXIT_GROUP.into()]
                } else {
                    Vec::new()
                },
            }),
            ..Default::default()
        }),
    }));
    Ok(c)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectionFailure {
    Authentication,
    HandshakeClosed,
    HandshakeTimeout,
    Handshake,
    Resolution,
    Refused,
    Timeout,
    Other,
}
impl ConnectionFailure {
    // Upstream errors can contain credentials and URLs. Retain only a category.
    fn classify(error: &str) -> Self {
        let error = error.to_ascii_lowercase();
        if error.contains("secretkeyerror") || error.contains("authentication failed") {
            Self::Authentication
        } else if error.contains("handshake")
            && (error.contains("closed") || error.contains("reset"))
        {
            Self::HandshakeClosed
        } else if error.contains("handshake")
            && (error.contains("timeout") || error.contains("timed out"))
        {
            Self::HandshakeTimeout
        } else if error.contains("handshake") || error.contains("noiseerror") {
            Self::Handshake
        } else if error.contains("resolve") || error.contains("dns") {
            Self::Resolution
        } else if error.contains("refused") {
            Self::Refused
        } else if error.contains("timeout")
            || error.contains("timed out")
            || error.contains("elapsed")
        {
            Self::Timeout
        } else {
            Self::Other
        }
    }
    fn message(self) -> &'static str {
        match self {
            Self::Authentication => "EasyTier 入口认证失败，请检查网络名称和密钥",
            Self::HandshakeClosed => {
                "EasyTier 入口在握手完成前关闭连接，请核对入口协议、端口、网络名称和对端日志"
            }
            Self::HandshakeTimeout => "EasyTier 入口未响应握手，请核对入口协议、端口及对端版本",
            Self::Handshake => "EasyTier 入口握手未完成，请检查对端版本、网络名称和密钥",
            Self::Resolution => "EasyTier 入口域名解析失败，请检查入口地址和 DNS",
            Self::Refused => "EasyTier 入口拒绝连接，请检查对端监听端口和防火墙",
            Self::Timeout => "EasyTier 入口连接超时，请检查入口地址和网络可达性",
            Self::Other => "EasyTier 入口连接失败，请检查入口地址、对端配置和网络可达性",
        }
    }
}

#[derive(Default, Clone)]
struct StartupObservation {
    interface: String,
    tun_failed: bool,
    broadcast_relay_failed: bool,
    peer_connected: bool,
    connection_failure: Option<ConnectionFailure>,
}
impl StartupObservation {
    fn observe(&mut self, event: GlobalCtxEvent) {
        match event {
            GlobalCtxEvent::TunDeviceReady(name) => {
                self.interface = name;
                self.tun_failed = false;
            }
            GlobalCtxEvent::TunDeviceError(_) => self.tun_failed = true,
            GlobalCtxEvent::UdpBroadcastRelayStartResult { error, .. } => {
                self.broadcast_relay_failed = error.is_some()
            }
            GlobalCtxEvent::PeerConnAdded(_) => {
                self.peer_connected = true;
                self.connection_failure = None;
            }
            GlobalCtxEvent::ConnectError(_, _, error) => {
                self.connection_failure = Some(ConnectionFailure::classify(&error));
            }
            _ => {}
        }
    }
    fn pending_message(&self, has_address: bool) -> &'static str {
        if self.tun_failed {
            "EasyTier 虚拟网卡初始化失败，请检查提权助手与系统网卡权限"
        } else if !has_address {
            if let Some(failure) = self.connection_failure {
                failure.message()
            } else if self.peer_connected {
                "EasyTier 已连接入口，但未分配虚拟 IPv4；请确认同网络设备在线，或填写未占用的静态 IPv4"
            } else {
                "EasyTier 尚未连接入口，无法自动分配虚拟 IPv4；请检查入口地址和对端状态"
            }
        } else {
            "EasyTier 虚拟网卡未就绪，请检查提权助手与系统网卡权限"
        }
    }
}

pub struct Backend {
    instance: Instance,
    profile: MeshProfile,
    gateway: Option<super::easytier_gateway::Gateway>,
    forward_gateway: Option<(super::easytier_gateway::Gateway, super::forwards::Forwards)>,
    started: std::time::Instant,
    observation: std::sync::Arc<parking_lot::Mutex<StartupObservation>>,
    _events: tokio_util::task::AbortOnDropHandle<()>,
}
impl Backend {
    pub async fn start(profile: MeshProfile) -> Result<Self> {
        let mut instance = Instance::new(configuration(&profile)?);
        let observation =
            std::sync::Arc::new(parking_lot::Mutex::new(StartupObservation::default()));
        let observed = observation.clone();
        let mut receiver = instance.get_global_ctx().subscribe();
        let events = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            loop {
                match receiver.recv().await {
                    Ok(event) => observed.lock().observe(event),
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    _ => {}
                }
            }
        }));
        instance
            .run()
            .await
            .map_err(|_| Error::other("EasyTier 启动失败，请检查虚拟网卡权限和网络配置"))?;
        let mut result = Self {
            instance,
            profile,
            gateway: None,
            forward_gateway: None,
            started: std::time::Instant::now(),
            observation,
            _events: events,
        };
        if result.profile.outbound_port != 0 || !result.profile.port_forwards.is_empty() {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(40);
            let address = loop {
                if let Some(ip) = result.instance.get_global_ctx().get_ipv4() {
                    break ip;
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(Error::other(
                        result.observation.lock().pending_message(false),
                    ));
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            };
            if result.profile.outbound_port != 0 {
                result.gateway = Some(
                    super::easytier_gateway::Gateway::start(
                        result.instance.get_peer_manager(),
                        address.to_string(),
                        result.profile.outbound_port,
                        result.profile.outbound_secret.clone(),
                        super::easytier_gateway::GatewayRoute::SelectedExit,
                    )
                    .await?,
                );
            }
            let rules = result.profile.port_forwards.clone();
            result.set_forwards(&rules).await?;
        }
        Ok(result)
    }
    pub(super) async fn set_forwards(&mut self, rules: &[super::PortForward]) -> Result<()> {
        if self.forward_gateway.is_none() && !rules.is_empty() {
            let address = self
                .instance
                .get_global_ctx()
                .get_ipv4()
                .ok_or_else(|| Error::other("请等待 EasyTier 获得虚拟地址后重试"))?;
            let password = super::MeshSecret(uuid::Uuid::new_v4().simple().to_string());
            let gateway = super::easytier_gateway::Gateway::start(
                self.instance.get_peer_manager(),
                address.to_string(),
                0,
                password.clone(),
                super::easytier_gateway::GatewayRoute::Mesh,
            )
            .await?;
            let forwards = super::forwards::Forwards::new(gateway.listen, password);
            self.forward_gateway = Some((gateway, forwards));
        }
        if let Some((_, forwards)) = &mut self.forward_gateway {
            forwards.apply(rules).await?;
        }
        Ok(())
    }
    pub async fn snapshot(&self) -> Result<MeshRuntimeView> {
        let ctx = self.instance.get_global_ctx();
        let observation = self.observation.lock().clone();
        if observation.tun_failed {
            return Err(Error::other(
                observation.pending_message(ctx.get_ipv4().is_some()),
            ));
        }
        let mut state = MeshRuntimeView {
            profile_id: self.profile.id.clone(),
            provider: MeshProvider::EasyTier,
            phase: MeshPhase::Starting,
            interface: observation.interface.clone(),
            routes: self.profile.remote_subnets.clone(),
            updated_at: crate::util::now_unix(),
            ..Default::default()
        };
        if let Some(ip) = ctx.get_ipv4() {
            if let Some(gateway) = &self.gateway {
                gateway.check_address(ip.address().into())?;
            }
            if let Some((gateway, _)) = &self.forward_gateway {
                gateway.check_address(ip.address().into())?;
            }
            state.addresses.push(ip.address().to_string());
            if let Ok(net) = ip.to_string().parse::<ipnet::IpNet>() {
                state.routes.push(net.trunc().to_string());
            }
            if !state.interface.is_empty() {
                state.phase = MeshPhase::Connected;
            }
        }
        if state.phase != MeshPhase::Connected {
            let message = observation.pending_message(!state.addresses.is_empty());
            if self.started.elapsed().as_secs() > 45 {
                return Err(Error::other(message));
            }
            state.warning = Some(message.into());
        } else if observation.broadcast_relay_failed {
            state.warning = Some("UDP 广播中继未启动，请检查物理网卡和提权助手权限".into());
        }
        let manager = self.instance.get_peer_manager();
        let route = manager.get_route();
        let routes = manager.list_routes().await;
        if let Some(gateway) = &self.gateway {
            let target = self.profile.exit_node.parse::<std::net::Ipv4Addr>().ok();
            let selected = routes.iter().find(|r| {
                r.cost > 0
                    && r.peer_id != manager.my_peer_id()
                    && r.ipv4_addr
                        .and_then(|a| a.address)
                        .map(std::net::Ipv4Addr::from)
                        == target
            });
            gateway.set_exit_peer(selected.map(|r| r.peer_id));
        }
        state.peers = routes
            .iter()
            .filter(|r| r.cost > 0 && r.peer_id != manager.my_peer_id())
            .take(1024)
            .map(|r| {
                let addresses: Vec<String> = r
                    .ipv4_addr
                    .as_ref()
                    .and_then(|a| a.address.as_ref())
                    .map(|a| a.to_string())
                    .into_iter()
                    .collect();
                let exit_selected = addresses.contains(&self.profile.exit_node);
                let offers_exit = exit_selected
                    || r.proxy_cidrs.iter().any(|cidr| cidr == "0.0.0.0/0")
                    || route
                        .get_peer_groups(r.peer_id)
                        .iter()
                        .any(|g| g == EXIT_GROUP);
                MeshPeer {
                    id: r.peer_id.to_string(),
                    name: r.hostname.clone(),
                    addresses,
                    subnets: r
                        .proxy_cidrs
                        .iter()
                        .filter_map(|s| s.parse::<ipnet::IpNet>().ok())
                        .filter(|n| n.prefix_len() != 0)
                        .map(|n| n.trunc().to_string())
                        .collect(),
                    client_version: r.version.clone(),
                    online: true,
                    offers_exit,
                    exit_selected,
                    latency_ms: None,
                    connection: Some(if r.cost <= 1 { "direct" } else { "relay" }.into()),
                    ..Default::default()
                }
            })
            .collect();
        state.routes.sort();
        state.routes.dedup();
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_diagnostics_distinguish_connection_allocation_and_tun() {
        let mut observation = StartupObservation::default();
        observation.observe(GlobalCtxEvent::ConnectError(
            "tcp://private.example:11010".into(),
            "V4".into(),
            "SecretKeyError(fake-private-key)".into(),
        ));
        assert_eq!(
            observation.connection_failure,
            Some(ConnectionFailure::Authentication)
        );
        assert!(observation.pending_message(false).contains("认证失败"));
        assert!(
            !observation
                .pending_message(false)
                .contains("fake-private-key")
        );
        assert!(
            !observation
                .pending_message(false)
                .contains("private.example")
        );
        observation.observe(GlobalCtxEvent::PeerConnAdded(Default::default()));
        assert!(
            observation
                .pending_message(false)
                .contains("未分配虚拟 IPv4")
        );
        assert!(observation.connection_failure.is_none());
        observation.observe(GlobalCtxEvent::TunDeviceError("private error".into()));
        assert!(
            observation
                .pending_message(false)
                .contains("网卡初始化失败")
        );
        observation.observe(GlobalCtxEvent::TunDeviceReady("utun42".into()));
        assert!(!observation.tun_failed);
        assert_eq!(observation.interface, "utun42");
    }
    #[test]
    fn connection_errors_are_categorized_without_raw_output() {
        for (input, expected) in [
            (
                "wait handshake timeout",
                ConnectionFailure::HandshakeTimeout,
            ),
            (
                "conn closed during wait handshake response",
                ConnectionFailure::HandshakeClosed,
            ),
            ("failed to resolve domain", ConnectionFailure::Resolution),
            ("ConnectionRefused", ConnectionFailure::Refused),
            ("connect timeout after 10s", ConnectionFailure::Timeout),
            ("private upstream data", ConnectionFailure::Other),
        ] {
            assert_eq!(ConnectionFailure::classify(input), expected);
        }
    }
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
        assert!(c.get_secure_mode().is_none());
        assert!(c.get_flags().enable_encryption);
        let group = c.get_acl().unwrap().acl_v1.unwrap().group.unwrap();
        assert!(group.members.is_empty());
        assert_eq!(group.declares[0].group_name, EXIT_GROUP);
    }

    #[test]
    fn advanced_options_reach_the_embedded_instance_without_changing_route_policy() {
        use super::super::{EasyTierDeviceOptions, EasyTierOptions, P2pPolicy, TcpAcceleration};
        let mut p = MeshProfile {
            network_name: "fixture".into(),
            network_secret: super::super::MeshSecret("fixture".into()),
            peers: vec!["udp://192.0.2.1:11010".into()],
            easytier: EasyTierOptions {
                p2p: P2pPolicy::OnDemand,
                p2p_only: true,
                request_p2p: true,
                latency_first: true,
                ipv6_transport: true,
                tcp_acceleration: TcpAcceleration::Kcp,
                disable_encryption: true,
                mtu: Some(1240),
                device: EasyTierDeviceOptions {
                    bind_physical: false,
                    multi_thread: false,
                    userspace_stack: true,
                    accept_kcp: false,
                    accept_quic: false,
                    tcp_hole_punching: false,
                    udp_hole_punching: false,
                    symmetric_nat_punching: false,
                    port_mapping: true,
                    relay_peer_rpc: true,
                    private_mode: true,
                    system_forwarding: true,
                    udp_broadcast_relay: cfg!(windows),
                },
            },
            ..Default::default()
        };
        let config = configuration(&p).unwrap();
        let flags = config.get_flags();
        assert!(flags.lazy_p2p && !flags.disable_p2p && flags.p2p_only && flags.need_p2p);
        assert!(flags.latency_first && flags.enable_ipv6 && !flags.enable_encryption);
        assert_eq!(flags.mtu, 1240);
        assert!(flags.enable_kcp_proxy && !flags.enable_quic_proxy);
        assert!(flags.disable_kcp_input && flags.disable_quic_input);
        assert!(flags.use_smoltcp && !flags.bind_device && !flags.multi_thread);
        assert!(
            flags.disable_tcp_hole_punching
                && flags.disable_udp_hole_punching
                && flags.disable_sym_hole_punching
        );
        assert!(
            !flags.disable_upnp
                && flags.private_mode
                && flags.relay_all_peer_rpc
                && flags.proxy_forward_by_system
        );
        assert_eq!(flags.enable_udp_broadcast_relay, cfg!(windows));
        assert!(!flags.no_tun && !flags.accept_dns && flags.relay_network_whitelist.is_empty());
        assert!(config.get_routes().unwrap().is_empty());
        p.easytier.p2p = P2pPolicy::Disabled;
        p.easytier.tcp_acceleration = TcpAcceleration::Quic;
        let flags = configuration(&p).unwrap().get_flags();
        assert!(
            flags.disable_p2p
                && !flags.lazy_p2p
                && flags.enable_quic_proxy
                && !flags.enable_kcp_proxy
        );
        for (mtu, ok) in [(399, false), (400, true), (1380, true), (1381, false)] {
            p.easytier.mtu = Some(mtu);
            assert_eq!(configuration(&p).is_ok(), ok);
        }
    }

    #[test]
    fn exit_and_subnets_are_explicit_without_default_host_routes() {
        let mut p = MeshProfile {
            network_name: "fixture".into(),
            network_secret: super::super::MeshSecret("fake".into()),
            peers: vec!["tcp://192.0.2.1:11010".into()],
            remote_subnets: vec!["192.168.50.0/24".into()],
            advertised_subnets: vec!["192.168.60.0/24".into()],
            exit_node: "10.126.126.1".into(),
            port_forwards: vec![
                super::super::PortForward::parse("tcp://127.0.0.1:8080/10.126.126.2:80").unwrap(),
            ],
            ..Default::default()
        };
        let c = configuration(&p).unwrap();
        assert_eq!(
            c.get_exit_nodes(),
            vec!["10.126.126.1".parse::<std::net::IpAddr>().unwrap()]
        );
        assert_eq!(
            c.get_routes()
                .unwrap()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            p.remote_subnets
        );
        assert_eq!(c.get_proxy_cidrs()[0].cidr.to_string(), "192.168.60.0/24");
        assert!(c.get_port_forwards().is_empty());
        p.exit_node.clear();
        p.advertise_exit_node = true;
        let c = configuration(&p).unwrap();
        assert!(c.get_flags().enable_exit_node);
        assert!(!c.get_flags().proxy_forward_by_system);
        assert_eq!(
            c.get_acl().unwrap().acl_v1.unwrap().group.unwrap().members,
            [EXIT_GROUP]
        );
    }
}
