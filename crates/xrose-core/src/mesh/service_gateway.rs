//! Fixed authenticated SOCKS ingress for Tailscale's own endpoint traffic.
//! Destinations select an explicit service rule, never an arbitrary host port.
use super::{ForwardProtocol, MeshSecret, ServiceForward};
use crate::{Error, Result};
use std::{
    collections::HashMap,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{mpsc, watch},
    task::JoinSet,
};
use tokio_util::task::AbortOnDropHandle;

type Rules = watch::Receiver<Vec<ServiceForward>>;
const LIMIT: usize = 128;
const TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct Gateway {
    pub address: SocketAddr,
    pub password: MeshSecret,
    rules: watch::Sender<Vec<ServiceForward>>,
    _task: AbortOnDropHandle<()>,
}
impl Gateway {
    pub async fn start(rules: &[ServiceForward]) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| Error::other("无法分配服务转发端口，请重试"))?;
        let address = listener.local_addr()?;
        let password = MeshSecret(uuid::Uuid::new_v4().simple().to_string());
        let secret = password.clone();
        let (rules, receiver) = watch::channel(rules.to_vec());
        let task = tokio::spawn(async move {
            let mut clients = JoinSet::new();
            loop {
                tokio::select! {
                    Some(_) = clients.join_next(), if !clients.is_empty() => {}
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break; };
                        if clients.len() >= LIMIT { continue; }
                        let rules = receiver.clone();
                        let secret = secret.clone();
                        clients.spawn(async move { let _ = serve(socket, &secret.0, rules).await; });
                    }
                }
            }
        });
        Ok(Self {
            address,
            password,
            rules,
            _task: AbortOnDropHandle::new(task),
        })
    }
    pub fn apply(&self, rules: &[ServiceForward]) {
        self.rules.send_if_modified(|old| {
            if old == rules {
                return false;
            }
            *old = rules.to_vec();
            true
        });
    }
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "Service request rejected")
}

fn target(rules: &Rules, protocol: ForwardProtocol, destination: SocketAddr) -> Option<SocketAddr> {
    // sing-box maps only the endpoint's own addresses to these exact loopback IPs.
    if destination.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST)
        && destination.ip() != IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
    {
        return None;
    }
    rules
        .borrow()
        .iter()
        .find(|r| r.protocol == protocol && r.port == destination.port())
        .map(|r| r.target)
}

async fn read_address(socket: &mut TcpStream) -> io::Result<SocketAddr> {
    let ip = match socket.read_u8().await? {
        1 => {
            let mut bytes = [0; 4];
            socket.read_exact(&mut bytes).await?;
            IpAddr::from(bytes)
        }
        4 => {
            let mut bytes = [0; 16];
            socket.read_exact(&mut bytes).await?;
            IpAddr::from(bytes)
        }
        _ => return Err(invalid()),
    };
    Ok((ip, socket.read_u16().await?).into())
}

pub(super) async fn read_request(
    socket: &mut TcpStream,
    password: &str,
) -> io::Result<(u8, SocketAddr)> {
    super::easytier_gateway::authenticate(socket, password).await?;
    if socket.read_u8().await? != 5 {
        return Err(invalid());
    }
    let command = socket.read_u8().await?;
    if socket.read_u8().await? != 0 {
        return Err(invalid());
    }
    Ok((command, read_address(socket).await?))
}

async fn serve(mut socket: TcpStream, password: &str, mut rules: Rules) -> io::Result<()> {
    let (command, destination) = tokio::time::timeout(TIMEOUT, read_request(&mut socket, password))
        .await
        .map_err(|_| invalid())??;
    if command == 1 {
        let Some(address) = target(&rules, ForwardProtocol::Tcp, destination) else {
            socket.write_all(&[5, 2, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            return Err(invalid());
        };
        let mut upstream = tokio::time::timeout(TIMEOUT, TcpStream::connect(address))
            .await
            .map_err(|_| invalid())??;
        if target(&rules, ForwardProtocol::Tcp, destination) != Some(address) {
            return Err(invalid());
        }
        socket.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).await?;
        let copy = tokio::io::copy_bidirectional(&mut socket, &mut upstream);
        tokio::pin!(copy);
        loop {
            tokio::select! {
                result = &mut copy => { result?; break; }
                changed = rules.changed() => {
                    if changed.is_err() || target(&rules, ForwardProtocol::Tcp, destination).is_none() { break; }
                }
            }
        }
    } else if command == 3 {
        udp(socket, rules).await?;
    } else {
        socket.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    }
    Ok(())
}

fn decode(packet: &[u8]) -> Option<(SocketAddr, &[u8])> {
    if packet.get(..3)? != [0, 0, 0] {
        return None;
    }
    let (ip, end) = match *packet.get(3)? {
        1 => (
            IpAddr::from(<[u8; 4]>::try_from(packet.get(4..8)?).ok()?),
            8,
        ),
        4 => (
            IpAddr::from(<[u8; 16]>::try_from(packet.get(4..20)?).ok()?),
            20,
        ),
        _ => return None,
    };
    let port = u16::from_be_bytes(packet.get(end..end + 2)?.try_into().ok()?);
    Some(((ip, port).into(), &packet[end + 2..]))
}
fn encode(address: SocketAddr, body: &[u8]) -> Vec<u8> {
    let mut result = vec![0, 0, 0];
    match address.ip() {
        IpAddr::V4(ip) => {
            result.push(1);
            result.extend(ip.octets());
        }
        IpAddr::V6(ip) => {
            result.push(4);
            result.extend(ip.octets());
        }
    }
    result.extend(address.port().to_be_bytes());
    result.extend(body);
    result
}

async fn udp(mut control: TcpStream, mut rules: Rules) -> io::Result<()> {
    let relay = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let port = relay.local_addr()?.port().to_be_bytes();
    control
        .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, port[0], port[1]])
        .await?;
    let mut peer = None;
    let mut sessions = HashMap::<SocketAddr, (SocketAddr, u64, mpsc::Sender<Vec<u8>>)>::new();
    let mut tasks = JoinSet::new();
    let mut generation = 0u64;
    let (replies, mut received) = mpsc::channel::<(SocketAddr, SocketAddr, Vec<u8>)>(64);
    let mut buffer = vec![0; 65535];
    let mut byte = [0];
    loop {
        tokio::select! {
            _ = control.read(&mut byte) => break,
            changed = rules.changed() => {
                if changed.is_err() { break; }
                sessions.retain(|destination, (address, _, _)| target(&rules, ForwardProtocol::Udp, *destination) == Some(*address));
            }
            Some(Ok((destination, id))) = tasks.join_next(), if !tasks.is_empty() => {
                if sessions.get(&destination).is_some_and(|(_, generation, _)| *generation == id) { sessions.remove(&destination); }
            }
            Some((destination, address, data)) = received.recv() => {
                if target(&rules, ForwardProtocol::Udp, destination) == Some(address) {
                    if let Some(peer) = peer { relay.send_to(&encode(destination, &data), peer).await?; }
                }
            }
            packet = relay.recv_from(&mut buffer) => {
                let (n, source) = packet?;
                if !source.ip().is_loopback() || peer.is_some_and(|p| p != source) { continue; }
                let Some((destination, body)) = decode(&buffer[..n]) else { continue; };
                let Some(address) = target(&rules, ForwardProtocol::Udp, destination) else { continue; };
                peer = Some(source);
                if sessions.get(&destination).is_some_and(|(a, _, _)| *a != address) { sessions.remove(&destination); }
                if !sessions.contains_key(&destination) && tasks.len() < LIMIT {
                    let (sender, receiver) = mpsc::channel(8);
                    generation += 1;
                    let id = generation;
                    sessions.insert(destination, (address, id, sender));
                    let replies = replies.clone();
                    let rules = rules.clone();
                    tasks.spawn(async move {
                        let _ = udp_session(destination, address, receiver, replies, rules).await;
                        (destination, id)
                    });
                }
                if let Some((_, _, sender)) = sessions.get(&destination) { let _ = sender.try_send(body.to_vec()); }
            }
            _ = tokio::time::sleep(Duration::from_secs(60)) => break,
        }
    }
    Ok(())
}

async fn udp_session(
    destination: SocketAddr,
    address: SocketAddr,
    mut input: mpsc::Receiver<Vec<u8>>,
    output: mpsc::Sender<(SocketAddr, SocketAddr, Vec<u8>)>,
    mut rules: Rules,
) -> io::Result<()> {
    let bind: SocketAddr = if address.is_ipv4() {
        ([0, 0, 0, 0], 0).into()
    } else {
        (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let socket = UdpSocket::bind(bind).await?;
    socket.connect(address).await?;
    let mut buffer = vec![0; 65535];
    loop {
        tokio::select! {
            changed = rules.changed() => {
                if changed.is_err() || target(&rules, ForwardProtocol::Udp, destination) != Some(address) { break; }
            }
            packet = input.recv() => {
                let Some(data) = packet else { break; };
                if target(&rules, ForwardProtocol::Udp, destination) != Some(address) { break; }
                socket.send(&data).await?;
            }
            received = socket.recv(&mut buffer) => {
                let n = received?;
                if output.send((destination, address, buffer[..n].to_vec())).await.is_err() { break; }
            }
            _ = tokio::time::sleep(Duration::from_secs(60)) => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_socks5::{Auth, SocksDatagram};

    async fn connect(gateway: &Gateway, destination: SocketAddr) -> io::Result<TcpStream> {
        let mut stream = TcpStream::connect(gateway.address).await?;
        async_socks5::connect(
            &mut stream,
            destination,
            Some(Auth::new("xrose", gateway.password.0.clone())),
        )
        .await
        .map_err(|_| invalid())?;
        Ok(stream)
    }
    async fn echo(stream: &mut TcpStream) {
        stream.write_all(b"ping").await.unwrap();
        let mut reply = [0; 4];
        stream.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply, b"ping");
    }

    #[tokio::test]
    async fn live_gateway_updates_preserve_other_connections_and_reject_unpublished_ports() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let target = listener.local_addr().unwrap();
            let _server = AbortOnDropHandle::new(tokio::spawn(async move {
                let mut clients = JoinSet::new();
                loop {
                    tokio::select! {
                        accepted = listener.accept() => {
                            let (mut socket, _) = accepted.unwrap();
                            clients.spawn(async move {
                                let (mut input, mut output) = socket.split();
                                let _ = tokio::io::copy(&mut input, &mut output).await;
                            });
                        }
                        _ = clients.join_next(), if !clients.is_empty() => {}
                    }
                }
            }));
            let gateway = Gateway::start(&[]).await.unwrap();
            let first: SocketAddr = "127.0.0.1:18080".parse().unwrap();
            let second: SocketAddr = "[::1]:18081".parse().unwrap();
            assert!(connect(&gateway, first).await.is_err());
            let one = ServiceForward {
                protocol: ForwardProtocol::Tcp,
                port: first.port(),
                target,
            };
            let two = ServiceForward {
                port: second.port(),
                ..one.clone()
            };
            gateway.apply(&[one.clone()]);
            let mut established = connect(&gateway, first).await.unwrap();
            echo(&mut established).await;
            gateway.apply(&[one.clone(), two.clone()]);
            let mut other = connect(&gateway, second).await.unwrap();
            echo(&mut other).await;
            echo(&mut established).await;
            // The bridge must not expose arbitrary host destinations or accept unauthenticated clients.
            assert!(
                connect(&gateway, "192.0.2.10:18080".parse().unwrap())
                    .await
                    .is_err()
            );
            let mut wrong = TcpStream::connect(gateway.address).await.unwrap();
            assert!(
                async_socks5::connect(&mut wrong, first, Some(Auth::new("xrose", "wrong")))
                    .await
                    .is_err()
            );
            gateway.apply(&[one]);
            assert!(connect(&gateway, second).await.is_err());
            let mut byte = [0];
            assert_eq!(other.read(&mut byte).await.unwrap(), 0);
            echo(&mut established).await;
            let address = gateway.address;
            drop(gateway);
            assert_eq!(established.read(&mut byte).await.unwrap(), 0);
            let _rebound = TcpListener::bind(address).await.unwrap();
        })
        .await
        .expect("gateway fixture timed out");
    }

    #[tokio::test]
    async fn udp_live_rules_support_both_address_families_and_close_removed_sessions() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let target = server.local_addr().unwrap();
            let _server = AbortOnDropHandle::new(tokio::spawn(async move {
                let mut buf = [0; 64];
                loop {
                    let (n, peer) = server.recv_from(&mut buf).await.unwrap();
                    server.send_to(&buf[..n], peer).await.unwrap();
                }
            }));
            let gateway = Gateway::start(&[]).await.unwrap();
            let control = TcpStream::connect(gateway.address).await.unwrap();
            let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let local = socket.local_addr().unwrap();
            let relay = SocksDatagram::associate(
                control,
                socket,
                Some(Auth::new("xrose", gateway.password.0.clone())),
                Some(local),
            )
            .await
            .unwrap();
            let one = ServiceForward {
                protocol: ForwardProtocol::Udp,
                port: 18080,
                target,
            };
            let two = ServiceForward {
                port: 18081,
                ..one.clone()
            };
            let destinations: [SocketAddr; 2] = [
                "127.0.0.1:18080".parse().unwrap(),
                "[::1]:18081".parse().unwrap(),
            ];
            gateway.apply(&[one.clone(), two.clone()]);
            let mut buf = [0; 64];
            for destination in destinations {
                relay.send_to(b"udp", destination).await.unwrap();
                let (n, source) = relay.recv_from(&mut buf).await.unwrap();
                assert_eq!(&buf[..n], b"udp");
                assert_eq!(source, async_socks5::AddrKind::Ip(destination));
            }
            gateway.apply(&[two]);
            relay.send_to(b"denied", destinations[0]).await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(100), relay.recv_from(&mut buf))
                    .await
                    .is_err()
            );
            relay.send_to(b"kept", destinations[1]).await.unwrap();
            let (n, _) = relay.recv_from(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"kept");
            gateway.apply(&[one]);
            relay.send_to(b"added", destinations[0]).await.unwrap();
            let (n, _) = relay.recv_from(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"added");
        })
        .await
        .expect("UDP gateway fixture timed out");
    }
}
