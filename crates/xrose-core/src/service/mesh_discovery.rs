//! Keep discovery sockets scoped to currently connected overlays and bound their concurrency.
use super::{AppService, Inner, ToastLevel};
use crate::{
    Error, Result,
    mesh::{
        MeshPhase, MeshProvider, MeshRuntimeView, MeshSnapshot,
        sync::{self, Hello},
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpSocket, TcpStream},
    task::{JoinHandle, JoinSet},
    time::{Instant, timeout},
};

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Target {
    pub profile: String,
    pub peer: String,
    ip: IpAddr,
    source: IpAddr,
}
impl Target {
    pub async fn connect(&self) -> Result<TcpStream> {
        let socket = if self.ip.is_ipv4() {
            TcpSocket::new_v4()?
        } else {
            TcpSocket::new_v6()?
        };
        socket.bind(SocketAddr::new(self.source, 0))?;
        socket
            .connect(SocketAddr::new(self.ip, sync::PORT))
            .await
            .map_err(|_| Error::other("无法连接对端 Xrose，请检查组网连接及 TCP 46973 访问权限"))
    }
}
fn usable(ip: IpAddr) -> bool {
    !ip.is_unspecified() && !ip.is_loopback() && !ip.is_multicast() && ip != Ipv4Addr::BROADCAST
}
fn targets(mesh: &MeshSnapshot) -> BTreeSet<Target> {
    let mut result = BTreeSet::new();
    for n in mesh
        .networks
        .iter()
        .filter(|n| n.phase == MeshPhase::Connected)
    {
        for p in n.peers.iter().filter(|p| p.online) {
            let ips = p
                .addresses
                .iter()
                .filter_map(|s| s.parse::<IpAddr>().ok())
                .filter(|ip| usable(*ip));
            for ip in ips {
                let local = match n.provider {
                    MeshProvider::EasyTier => n
                        .addresses
                        .iter()
                        .filter_map(|s| s.parse::<IpAddr>().ok())
                        .find(|a| a.is_ipv4() && ip.is_ipv4() && usable(*a)),
                    MeshProvider::Tailscale => {
                        let (net, tun) = if ip.is_ipv4() {
                            ("100.64.0.0/10", crate::mesh::TAILSCALE_TUN_ADDRESS)
                        } else {
                            (
                                "fd7a:115c:a1e0::/48",
                                crate::mesh::TAILSCALE_TUN_IPV6_ADDRESS,
                            )
                        };
                        net.parse::<ipnet::IpNet>()
                            .unwrap()
                            .contains(&ip)
                            .then(|| tun.parse::<ipnet::IpNet>().unwrap().addr())
                    }
                };
                if let Some(source) = local {
                    result.insert(Target {
                        profile: n.profile_id.clone(),
                        peer: p.id.clone(),
                        ip,
                        source,
                    });
                    break;
                }
            }
        }
    }
    result
}
fn network_for(mesh: &MeshSnapshot, listener: IpAddr, remote: IpAddr) -> Option<&MeshRuntimeView> {
    mesh.networks.iter().find(|n| {
        n.phase == MeshPhase::Connected
            && match n.provider {
                MeshProvider::Tailscale => listener.is_loopback() && remote.is_loopback(),
                MeshProvider::EasyTier => {
                    !listener.is_loopback()
                        && n.addresses.iter().any(|s| s.parse() == Ok(listener))
                        && n.peers.iter().any(|p| {
                            p.online && p.addresses.iter().any(|s| s.parse() == Ok(remote))
                        })
                }
            }
    })
}

fn listen(listener: TcpListener, ip: IpAddr, weak: Weak<Inner>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let permits = Arc::new(tokio::sync::Semaphore::new(12));
        loop {
            let Ok((mut s, remote)) = listener.accept().await else {
                break;
            };
            let Some(inner) = weak.upgrade() else { break };
            let Ok(permit) = permits.clone().try_acquire_owned() else {
                continue;
            };
            let mesh = inner.mesh.snapshot();
            let Some(n) = network_for(&mesh, ip, remote.ip()) else {
                continue;
            };
            let profile = n.profile_id.clone();
            let Some(p) = mesh.profiles.iter().find(|p| p.id == profile) else {
                continue;
            };
            let hello = Hello::local(p.advertised_subnets.clone());
            let relay_password = ip.is_loopback().then(|| inner.mesh.sync_password.clone());
            let weak = weak.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let _ = timeout(Duration::from_secs(15), async move {
                    if let Some(password) = relay_password {
                        sync::accept_relay(&mut s, &password.0).await?;
                    }
                    sync::serve(s, hello, move || {
                        let inner = weak.upgrade()?;
                        let mesh = inner.mesh.snapshot();
                        if network_for(&mesh, ip, remote.ip())?.profile_id != profile {
                            return None;
                        }
                        inner
                            .mesh_sync
                            .offer
                            .lock()
                            .as_ref()
                            .filter(|o| o.profile == profile)
                            .cloned()
                    })
                    .await
                })
                .await;
            });
        }
    })
}
impl AppService {
    pub(super) fn sync_target(&self, profile: &str, peer: &str) -> Result<Target> {
        targets(&self.inner.mesh.snapshot())
            .into_iter()
            .find(|t| {
                t.profile == profile
                    && t.peer == peer
                    && self.inner.mesh_sync.discovered.read().contains_key(t)
            })
            .ok_or_else(|| Error::other("设备已离线或尚未发现 Xrose，请重新选择"))
    }
    pub(super) fn annotate_peers(&self, mesh: &mut MeshSnapshot) {
        let known = self.inner.mesh_sync.discovered.read();
        for n in &mut mesh.networks {
            for p in &mut n.peers {
                if p.online && n.phase == MeshPhase::Connected {
                    p.xrose = p
                        .addresses
                        .iter()
                        .filter_map(|a| a.parse::<IpAddr>().ok())
                        .find_map(|ip| {
                            // Build the same key for either provider without scanning the discovery cache.
                            let source = if n.provider == MeshProvider::EasyTier {
                                n.addresses.iter().find_map(|v| v.parse().ok())?
                            } else {
                                if ip.is_ipv4() {
                                    crate::mesh::TAILSCALE_TUN_ADDRESS
                                } else {
                                    crate::mesh::TAILSCALE_TUN_IPV6_ADDRESS
                                }
                                .parse::<ipnet::IpNet>()
                                .ok()?
                                .addr()
                            };
                            known
                                .get(&Target {
                                    profile: n.profile_id.clone(),
                                    peer: p.id.clone(),
                                    ip,
                                    source,
                                })
                                .cloned()
                        });
                }
            }
        }
    }
    pub(super) fn start_mesh_discovery(&self) {
        let weak = Arc::downgrade(&self.inner);
        // A startup connection can request a worker as soon as AppService is returned.
        let relay = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .ok()
            .and_then(|listener| {
                listener.set_nonblocking(true).ok()?;
                self.inner.mesh.sync_port.store(
                    listener.local_addr().ok()?.port(),
                    std::sync::atomic::Ordering::Release,
                );
                Some(listener)
            });
        self.spawn(async move {
            let mut listeners = BTreeMap::<IpAddr, JoinHandle<()>>::new();
            if let Some(relay) = relay.and_then(|s| TcpListener::from_std(s).ok()) {
                let local = IpAddr::V4(Ipv4Addr::LOCALHOST);
                listeners.insert(local, listen(relay, local, weak.clone()));
            }
            let mut retry = BTreeMap::<Target, Instant>::new();
            let mut probes = JoinSet::new();
            let mut failed = BTreeSet::new();
            loop {
                let Some(inner) = weak.upgrade() else { break };
                let mesh = inner.mesh.snapshot();
                let wanted = targets(&mesh);
                let mut binds = BTreeSet::new();
                // The Tailscale endpoint is userspace. Reserve its loopback relay before a worker starts.
                binds.insert(IpAddr::V4(Ipv4Addr::LOCALHOST));
                for n in mesh.networks.iter().filter(|n| {
                    n.phase == MeshPhase::Connected && n.provider == MeshProvider::EasyTier
                }) {
                    binds.extend(
                        n.addresses
                            .iter()
                            .filter_map(|s| s.parse::<IpAddr>().ok())
                            .filter(|a| a.is_ipv4() && usable(*a)),
                    );
                }
                listeners.retain(|ip, task| {
                    if !binds.contains(ip) {
                        task.abort();
                        false
                    } else {
                        !task.is_finished()
                    }
                });
                failed.retain(|ip| binds.contains(ip));
                for ip in binds {
                    if listeners.contains_key(&ip) {
                        continue;
                    }
                    let port = if ip.is_loopback() { 0 } else { sync::PORT };
                    match TcpListener::bind(SocketAddr::new(ip, port)).await {
                        Ok(listener) => {
                            if ip.is_loopback()
                                && let Ok(addr) = listener.local_addr()
                            {
                                inner
                                    .mesh
                                    .sync_port
                                    .store(addr.port(), std::sync::atomic::Ordering::Release);
                            }
                            failed.remove(&ip);
                            listeners.insert(ip, listen(listener, ip, weak.clone()));
                        }
                        Err(_) if failed.insert(ip) => AppService {
                            inner: inner.clone(),
                        }
                        .notify(ToastLevel::Warning, "无法监听组网发现端口，请检查端口占用"),
                        Err(_) => {}
                    }
                }
                while let Some(Ok((key, answer))) = probes.try_join_next() {
                    if wanted.contains(&key) {
                        let mut cache = inner.mesh_sync.discovered.write();
                        if let Some(hello) = answer {
                            cache.insert(key, hello);
                        } else {
                            cache.remove(&key);
                        }
                    }
                }
                inner
                    .mesh_sync
                    .discovered
                    .write()
                    .retain(|k, _| wanted.contains(k));
                retry.retain(|k, _| wanted.contains(k));
                inner.mesh_sync.expire_shares(&mesh);
                let now = Instant::now();
                let mut due: Vec<_> = wanted
                    .into_iter()
                    .filter(|k| retry.get(k).is_none_or(|t| *t <= now))
                    .collect();
                due.sort_by_key(|k| retry.get(k).copied());
                for key in due.into_iter().take(8usize.saturating_sub(probes.len())) {
                    retry.insert(key.clone(), now + Duration::from_secs(30));
                    probes.spawn(async move {
                        let result = timeout(Duration::from_secs(3), async {
                            sync::discover(key.connect().await?).await
                        })
                        .await
                        .ok()
                        .and_then(Result::ok);
                        (key, result)
                    });
                }
                drop(inner);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            for task in listeners.into_values() {
                task.abort();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{network_for, targets};
    use crate::mesh::{MeshPeer, MeshPhase, MeshProvider, MeshRuntimeView, MeshSnapshot};
    #[test]
    fn peer_discovery_requires_a_live_overlay_and_uses_its_local_source() {
        let easy = MeshRuntimeView {
            provider: MeshProvider::EasyTier,
            profile_id: "east".into(),
            phase: MeshPhase::Connected,
            addresses: vec!["192.0.2.1".into()],
            peers: vec![MeshPeer {
                id: "one".into(),
                online: true,
                addresses: vec!["192.0.2.2".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let ts = MeshRuntimeView {
            provider: MeshProvider::Tailscale,
            profile_id: "west".into(),
            phase: MeshPhase::Connected,
            peers: vec![MeshPeer {
                id: "two".into(),
                online: true,
                addresses: vec!["203.0.113.1".into(), "100.64.0.2".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut mesh = MeshSnapshot {
            networks: vec![easy, ts],
            ..Default::default()
        };
        let found = targets(&mesh);
        assert_eq!(found.len(), 2);
        assert!(
            found
                .iter()
                .any(|t| t.ip.to_string() == "192.0.2.2" && t.source.to_string() == "192.0.2.1")
        );
        assert!(found.iter().any(|t| t.ip.to_string()=="100.64.0.2" && t.source.to_string()=="10.255.253.1"));
        let address = |s: &str| s.parse().unwrap();
        assert!(network_for(&mesh, address("192.0.2.1"), address("192.0.2.3")).is_none());
        assert!(network_for(&mesh, address("192.0.2.1"), address("192.0.2.2")).is_some());
        assert!(network_for(&mesh, address("127.0.0.1"), address("203.0.113.1")).is_none());
        mesh.networks[0].peers[0].online = false;
        mesh.networks[1].phase = MeshPhase::Disconnected;
        assert!(targets(&mesh).is_empty());
        assert!(network_for(&mesh, address("127.0.0.1"), address("127.0.0.1")).is_none());
    }
}
