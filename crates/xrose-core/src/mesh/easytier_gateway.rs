//! Authenticated loopback SOCKS gateway. Every payload uses the mesh stack;
//! missing peers never fall back to host sockets or host default routes.
use super::MeshSecret;
use crate::{Error, Result};
use easytier::{
    peers::{PeerPacketFilter, peer_manager::PeerManager},
    tunnel::packet_def::{PacketType, ZCPacket},
};
use parking_lot::Mutex;
use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    task::JoinSet,
};
use tokio_smoltcp::{self as stack, smoltcp};

use smoltcp::wire::IpProtocol;

type Ports = Arc<Mutex<HashSet<(IpProtocol, u16)>>>;

struct ChannelDevice {
    incoming: tokio::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>,
    outgoing: tokio_util::sync::PollSender<Vec<u8>>,
    caps: smoltcp::phy::DeviceCapabilities,
}
impl stack::device::AsyncDevice for ChannelDevice {
    fn capabilities(&self) -> &smoltcp::phy::DeviceCapabilities {
        &self.caps
    }
}
impl futures_util::Stream for ChannelDevice {
    type Item = std::io::Result<Vec<u8>>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.incoming.poll_recv(cx)
    }
}
impl futures_util::Sink<Vec<u8>> for ChannelDevice {
    type Error = std::io::Error;
    fn poll_ready(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.outgoing.poll_reserve(cx).map_err(|_| invalid())
    }
    fn start_send(mut self: std::pin::Pin<&mut Self>, item: Vec<u8>) -> std::io::Result<()> {
        self.outgoing.send_item(item).map_err(|_| invalid())
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_close(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.outgoing.close();
        std::task::Poll::Ready(Ok(()))
    }
}

struct PacketSink {
    ports: Ports,
    address: Ipv4Addr,
    sender: tokio::sync::mpsc::Sender<std::io::Result<Vec<u8>>>,
}
#[async_trait::async_trait]
impl PeerPacketFilter for PacketSink {
    async fn try_process_packet_from_peer(&self, packet: ZCPacket) -> Option<ZCPacket> {
        if !packet
            .peer_manager_header()
            .is_some_and(|h| h.packet_type == PacketType::Data as u8)
        {
            return Some(packet);
        }
        let data = packet.payload();
        if data.len() < 20 || data[0] >> 4 != 4 || data[16..20] != self.address.octets() {
            return Some(packet);
        }
        let offset = usize::from(data[0] & 15) * 4;
        // Fragmented UDP is reassembled by smoltcp; also retain the kernel copy
        // since later fragments do not contain a transport port.
        let protocol = IpProtocol::from(data[9]);
        if protocol == IpProtocol::Udp && (data[6] & 0x3f != 0 || data[7] != 0) {
            if self.ports.lock().iter().any(|(p, _)| *p == IpProtocol::Udp) {
                let _ = self.sender.try_send(Ok(data.to_vec()));
            }
            return Some(packet);
        }
        if matches!(protocol, IpProtocol::Tcp | IpProtocol::Udp)
            && offset >= 20
            && data.len() >= offset + 4
            && self.ports.lock().contains(&(
                protocol,
                u16::from_be_bytes([data[offset + 2], data[offset + 3]]),
            ))
        {
            let _ = self.sender.try_send(Ok(data.to_vec()));
            None
        } else {
            Some(packet)
        }
    }
}

struct PortGuard {
    _holder: socket2::Socket,
    port: u16,
    protocol: IpProtocol,
    ports: Ports,
}
impl PortGuard {
    fn new(ports: &Ports, protocol: IpProtocol) -> std::io::Result<Self> {
        // Windows can reserve different port ranges for TCP and UDP. Reserve
        // and intercept only the protocol used by this particular flow.
        let kind = match protocol {
            IpProtocol::Tcp => socket2::Type::STREAM,
            IpProtocol::Udp => socket2::Type::DGRAM,
            _ => return Err(invalid()),
        };
        let holder = socket2::Socket::new(socket2::Domain::IPV4, kind, None)?;
        holder.bind(&SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0).into())?;
        let port = holder.local_addr()?.as_socket().ok_or_else(invalid)?.port();
        ports.lock().insert((protocol, port));
        Ok(Self {
            _holder: holder,
            protocol,
            port,
            ports: ports.clone(),
        })
    }
    fn port(&self) -> u16 {
        self.port
    }
}
impl Drop for PortGuard {
    fn drop(&mut self) {
        self.ports.lock().remove(&(self.protocol, self.port));
    }
}

pub(super) enum GatewayRoute {
    SelectedExit,
    Mesh,
}
pub struct Gateway {
    pub(super) listen: SocketAddr,
    address: IpAddr,
    exit_peer: tokio::sync::watch::Sender<Option<u32>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl Gateway {
    pub async fn start(
        peer: Arc<PeerManager>,
        address: String,
        port: u16,
        password: MeshSecret,
        route: GatewayRoute,
    ) -> Result<Self> {
        let cidr = address
            .parse::<ipnet::Ipv4Net>()
            .map_err(|_| Error::config("EasyTier 虚拟地址无效"))?;
        let (exit_peer, selected) = tokio::sync::watch::channel(None);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .await
            .map_err(|_| Error::other("无法监听本地组网网关，请检查端口占用"))?;
        let listen = listener.local_addr()?;
        let ports = Ports::default();
        let mut caps = smoltcp::phy::DeviceCapabilities::default();
        caps.medium = smoltcp::phy::Medium::Ip;
        caps.max_transmission_unit = 1284;
        let (sender, incoming) = tokio::sync::mpsc::channel(1024);
        let (outgoing, mut receiver) = tokio::sync::mpsc::channel::<Vec<u8>>(1024);
        let device = ChannelDevice {
            incoming,
            outgoing: tokio_util::sync::PollSender::new(outgoing),
            caps,
        };
        peer.add_packet_process_pipeline(Box::new(PacketSink {
            ports: ports.clone(),
            address: cidr.addr(),
            sender,
        }))
        .await;
        let net = Arc::new(stack::Net::new(
            device,
            stack::NetConfig::new(
                smoltcp::iface::Config::new(smoltcp::wire::HardwareAddress::Ip),
                address
                    .parse()
                    .map_err(|_| Error::config("EasyTier 虚拟地址无效"))?,
                vec![cidr.addr().into()],
            ),
        ));
        let mut tasks = Vec::new();
        tasks.push(tokio::spawn(async move {
            while let Some(data) = receiver.recv().await {
                if matches!(route, GatewayRoute::Mesh) {
                    if let Ok(ip) = smoltcp::wire::Ipv4Packet::new_checked(data.as_slice()) {
                        let dst = IpAddr::V4(ip.dst_addr());
                        let _ = peer
                            .send_msg_by_ip(ZCPacket::new_with_payload(&data), dst, false)
                            .await;
                    }
                    continue;
                }
                // An overlapping subnet or default-route announcement must never
                // redirect this gateway's packets to a different exit.
                let Some(id) = *selected.borrow() else {
                    continue;
                };
                let mut packet = ZCPacket::new_with_payload(&data);
                packet.fill_peer_manager_hdr(peer.my_peer_id(), id, PacketType::Data as u8);
                packet
                    .mut_peer_manager_header()
                    .unwrap()
                    .set_exit_node(true);
                let _ = peer.send_msg_for_proxy(packet, id).await;
            }
        }));
        let password = password.0;
        tasks.push(tokio::spawn(async move {
            let mut clients = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break; };
                        if clients.len() >= 256 { continue; }
                        let (net, ports, password) = (net.clone(), ports.clone(), password.clone());
                        clients.spawn(async move { let _ = serve(socket, net, ports, &password).await; });
                    }
                    _ = clients.join_next(), if !clients.is_empty() => {}
                }
            }
        }));
        Ok(Self {
            listen,
            address: cidr.addr().into(),
            exit_peer,
            tasks,
        })
    }
    pub fn set_exit_peer(&self, peer: Option<u32>) {
        self.exit_peer.send_replace(peer);
    }

    pub fn check_address(&self, address: IpAddr) -> Result<()> {
        if self.address != address || self.tasks.iter().any(|task| task.is_finished()) {
            return Err(Error::other(
                "EasyTier 虚拟地址已变化或网关已停止，请重新连接组网",
            ));
        }
        Ok(())
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

fn invalid() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, "SOCKS request rejected")
}

pub(super) async fn authenticate(socket: &mut TcpStream, password: &str) -> std::io::Result<()> {
    if socket.read_u8().await? != 5 {
        return Err(invalid());
    }
    let count = socket.read_u8().await?;
    let mut methods = vec![0; usize::from(count)];
    socket.read_exact(&mut methods).await?;
    if !methods.contains(&2) || password.is_empty() {
        socket.write_all(&[5, 255]).await?;
        return Err(invalid());
    }
    socket.write_all(&[5, 2]).await?;
    if socket.read_u8().await? != 1 {
        return Err(invalid());
    }
    let count = socket.read_u8().await?;
    let mut username = vec![0; usize::from(count)];
    socket.read_exact(&mut username).await?;
    let count = socket.read_u8().await?;
    let mut supplied = vec![0; usize::from(count)];
    socket.read_exact(&mut supplied).await?;
    if username != b"xrose" || supplied != password.as_bytes() {
        socket.write_all(&[1, 1]).await?;
        return Err(invalid());
    }
    socket.write_all(&[1, 0]).await
}

async fn read_target(socket: &mut TcpStream) -> std::io::Result<SocketAddr> {
    let ip = match socket.read_u8().await? {
        1 => {
            let mut bytes = [0; 4];
            socket.read_exact(&mut bytes).await?;
            IpAddr::V4(bytes.into())
        }
        3 => {
            let count = socket.read_u8().await?;
            let mut name = vec![0; usize::from(count)];
            socket.read_exact(&mut name).await?;
            let port = socket.read_u16().await?;
            let name = String::from_utf8(name).map_err(|_| invalid())?;
            // Proxy cores normally resolve destinations according to their DNS
            // policy. Keep SOCKS clients with domain requests interoperable.
            return tokio::net::lookup_host((name.as_str(), port))
                .await?
                .find(SocketAddr::is_ipv4)
                .ok_or_else(invalid);
        }
        _ => return Err(invalid()),
    };
    Ok(SocketAddr::new(ip, socket.read_u16().await?))
}

async fn serve(
    mut socket: TcpStream,
    net: Arc<stack::Net>,
    ports: Ports,
    password: &str,
) -> std::io::Result<()> {
    let (command, target) = tokio::time::timeout(Duration::from_secs(10), async {
        authenticate(&mut socket, password).await?;
        if socket.read_u8().await? != 5 {
            return Err(invalid());
        }
        let command = socket.read_u8().await?;
        if socket.read_u8().await? != 0 {
            return Err(invalid());
        }
        Ok((command, read_target(&mut socket).await?))
    })
    .await
    .map_err(|_| invalid())??;
    if command == 1 {
        let port = PortGuard::new(&ports, IpProtocol::Tcp)?;
        let connected = tokio::time::timeout(
            Duration::from_secs(15),
            net.tcp_connect_with_options(
                target,
                stack::TcpConnectOptions::default().with_source_port(port.port()),
            ),
        )
        .await;
        let mut upstream = match connected {
            Ok(Ok(stream)) => stream,
            _ => {
                socket.write_all(&[5, 4, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
                return Err(invalid());
            }
        };
        socket.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).await?;
        tokio::io::copy_bidirectional(&mut socket, &mut upstream).await?;
    } else if command == 3 {
        let relay = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let port = relay.local_addr()?.port().to_be_bytes();
        socket
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, port[0], port[1]])
            .await?;
        let port_guard = PortGuard::new(&ports, IpProtocol::Udp)?;
        let upstream = net
            .udp_bind(SocketAddr::new(
                Ipv4Addr::UNSPECIFIED.into(),
                port_guard.port(),
            ))
            .await?;
        let mut client = None;
        let mut sent = HashSet::new();
        let mut input = vec![0u8; 65536];
        let mut output = vec![0u8; 65536];
        let mut control = [0; 1];
        loop {
            tokio::select! {
                _ = socket.read(&mut control) => break,
                packet = relay.recv_from(&mut input) => {
                    let (count, source) = packet?;
                    if !source.ip().is_loopback() || client.is_some_and(|old| old != source) { continue; }
                    let Some((destination, body)) = udp_destination(&input[..count]).await else { continue; };
                    if sent.len() >= 256 && !sent.contains(&destination) { continue; }
                    client = Some(source); sent.insert(destination);
                    upstream.send_to(body, destination).await?;
                }
                packet = upstream.recv_from(&mut output[10..]) => {
                    let (count, source) = packet?;
                    if let (Some(client), IpAddr::V4(ip)) = (client,source.ip()) {
                        if !sent.contains(&source) { continue; }
                        output[..4].copy_from_slice(&[0,0,0,1]); output[4..8].copy_from_slice(&ip.octets()); output[8..10].copy_from_slice(&source.port().to_be_bytes());
                        relay.send_to(&output[..count+10],client).await?;
                    }
                }
                _ = tokio::time::sleep(Duration::from_secs(120)) => break,
            }
        }
    } else {
        socket.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    }
    Ok(())
}

fn decode_udp(data: &[u8]) -> Option<(SocketAddr, &[u8])> {
    if data.len() < 10 || data[..4] != [0, 0, 0, 1] {
        return None;
    }
    Some((
        SocketAddr::new(
            Ipv4Addr::new(data[4], data[5], data[6], data[7]).into(),
            u16::from_be_bytes([data[8], data[9]]),
        ),
        &data[10..],
    ))
}

async fn udp_destination(data: &[u8]) -> Option<(SocketAddr, &[u8])> {
    if let Some(destination) = decode_udp(data) {
        return Some(destination);
    }
    if data.len() < 7 || data[..4] != [0, 0, 0, 3] {
        return None;
    }
    let end = 5 + usize::from(data[4]);
    if end == 5 || data.len() < end + 2 {
        return None;
    }
    let name = std::str::from_utf8(&data[5..end]).ok()?;
    let port = u16::from_be_bytes([data[end], data[end + 1]]);
    let mut addresses = tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::lookup_host((name, port)),
    )
    .await
    .ok()?
    .ok()?;
    Some((addresses.find(SocketAddr::is_ipv4)?, &data[end + 2..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn gateway_only_intercepts_the_reserved_transport() {
        let ports = Ports::default();
        let reservation = PortGuard::new(&ports, IpProtocol::Tcp).unwrap();
        let (sender, mut received) = tokio::sync::mpsc::channel(2);
        let sink = PacketSink {
            ports,
            address: Ipv4Addr::new(192, 0, 2, 1),
            sender,
        };
        let packet = |protocol: IpProtocol| {
            let mut data = [0; 40];
            data[0] = 0x45;
            data[9] = protocol.into();
            data[16..20].copy_from_slice(&sink.address.octets());
            data[22..24].copy_from_slice(&reservation.port().to_be_bytes());
            let mut packet = ZCPacket::new_with_payload(&data);
            packet.fill_peer_manager_hdr(1, 2, PacketType::Data as u8);
            packet
        };
        assert!(
            sink.try_process_packet_from_peer(packet(IpProtocol::Udp))
                .await
                .is_some()
        );
        assert!(received.try_recv().is_err());
        assert!(
            sink.try_process_packet_from_peer(packet(IpProtocol::Tcp))
                .await
                .is_none()
        );
        assert!(received.try_recv().unwrap().is_ok());
    }

    fn virtual_pair() -> (Arc<stack::Net>, Arc<stack::Net>) {
        let (a_tx, a_rx) = tokio::sync::mpsc::channel(64);
        let (b_tx, b_rx) = tokio::sync::mpsc::channel(64);
        // Convert channel output into received IP packets, without host sockets.
        let (a_wire, mut a_wire_rx) = tokio::sync::mpsc::channel(64);
        let (b_wire, mut b_wire_rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            while let Some(packet) = a_wire_rx.recv().await {
                if b_tx.send(Ok(packet)).await.is_err() {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            while let Some(packet) = b_wire_rx.recv().await {
                if a_tx.send(Ok(packet)).await.is_err() {
                    break;
                }
            }
        });
        let make = |ip: &str, incoming, outgoing| {
            let mut caps = smoltcp::phy::DeviceCapabilities::default();
            caps.medium = smoltcp::phy::Medium::Ip;
            caps.max_transmission_unit = 1284;
            Arc::new(stack::Net::new(
                ChannelDevice {
                    incoming,
                    outgoing: tokio_util::sync::PollSender::new(outgoing),
                    caps,
                },
                stack::NetConfig::new(
                    smoltcp::iface::Config::new(smoltcp::wire::HardwareAddress::Ip),
                    ip.parse().unwrap(),
                    vec![],
                ),
            ))
        };
        (
            make("192.0.2.1/24", a_rx, a_wire),
            make("192.0.2.2/24", b_rx, b_wire),
        )
    }

    async fn authorized_client(
        net: Arc<stack::Net>,
        request: &[u8],
    ) -> (
        TcpStream,
        tokio::task::JoinHandle<std::io::Result<()>>,
        [u8; 10],
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await?;
            serve(socket, net, Ports::default(), "test").await
        });
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(&[5, 1, 2]).await.unwrap();
        let mut reply = [0; 2];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply, [5, 2]);
        client.write_all(b"\x01\x05xrose\x04test").await.unwrap();
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply, [1, 0]);
        client.write_all(request).await.unwrap();
        let mut bound = [0; 10];
        client.read_exact(&mut bound).await.unwrap();
        assert_eq!(bound[1], 0);
        (client, task, bound)
    }

    #[tokio::test]
    async fn published_peer_receives_gateway_packets_over_an_in_memory_mesh() {
        use crate::mesh::{MeshProfile, easytier::configuration};
        use easytier::{
            common::{global_ctx::GlobalCtx, stun::MockStunInfoCollector},
            peers::{create_packet_recv_chan, peer_manager::RouteAlgoType},
            tunnel::ring::create_ring_tunnel_pair,
        };

        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let client = MeshProfile {
            network_name: "fixture".into(),
            network_secret: MeshSecret("fixture".into()),
            peers: vec!["tcp://192.0.2.254:11010".into()],
            ipv4: "192.0.2.1/24".into(),
            exit_node: "192.0.2.2".into(),
            outbound_port: listener.local_addr().unwrap().port(),
            outbound_secret: MeshSecret("test".into()),
            ..Default::default()
        };
        drop(listener);
        let exit = MeshProfile {
            id: MeshProfile::default().id,
            ipv4: "192.0.2.2/24".into(),
            exit_node: String::new(),
            advertise_exit_node: true,
            outbound_port: 0,
            ..client.clone()
        };
        let manager = |profile| {
            let ctx = Arc::new(GlobalCtx::new(configuration(profile).unwrap()));
            ctx.replace_stun_info_collector(Box::new(MockStunInfoCollector {
                udp_nat_type: Default::default(),
            }));
            let (tx, rx) = create_packet_recv_chan();
            (Arc::new(PeerManager::new(RouteAlgoType::Ospf, ctx, tx)), rx)
        };
        let (local, _local_packets) = manager(&client);
        let (remote, mut remote_packets) = manager(&exit);
        tokio::time::timeout(Duration::from_secs(15), async {
            local.run().await.unwrap();
            remote.run().await.unwrap();
            let (a, b) = create_ring_tunnel_pair();
            let (client_conn, server_conn) = tokio::join!(
                local.add_client_tunnel(a, false),
                remote.add_tunnel_as_server(b, true)
            );
            client_conn.unwrap();
            server_conn.unwrap();
            let route = local.get_route();
            while !route
                .get_peer_groups(remote.my_peer_id())
                .iter()
                .any(|g| g == "xrose-exit-node-v1")
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            let gateway = Gateway::start(
                local.clone(),
                client.ipv4.clone(),
                client.outbound_port,
                client.outbound_secret.clone(),
                GatewayRoute::SelectedExit,
            )
            .await
            .unwrap();
            gateway.set_exit_peer(Some(remote.my_peer_id()));
            let mut socket = TcpStream::connect((Ipv4Addr::LOCALHOST, client.outbound_port))
                .await
                .unwrap();
            // SOCKS5 password negotiation followed by CONNECT to an off-subnet destination.
            socket
                .write_all(
                    b"\x05\x01\x02\x01\x05xrose\x04test\x05\x01\x00\x01\xcb\x00\x71\x05\x00\x50",
                )
                .await
                .unwrap();
            let packet = remote_packets.recv().await.unwrap();
            assert!(packet.peer_manager_header().unwrap().is_exit_node());
            assert_eq!(&packet.payload()[16..20], &[203, 0, 113, 5]);
            let bridge = Gateway::start(
                local,
                client.ipv4.clone(),
                0,
                MeshSecret("test".into()),
                GatewayRoute::Mesh,
            )
            .await
            .unwrap();
            let mut forwards =
                crate::mesh::forwards::Forwards::new(bridge.listen, MeshSecret("test".into()));
            let listen = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
                .unwrap()
                .local_addr()
                .unwrap();
            let rule = crate::mesh::PortForward {
                protocol: crate::mesh::ForwardProtocol::Udp,
                listen,
                target: "192.0.2.2:5353".parse().unwrap(),
            };
            forwards.apply(&[rule]).await.unwrap();
            let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            client.send_to(b"forward", listen).await.unwrap();
            loop {
                let packet = remote_packets.recv().await.unwrap();
                if packet.payload().ends_with(b"forward") {
                    assert_eq!(&packet.payload()[16..20], &[192, 0, 2, 2]);
                    assert!(!packet.peer_manager_header().unwrap().is_exit_node());
                    break;
                }
            }
        })
        .await
        .expect("exit discovery and SOCKS forwarding over the ring tunnel");
    }

    #[tokio::test]
    async fn authenticated_tcp_and_udp_use_the_virtual_network() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (target, data) = udp_destination(b"\x00\x00\x00\x03\x09192.0.2.2\x14\xe9payload")
                .await
                .unwrap();
            assert_eq!(target.to_string(), "192.0.2.2:5353");
            assert_eq!(data, b"payload");
            let (local, remote) = virtual_pair();
            let mut listener = remote
                .tcp_bind("192.0.2.2:8080".parse().unwrap())
                .await
                .unwrap();
            let echo = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut payload = [0; 4];
                stream.read_exact(&mut payload).await.unwrap();
                assert_eq!(&payload, b"test");
                stream.write_all(b"done").await.unwrap();
                stream.flush().await.unwrap();
            });
            let (mut client, task, _) =
                authorized_client(local.clone(), &[5, 1, 0, 1, 192, 0, 2, 2, 31, 144]).await;
            client.write_all(b"test").await.unwrap();
            let mut payload = [0; 4];
            client.read_exact(&mut payload).await.unwrap();
            assert_eq!(&payload, b"done");
            drop(client);
            echo.await.unwrap();
            // Drop of the simulated server removes its socket without a FIN.
            // Abort the fixture relay after verifying delivery in both directions.
            task.abort();
            let echo = remote
                .udp_bind("192.0.2.2:5353".parse().unwrap())
                .await
                .unwrap();
            let (control, task, bound) =
                authorized_client(local, &[5, 3, 0, 1, 0, 0, 0, 0, 0, 0]).await;
            let relay = SocketAddr::new(
                Ipv4Addr::LOCALHOST.into(),
                u16::from_be_bytes([bound[8], bound[9]]),
            );
            let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            udp.send_to(&[0, 0, 0, 1, 192, 0, 2, 2, 20, 233, 42], relay)
                .await
                .unwrap();
            let mut packet = [0; 64];
            let (size, source) = echo.recv_from(&mut packet).await.unwrap();
            assert_eq!(&packet[..size], &[42]);
            echo.send_to(&[43], source).await.unwrap();
            let (size, _) = udp.recv_from(&mut packet).await.unwrap();
            assert_eq!(&packet[..size], &[0, 0, 0, 1, 192, 0, 2, 2, 20, 233, 43]);
            drop(control);
            task.await.unwrap().unwrap();
        })
        .await
        .expect("virtual gateway timed out");
    }
    #[tokio::test]
    async fn live_forward_edits_keep_existing_streams_and_relay_udp_replies() {
        use crate::mesh::{ForwardProtocol, PortForward, forwards::Forwards};
        tokio::time::timeout(Duration::from_secs(10), async {
            let (local, remote) = virtual_pair();
            let proxy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let mut forwards =
                Forwards::new(proxy.local_addr().unwrap(), MeshSecret("test".into()));
            let bridge = tokio::spawn(async move {
                let mut clients = JoinSet::new();
                while let Ok((socket, _)) = proxy.accept().await {
                    clients.spawn(serve(socket, local.clone(), Ports::default(), "test"));
                }
            });
            let tcp_target: SocketAddr = "192.0.2.2:8080".parse().unwrap();
            let udp_target: SocketAddr = "192.0.2.2:5353".parse().unwrap();
            let mut remote_tcp = remote.tcp_bind(tcp_target).await.unwrap();
            let remote_udp = remote.udp_bind(udp_target).await.unwrap();
            let listen = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .unwrap()
                .local_addr()
                .unwrap();
            let tcp = PortForward {
                protocol: ForwardProtocol::Tcp,
                listen,
                target: tcp_target,
            };
            let udp = PortForward {
                protocol: ForwardProtocol::Udp,
                listen,
                target: udp_target,
            };
            forwards.apply(std::slice::from_ref(&tcp)).await.unwrap();
            let mut client = TcpStream::connect(listen).await.unwrap();
            let (mut upstream, _) = remote_tcp.accept().await.unwrap();
            forwards.apply(&[tcp.clone(), udp.clone()]).await.unwrap();
            let datagram = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            datagram.send_to(b"one", listen).await.unwrap();
            let mut buf = [0; 16];
            let (n, source) = remote_udp.recv_from(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"one");
            remote_udp.send_to(b"two", source).await.unwrap();
            let (n, _) = datagram.recv_from(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"two");
            let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let bad = PortForward {
                listen: occupied.local_addr().unwrap(),
                ..tcp.clone()
            };
            assert!(forwards.apply(&[bad, udp]).await.is_err());
            let changed = PortForward {
                target: "192.0.2.2:8081".parse().unwrap(),
                ..tcp.clone()
            };
            let mut second = remote.tcp_bind(changed.target).await.unwrap();
            forwards.apply(&[changed]).await.unwrap();
            client.write_all(b"held").await.unwrap();
            upstream.read_exact(&mut buf[..4]).await.unwrap();
            assert_eq!(&buf[..4], b"held");
            let _new_client = TcpStream::connect(listen).await.unwrap();
            let _ = second.accept().await.unwrap();
            forwards.apply(&[]).await.unwrap();
            assert!(TcpStream::connect(listen).await.is_err());
            forwards.apply(&[tcp]).await.unwrap();
            let _again = TcpStream::connect(listen).await.unwrap();
            let _ = remote_tcp.accept().await.unwrap();
            bridge.abort();
        })
        .await
        .expect("local forwarding fixture timed out");
    }
    #[test]
    fn udp_rejects_fragments_and_truncated_requests() {
        assert!(decode_udp(&[0, 0, 1, 1, 1, 1, 1, 1, 0, 53]).is_none());
        assert!(decode_udp(&[0; 9]).is_none());
        let (target, payload) = decode_udp(&[0, 0, 0, 1, 10, 0, 0, 1, 0, 53, 42]).unwrap();
        assert_eq!(target.to_string(), "10.0.0.1:53");
        assert_eq!(payload, &[42]);
    }
    #[tokio::test]
    async fn socks_rejects_missing_and_wrong_authentication() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                assert!(authenticate(&mut socket, "fake-secret").await.is_err());
            }
        });
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(&[5, 1, 0]).await.unwrap();
        let mut response = [0; 2];
        client.read_exact(&mut response).await.unwrap();
        assert_eq!(response, [5, 255]);
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(&[5, 1, 2]).await.unwrap();
        client.read_exact(&mut response).await.unwrap();
        assert_eq!(response, [5, 2]);
        client.write_all(b"\x01\x05xrose\x03bad").await.unwrap();
        client.read_exact(&mut response).await.unwrap();
        assert_eq!(response, [1, 1]);
        server.await.unwrap();
    }
}
