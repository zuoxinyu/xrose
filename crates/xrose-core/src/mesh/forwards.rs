//! Scoped listeners dial either the authenticated mesh SOCKS inbound or a host service.
use super::{ForwardProtocol, MeshSecret, PortForward};
use crate::{Error, Result};
use async_socks5::{AddrKind, Auth, SocksDatagram};
use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{mpsc, watch},
    task::JoinSet,
};
use tokio_util::task::AbortOnDropHandle;

const MAX_CLIENTS: usize = 128;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const UDP_IDLE: Duration = Duration::from_secs(60);

pub(super) struct Forwards {
    proxy: Arc<Proxy>,
    listeners: Vec<Listener>,
}
struct Proxy {
    address: Option<SocketAddr>,
    password: MeshSecret,
}
struct Listener {
    protocol: ForwardProtocol,
    listen: SocketAddr,
    target: watch::Sender<SocketAddr>,
    task: AbortOnDropHandle<()>,
}
impl Forwards {
    pub fn new(address: SocketAddr, password: MeshSecret) -> Self {
        Self {
            proxy: Arc::new(Proxy {
                address: Some(address),
                password,
            }),
            listeners: Vec::new(),
        }
    }
    pub fn direct() -> Self {
        Self {
            proxy: Arc::new(Proxy {
                address: None,
                password: MeshSecret::default(),
            }),
            listeners: Vec::new(),
        }
    }
    pub fn check_address(&self, address: std::net::IpAddr) -> Result<()> {
        if self.listeners.iter().any(|l| l.listen.ip() != address) {
            return Err(Error::other("服务转发的虚拟地址已变化，请重新连接组网"));
        }
        Ok(())
    }
    /// Release only ports being moved to the other direction, before its bind.
    pub async fn release_ports(&mut self, ports: &[(ForwardProtocol, u16)]) {
        for i in (0..self.listeners.len()).rev() {
            if ports.contains(&(self.listeners[i].protocol, self.listeners[i].listen.port())) {
                self.stop(i).await;
            }
        }
    }
    pub async fn apply(&mut self, rules: &[PortForward]) -> Result<()> {
        // Bind additions before changing live listeners. Only an overlapping old
        // wildcard/specific bind must be released first; the worker rolls it back on error.
        let mut additions = Vec::new();
        for rule in rules {
            if self
                .listeners
                .iter()
                .any(|l| l.protocol == rule.protocol && l.listen == rule.listen)
            {
                continue;
            }
            for i in (0..self.listeners.len()).rev() {
                let old = &self.listeners[i];
                if old.protocol == rule.protocol
                    && old.listen.port() == rule.listen.port()
                    && (old.listen.ip().is_unspecified() || rule.listen.ip().is_unspecified())
                {
                    self.stop(i).await;
                }
            }
            let bind_error = |_| Error::other("无法监听转发端口，请检查监听地址、权限及端口占用");
            let (target, rx) = watch::channel(rule.target);
            let proxy = self.proxy.clone();
            let task = match rule.protocol {
                ForwardProtocol::Tcp => {
                    let socket = TcpListener::bind(rule.listen).await.map_err(bind_error)?;
                    tokio::spawn(tcp(socket, proxy, rx))
                }
                ForwardProtocol::Udp => {
                    let socket = UdpSocket::bind(rule.listen).await.map_err(bind_error)?;
                    tokio::spawn(udp(socket, proxy, rx))
                }
            };
            additions.push(Listener {
                protocol: rule.protocol,
                listen: rule.listen,
                target,
                task: AbortOnDropHandle::new(task),
            });
        }
        for i in (0..self.listeners.len()).rev() {
            let old = &self.listeners[i];
            if let Some(rule) = rules
                .iter()
                .find(|r| r.protocol == old.protocol && r.listen == old.listen)
            {
                old.target.send_if_modified(|t| {
                    if *t == rule.target {
                        return false;
                    }
                    *t = rule.target;
                    true
                });
            } else {
                self.stop(i).await;
            }
        }
        self.listeners.extend(additions);
        Ok(())
    }
    async fn stop(&mut self, index: usize) {
        let old = self.listeners.swap_remove(index);
        old.task.abort();
        let _ = old.task.await;
    }
}
impl Proxy {
    fn auth(&self) -> Option<Auth> {
        Some(Auth::new("xrose", self.password.0.clone()))
    }
    async fn connect(&self, target: SocketAddr) -> Option<TcpStream> {
        let Some(address) = self.address else {
            return TcpStream::connect(target).await.ok();
        };
        let mut stream = TcpStream::connect(address).await.ok()?;
        async_socks5::connect(&mut stream, target, self.auth())
            .await
            .ok()?;
        Some(stream)
    }
    async fn associate(&self) -> Option<SocksDatagram<TcpStream>> {
        let stream = TcpStream::connect(self.address?).await.ok()?;
        let socket = UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .ok()?;
        let local = socket.local_addr().ok()?;
        SocksDatagram::associate(stream, socket, self.auth(), Some(local))
            .await
            .ok()
    }
}
async fn tcp(socket: TcpListener, proxy: Arc<Proxy>, target: watch::Receiver<SocketAddr>) {
    let mut clients = JoinSet::new();
    loop {
        tokio::select! {
            Some(_) = clients.join_next(), if !clients.is_empty() => {}
            result = socket.accept(), if clients.len() < MAX_CLIENTS => {
                let Ok((mut client, _)) = result else { break; };
                let proxy = proxy.clone();
                let target = *target.borrow();
                clients.spawn(async move {
                    if let Ok(Some(mut remote)) = tokio::time::timeout(CONNECT_TIMEOUT, proxy.connect(target)).await {
                        let _ = tokio::io::copy_bidirectional(&mut client, &mut remote).await;
                    }
                });
            }
        }
    }
}
type Packet = (SocketAddr, Vec<u8>);
async fn udp(socket: UdpSocket, proxy: Arc<Proxy>, mut target: watch::Receiver<SocketAddr>) {
    let mut clients = HashMap::<SocketAddr, mpsc::Sender<Vec<u8>>>::new();
    let mut sessions = JoinSet::new();
    let (replies, mut received) = mpsc::channel::<Packet>(64);
    let mut buffer = vec![0; 65535];
    loop {
        tokio::select! {
            changed = target.changed() => {
                if changed.is_err() { break; }
                clients.clear();
                sessions.shutdown().await;
                while received.try_recv().is_ok() {}
            }
            Some(Ok(client)) = sessions.join_next(), if !sessions.is_empty() => { clients.remove(&client); }
            Some((client, packet)) = received.recv() => { let _ = socket.send_to(&packet, client).await; }
            result = socket.recv_from(&mut buffer) => {
                let Ok((size, client)) = result else { break; };
                if !clients.contains_key(&client) && clients.len() < MAX_CLIENTS {
                    let (tx, rx) = mpsc::channel(8);
                    clients.insert(client, tx);
                    let proxy = proxy.clone();
                    let replies = replies.clone();
                    let target = *target.borrow();
                    sessions.spawn(async move {
                        udp_session(proxy, target, client, rx, replies).await;
                        client
                    });
                }
                if let Some(tx) = clients.get(&client) { let _ = tx.try_send(buffer[..size].to_vec()); }
            }
        }
    }
}
async fn udp_session(
    proxy: Arc<Proxy>,
    target: SocketAddr,
    client: SocketAddr,
    mut input: mpsc::Receiver<Vec<u8>>,
    replies: mpsc::Sender<Packet>,
) {
    if proxy.address.is_none() {
        let bind: SocketAddr = if target.is_ipv4() {
            ([0, 0, 0, 0], 0).into()
        } else {
            (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
        };
        let Ok(socket) = UdpSocket::bind(bind).await else {
            return;
        };
        if socket.connect(target).await.is_err() {
            return;
        }
        let mut buf = vec![0; 65535];
        loop {
            tokio::select! {
                packet = input.recv() => {
                    let Some(packet) = packet else { break; };
                    if socket.send(&packet).await.is_err() { break; }
                }
                result = socket.recv(&mut buf) => {
                    let Ok(n) = result else { break; };
                    if replies.send((client, buf[..n].to_vec())).await.is_err() { break; }
                }
                _ = tokio::time::sleep(UDP_IDLE) => break,
            }
        }
        return;
    }
    let Ok(Some(relay)) = tokio::time::timeout(CONNECT_TIMEOUT, proxy.associate()).await else {
        return;
    };
    let mut buf = vec![0; 65535];
    loop {
        tokio::select! {
            packet = input.recv() => {
                let Some(packet) = packet else { break; };
                if relay.send_to(&packet, target).await.is_err() { break; }
            }
            result = relay.recv_from(&mut buf) => {
                let Ok((n, source)) = result else { break; };
                if source == AddrKind::Ip(target) && replies.send((client, buf[..n].to_vec())).await.is_err() { break; }
            }
            _ = tokio::time::sleep(UDP_IDLE) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn live_listener_edits_and_failed_bind_keep_unrelated_tcp_stream() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let target = server.local_addr().unwrap();
            let _echo = AbortOnDropHandle::new(tokio::spawn(async move {
                let (mut socket, _) = server.accept().await.unwrap();
                let (mut input, mut output) = socket.split();
                let _ = tokio::io::copy(&mut input, &mut output).await;
            }));
            let reserve = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let listen = reserve.local_addr().unwrap();
            drop(reserve);
            let rule = PortForward {
                protocol: ForwardProtocol::Tcp,
                listen,
                target,
            };
            let mut forwards = Forwards::direct();
            forwards.apply(&[rule.clone()]).await.unwrap();
            let mut client = TcpStream::connect(listen).await.unwrap();
            let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut second = rule.clone();
            second.listen = occupied.local_addr().unwrap();
            assert!(
                forwards
                    .apply(&[rule.clone(), second.clone()])
                    .await
                    .is_err()
            );
            drop(occupied);
            for added in [true, false] {
                // The already connected stream must survive adding/removing another listener.
                let rules = if added {
                    vec![rule.clone(), second.clone()]
                } else {
                    vec![rule.clone()]
                };
                forwards.apply(&rules).await.unwrap();
                client.write_all(b"ping").await.unwrap();
                let mut reply = [0; 4];
                client.read_exact(&mut reply).await.unwrap();
                assert_eq!(&reply, b"ping");
            }
        })
        .await
        .expect("listener update fixture timed out");
    }

    #[tokio::test]
    async fn service_forward_tcp_udp_round_trip_and_cleanup() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let tcp_target = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let udp_target = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let tcp_address = tcp_target.local_addr().unwrap();
            let udp_address = udp_target.local_addr().unwrap();
            let echo_tcp = AbortOnDropHandle::new(tokio::spawn(async move {
                let (mut socket, _) = tcp_target.accept().await.unwrap();
                let mut bytes = [0; 4];
                socket.read_exact(&mut bytes).await.unwrap();
                socket.write_all(&bytes).await.unwrap();
            }));
            let echo_udp = AbortOnDropHandle::new(tokio::spawn(async move {
                let mut bytes = [0; 8];
                for _ in 0..2 {
                    let (n, peer) = udp_target.recv_from(&mut bytes).await.unwrap();
                    udp_target.send_to(&bytes[..n], peer).await.unwrap();
                }
            }));
            let reserve = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let listen = reserve.local_addr().unwrap();
            drop(reserve);
            let mut forwards = Forwards::direct();
            let rules = vec![
                PortForward {
                    protocol: ForwardProtocol::Tcp,
                    listen,
                    target: tcp_address,
                },
                PortForward {
                    protocol: ForwardProtocol::Udp,
                    listen,
                    target: udp_address,
                },
            ];
            forwards.apply(&rules).await.unwrap();
            let mut client = TcpStream::connect(listen).await.unwrap();
            client.write_all(b"ping").await.unwrap();
            let mut reply = [0; 4];
            client.read_exact(&mut reply).await.unwrap();
            assert_eq!(&reply, b"ping");
            for message in [b"one", b"two"] {
                let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                client.send_to(message, listen).await.unwrap();
                let (n, source) = client.recv_from(&mut reply).await.unwrap();
                assert_eq!(&reply[..n], message);
                assert_eq!(source, listen);
            }
            echo_tcp.await.unwrap();
            echo_udp.await.unwrap();
            forwards.apply(&[]).await.unwrap();
            assert!(TcpStream::connect(listen).await.is_err());
            let _tcp = TcpListener::bind(listen).await.unwrap();
            let _udp = UdpSocket::bind(listen).await.unwrap();
        })
        .await
        .expect("local service fixture timed out");
    }
}
