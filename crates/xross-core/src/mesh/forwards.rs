//! Independently managed listeners dial through the authenticated, mesh-only SOCKS inbound.
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
    address: SocketAddr,
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
            proxy: Arc::new(Proxy { address, password }),
            listeners: Vec::new(),
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
        Some(Auth::new("xross", self.password.0.clone()))
    }
    async fn connect(&self, target: SocketAddr) -> Option<TcpStream> {
        let mut stream = TcpStream::connect(self.address).await.ok()?;
        async_socks5::connect(&mut stream, target, self.auth())
            .await
            .ok()?;
        Some(stream)
    }
    async fn associate(&self) -> Option<SocksDatagram<TcpStream>> {
        let stream = TcpStream::connect(self.address).await.ok()?;
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
