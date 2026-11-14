//! Bounded userspace exit forwarding into the authenticated local policy router.
//! Peer ACLs run before this filter. Captured traffic never uses host destination sockets.
use super::{
    ExitBridge,
    easytier_gateway::{ChannelDevice, socket_buffers},
};
use crate::{Error, Result};
use async_socks5::{AddrKind, Auth, SocksDatagram};
use easytier::{
    peers::{PeerPacketFilter, peer_manager::PeerManager},
    tunnel::packet_def::{PacketType, ZCPacket},
};
use std::{
    collections::HashMap,
    io,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    net::{TcpStream, UdpSocket},
    sync::mpsc,
    task::JoinSet,
};
use tokio_smoltcp::{self as stack, smoltcp};
use tokio_util::task::AbortOnDropHandle;

const STACK_ADDRESS: Ipv4Addr = Ipv4Addr::new(198, 18, 0, 1);
const MAX_FLOWS: usize = 128;
const MAX_DATAGRAM: usize = 8192;
const MAX_FRAGMENTS: usize = 32;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const UDP_IDLE: Duration = Duration::from_secs(60);
const FRAGMENT_TTL: Duration = Duration::from_secs(10);

type Input = (u32, Vec<u8>);

struct Filter {
    input: mpsc::Sender<Input>,
    address: Ipv4Addr,
}
#[async_trait::async_trait]
impl PeerPacketFilter for Filter {
    async fn try_process_packet_from_peer(&self, packet: ZCPacket) -> Option<ZCPacket> {
        let Some(header) = packet.peer_manager_header() else {
            return Some(packet);
        };
        if header.packet_type != PacketType::Data as u8
            || (header.is_no_proxy() && !header.is_exit_node())
        {
            return Some(packet);
        }
        let dns = ipv4(packet.payload()).is_some_and(|ip| {
            ip.destination == self.address
                && matches!(ip.protocol, 6 | 17)
                && ip.fragment_offset == 0
                && ip.payload.get(2..4) == Some(&53u16.to_be_bytes())
        });
        if !header.is_exit_node() && !dns {
            return Some(packet);
        }
        if packet.payload().len() <= MAX_DATAGRAM {
            let _ = self
                .input
                .try_send((header.from_peer_id.get(), packet.payload().to_vec()));
        }
        // Queue saturation, unsupported protocols and malformed packets fail closed.
        None
    }
}

pub(super) struct Gateway {
    task: AbortOnDropHandle<()>,
    address: Ipv4Addr,
}
impl Gateway {
    pub async fn start(
        peer: Arc<PeerManager>,
        address: Ipv4Addr,
        bridge: ExitBridge,
    ) -> Result<Self> {
        bridge.validate()?;
        let (input, packets) = mpsc::channel(128);
        peer.add_packet_process_pipeline(Box::new(Filter { input, address }))
            .await;
        Ok(Self {
            task: AbortOnDropHandle::new(tokio::spawn(run(peer, bridge, packets))),
            address,
        })
    }
    pub fn check_address(&self, address: Ipv4Addr) -> Result<()> {
        if self.address != address || self.task.is_finished() {
            return Err(Error::other(
                "出口分流已停止或虚拟地址已变化，请重新连接组网",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Flow {
    peer: u32,
    protocol: u8,
    source: SocketAddrV4,
    destination: SocketAddrV4,
}
struct Slot {
    flow: Flow,
    generation: u64,
    finished: Option<Instant>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct FragmentKey {
    peer: u32,
    source: Ipv4Addr,
    destination: Ipv4Addr,
    protocol: u8,
    id: u16,
}
struct Fragment {
    header: Option<Vec<u8>>,
    pieces: Vec<(usize, Vec<u8>)>,
    length: Option<usize>,
    started: Instant,
}
#[derive(Default)]
struct Reassembly {
    packets: HashMap<FragmentKey, Fragment>,
}
impl Reassembly {
    fn receive(&mut self, peer: u32, data: Vec<u8>) -> Option<Vec<u8>> {
        let ip = ipv4(&data)?;
        if !matches!(ip.protocol, 6 | 17) {
            return None;
        }
        if ip.fragment_offset == 0 && !ip.more_fragments {
            return Some(data[..ip.total].to_vec());
        }
        self.packets
            .retain(|_, p| p.started.elapsed() < FRAGMENT_TTL);
        let key = ip.fragment_key(peer);
        if self.packets.len() >= MAX_FRAGMENTS && !self.packets.contains_key(&key) {
            return None;
        }
        let end = ip.fragment_offset.checked_add(ip.payload.len())?;
        if end > MAX_DATAGRAM - 60 || (ip.more_fragments && ip.payload.len() % 8 != 0) {
            self.packets.remove(&key);
            return None;
        }
        let entry = self.packets.entry(key).or_insert_with(|| Fragment {
            header: None,
            pieces: Vec::new(),
            length: None,
            started: Instant::now(),
        });
        if entry
            .pieces
            .iter()
            .any(|(start, piece)| *start == ip.fragment_offset && piece.as_slice() == ip.payload)
        {
            return None;
        }
        if entry.pieces.len() >= 64
            || entry
                .pieces
                .iter()
                .any(|(start, piece)| *start < end && ip.fragment_offset < *start + piece.len())
            || entry
                .length
                .is_some_and(|length| end > length || (!ip.more_fragments && end != length))
        {
            self.packets.remove(&key);
            return None;
        }
        if ip.fragment_offset == 0 {
            entry.header = Some(data[..ip.header].to_vec());
        }
        if !ip.more_fragments {
            if entry
                .pieces
                .iter()
                .any(|(start, piece)| *start + piece.len() > end)
            {
                self.packets.remove(&key);
                return None;
            }
            entry.length = Some(end);
        }
        entry.pieces.push((ip.fragment_offset, ip.payload.to_vec()));
        let length = entry.length?;
        let header = entry.header.as_ref()?;
        if entry
            .pieces
            .iter()
            .map(|(_, piece)| piece.len())
            .sum::<usize>()
            != length
        {
            return None;
        }
        let mut packet = vec![0; header.len() + length];
        packet[..header.len()].copy_from_slice(header);
        for (offset, piece) in &entry.pieces {
            packet[header.len() + offset..header.len() + offset + piece.len()]
                .copy_from_slice(piece);
        }
        let total = packet.len() as u16;
        packet[2..4].copy_from_slice(&total.to_be_bytes());
        packet[6..8].fill(0);
        fix_ip_checksum(&mut packet);
        self.packets.remove(&key);
        Some(packet)
    }
}

struct Ipv4<'a> {
    source: Ipv4Addr,
    destination: Ipv4Addr,
    protocol: u8,
    id: u16,
    fragment_offset: usize,
    more_fragments: bool,
    header: usize,
    total: usize,
    payload: &'a [u8],
}
impl Ipv4<'_> {
    fn fragment_key(&self, peer: u32) -> FragmentKey {
        FragmentKey {
            peer,
            source: self.source,
            destination: self.destination,
            protocol: self.protocol,
            id: self.id,
        }
    }
    fn sockets(&self) -> Option<(SocketAddrV4, SocketAddrV4)> {
        if self.fragment_offset != 0 {
            return None;
        }
        let ports = self.payload.get(..4)?;
        Some((
            SocketAddrV4::new(self.source, u16::from_be_bytes([ports[0], ports[1]])),
            SocketAddrV4::new(self.destination, u16::from_be_bytes([ports[2], ports[3]])),
        ))
    }
}
fn ipv4(data: &[u8]) -> Option<Ipv4<'_>> {
    if data.len() < 20 || data[0] >> 4 != 4 {
        return None;
    }
    let header = usize::from(data[0] & 15) * 4;
    let total = usize::from(u16::from_be_bytes([data[2], data[3]]));
    if header < 20
        || header > total
        || total > data.len()
        || total > MAX_DATAGRAM
        || checksum(&data[..header]) != 0
    {
        return None;
    }
    let fragment = u16::from_be_bytes([data[6], data[7]]);
    Some(Ipv4 {
        source: Ipv4Addr::new(data[12], data[13], data[14], data[15]),
        destination: Ipv4Addr::new(data[16], data[17], data[18], data[19]),
        protocol: data[9],
        id: u16::from_be_bytes([data[4], data[5]]),
        fragment_offset: usize::from(fragment & 0x1fff) * 8,
        more_fragments: fragment & 0x2000 != 0,
        header,
        total,
        payload: &data[header..total],
    })
}
fn checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    for word in data.chunks(2) {
        sum += u32::from(u16::from_be_bytes([word[0], *word.get(1).unwrap_or(&0)]));
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}
fn fix_ip_checksum(packet: &mut [u8]) {
    let header = usize::from(packet[0] & 15) * 4;
    packet[10..12].fill(0);
    let value = checksum(&packet[..header]);
    packet[10..12].copy_from_slice(&value.to_be_bytes());
}

/// RFC 1624 checksum adjustment also works on the first fragment of a datagram.
fn adjust(checksum: u16, old: &[u8], new: &[u8]) -> u16 {
    let mut sum = u32::from(!checksum);
    for (old, new) in old.chunks_exact(2).zip(new.chunks_exact(2)) {
        sum += u32::from(!u16::from_be_bytes([old[0], old[1]]));
        sum += u32::from(u16::from_be_bytes([new[0], new[1]]));
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}
fn rewrite(packet: &mut [u8], source: bool, address: Ipv4Addr, port: u16) -> Option<()> {
    let ip = ipv4(packet)?;
    let header = ip.header;
    let first = ip.fragment_offset == 0;
    let protocol = ip.protocol;
    let address_offset = if source { 12 } else { 16 };
    let port_offset = header + if source { 0 } else { 2 };
    if first {
        let minimum = if protocol == 6 {
            20
        } else if protocol == 17 {
            8
        } else {
            return None;
        };
        if ip.payload.len() < minimum {
            return None;
        }
        let check_offset = header + if protocol == 6 { 16 } else { 6 };
        let original = u16::from_be_bytes([packet[check_offset], packet[check_offset + 1]]);
        if original != 0 || protocol == 6 {
            let value = adjust(
                original,
                &packet[address_offset..address_offset + 4],
                &address.octets(),
            );
            let value = adjust(
                value,
                &packet[port_offset..port_offset + 2],
                &port.to_be_bytes(),
            );
            let value = if protocol == 17 && value == 0 {
                0xffff
            } else {
                value
            };
            packet[check_offset..check_offset + 2].copy_from_slice(&value.to_be_bytes());
        }
        packet[port_offset..port_offset + 2].copy_from_slice(&port.to_be_bytes());
    }
    packet[address_offset..address_offset + 4].copy_from_slice(&address.octets());
    fix_ip_checksum(packet);
    Some(())
}

async fn run(peer: Arc<PeerManager>, bridge: ExitBridge, mut packets: mpsc::Receiver<Input>) {
    let (input, received) = mpsc::channel(64);
    let (output, mut replies) = mpsc::channel(64);
    let mut caps = smoltcp::phy::DeviceCapabilities::default();
    caps.medium = smoltcp::phy::Medium::Ip;
    caps.max_transmission_unit = 1280;
    caps.max_burst_size = Some(8);
    let mut interface = smoltcp::iface::Config::new(smoltcp::wire::HardwareAddress::Ip);
    interface.random_seed = uuid::Uuid::new_v4().as_u128() as u64;
    let mut config = stack::NetConfig::new(
        interface,
        smoltcp::wire::IpCidr::new(STACK_ADDRESS.into(), 32),
        vec![STACK_ADDRESS.into()],
    );
    config.buffer_size = socket_buffers();
    let net = Arc::new(stack::Net::new(
        ChannelDevice {
            incoming: received,
            outgoing: tokio_util::sync::PollSender::new(output),
            caps,
        },
        config,
    ));
    let mut flows = HashMap::<Flow, u16>::new();
    let mut slots = HashMap::<u16, Slot>::new();
    let mut reassembly = Reassembly::default();
    let mut outgoing_fragments = HashMap::<FragmentKey, (u16, Instant)>::new();
    let mut tasks = JoinSet::new();
    let mut next_port = 20000u16;
    let mut generation = 0u64;
    let mut cleanup = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            packet = packets.recv() => {
                let Some((sender, packet)) = packet else { break; };
                let Some(mut packet) = reassembly.receive(sender, packet) else { continue; };
                let Some(ip) = ipv4(&packet) else { continue; };
                let Some((source, destination)) = ip.sockets() else { continue; };
                let flow = Flow {peer: sender, source, destination, protocol: ip.protocol};
                let port = if let Some(port) = flows.get(&flow) { *port } else {
                    if slots.len() >= MAX_FLOWS { continue; }
                    // Non-SYN TCP packets cannot create a fresh flow after its session ended.
                    if flow.protocol == 6 && !ip.payload.get(13).is_some_and(|flags| flags & 0x12 == 2) { continue; }
                    while slots.contains_key(&next_port) { next_port = if next_port == 60000 { 20000 } else { next_port + 1 }; }
                    let port = next_port;
                    next_port = if next_port == 60000 { 20000 } else { next_port + 1 };
                    generation += 1;
                    let id = generation;
                    let address = SocketAddr::from((STACK_ADDRESS, port));
                    let bridge = bridge.clone();
                    if flow.protocol == 6 {
                        let Ok(listener) = net.tcp_bind(address).await else { continue; };
                        tasks.spawn(async move { let _ = tcp(listener, flow, bridge).await; (port, id) });
                    } else {
                        let Ok(socket) = net.udp_bind(address).await else { continue; };
                        tasks.spawn(async move { let _ = udp(socket, flow, bridge).await; (port, id) });
                    }
                    slots.insert(port, Slot { flow, generation: id, finished: None });
                    flows.insert(flow, port);
                    port
                };
                if rewrite(&mut packet, false, STACK_ADDRESS, port).is_some() {
                    let _ = input.try_send(Ok(packet));
                }
            }
            packet = replies.recv() => {
                let Some(mut packet) = packet else { break; };
                let Some(ip) = ipv4(&packet) else { continue; };
                let fragment = ip.fragment_key(0);
                let port = if let Some((source, _)) = ip.sockets() {
                    if ip.more_fragments {
                        if outgoing_fragments.len() >= MAX_FRAGMENTS { continue; }
                        outgoing_fragments.insert(fragment, (source.port(), Instant::now()));
                    }
                    source.port()
                } else {
                    let Some((port, _)) = outgoing_fragments.get(&fragment) else { continue; };
                    *port
                };
                let Some(slot) = slots.get(&port) else { continue; };
                if ip.destination != *slot.flow.source.ip() || ip.protocol != slot.flow.protocol { continue; }
                let flow = slot.flow;
                if !ip.more_fragments { outgoing_fragments.remove(&fragment); }
                if rewrite(&mut packet, true, *flow.destination.ip(), flow.destination.port()).is_none() { continue; }
                let mut packet = ZCPacket::new_with_payload(&packet);
                packet.fill_peer_manager_hdr(peer.my_peer_id(), flow.peer, PacketType::Data as u8);
                packet.mut_peer_manager_header().unwrap().set_no_proxy(true);
                let _ = tokio::time::timeout(Duration::from_secs(1), peer.send_msg_for_proxy(packet, flow.peer)).await;
            }
            completed = tasks.join_next(), if !tasks.is_empty() => {
                match completed {
                    Some(Ok((port, id))) => {
                        if let Some(slot) = slots.get_mut(&port).filter(|slot| slot.generation == id) {
                            slot.finished = Some(Instant::now());
                        }
                    }
                    Some(Err(_)) => break,
                    None => {},
                }
            }
            _ = cleanup.tick() => {
                // Leave a short interval for the stack to transmit FIN/RST packets.
                slots.retain(|_, slot| {
                    let keep = slot.finished.is_none_or(|time| time.elapsed() < Duration::from_secs(2));
                    if !keep { flows.remove(&slot.flow); }
                    keep
                });
                reassembly.packets.retain(|_, fragment| fragment.started.elapsed() < FRAGMENT_TTL);
                outgoing_fragments.retain(|_, (_, time)| time.elapsed() < FRAGMENT_TTL);
            }
        }
    }
}

fn failed() -> io::Error {
    io::Error::other("Exit policy connection failed")
}
async fn tcp(mut listener: stack::TcpListener, flow: Flow, bridge: ExitBridge) -> io::Result<()> {
    let (mut source, address) = tokio::time::timeout(CONNECT_TIMEOUT, listener.accept())
        .await
        .map_err(|_| failed())??;
    drop(listener);
    if address != SocketAddr::V4(flow.source) {
        return Err(failed());
    }
    let mut target = tokio::time::timeout(CONNECT_TIMEOUT, async {
        let mut target = TcpStream::connect(bridge.address()).await?;
        async_socks5::connect(
            &mut target,
            SocketAddr::V4(flow.destination),
            Some(Auth::new("xrose", bridge.password.0)),
        )
        .await
        .map_err(|_| failed())?;
        Ok::<_, io::Error>(target)
    })
    .await
    .map_err(|_| failed())??;
    tokio::io::copy_bidirectional(&mut source, &mut target).await?;
    Ok(())
}
async fn udp(socket: stack::UdpSocket, flow: Flow, bridge: ExitBridge) -> io::Result<()> {
    let relay = tokio::time::timeout(CONNECT_TIMEOUT, async {
        let control = TcpStream::connect(bridge.address()).await?;
        let local = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let address = local.local_addr()?;
        SocksDatagram::associate(
            control,
            local,
            Some(Auth::new("xrose", bridge.password.0)),
            Some(address),
        )
        .await
        .map_err(|_| failed())
    })
    .await
    .map_err(|_| failed())??;
    let mut sent = [0; MAX_DATAGRAM];
    let mut received = [0; MAX_DATAGRAM];
    loop {
        tokio::select! {
            packet = socket.recv_from(&mut sent) => {
                let (n, source) = packet?;
                if source != SocketAddr::V4(flow.source) { continue; }
                relay.send_to(&sent[..n], SocketAddr::V4(flow.destination)).await.map_err(|_| failed())?;
            }
            packet = relay.recv_from(&mut received) => {
                let (n, source) = packet.map_err(|_| failed())?;
                if source != AddrKind::Ip(SocketAddr::V4(flow.destination)) { continue; }
                socket.send_to(&received[..n], SocketAddr::V4(flow.source)).await?;
            }
            _ = tokio::time::sleep(UDP_IDLE) => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn udp_packet(payload: &[u8]) -> Vec<u8> {
        let mut packet = vec![0; 28 + payload.len()];
        packet[0] = 0x45;
        let length = packet.len() as u16;
        packet[2..4].copy_from_slice(&length.to_be_bytes());
        packet[4..6].copy_from_slice(&9u16.to_be_bytes());
        packet[8] = 64;
        packet[9] = 17;
        packet[12..16].copy_from_slice(&[192, 0, 2, 1]);
        packet[16..20].copy_from_slice(&[203, 0, 113, 5]);
        packet[20..22].copy_from_slice(&40000u16.to_be_bytes());
        packet[22..24].copy_from_slice(&53u16.to_be_bytes());
        packet[24..26].copy_from_slice(&((payload.len() + 8) as u16).to_be_bytes());
        packet[28..].copy_from_slice(payload);
        let mut pseudo = packet[12..20].to_vec();
        pseudo.extend([0, 17]);
        pseudo.extend(((payload.len() + 8) as u16).to_be_bytes());
        pseudo.extend(&packet[20..]);
        packet[26..28].copy_from_slice(&checksum(&pseudo).to_be_bytes());
        fix_ip_checksum(&mut packet);
        packet
    }
    fn fragments(packet: &[u8], size: usize) -> Vec<Vec<u8>> {
        let payload = &packet[20..];
        payload
            .chunks(size)
            .enumerate()
            .map(|(index, data)| {
                let mut fragment = packet[..20].to_vec();
                fragment.extend(data);
                let length = fragment.len() as u16;
                fragment[2..4].copy_from_slice(&length.to_be_bytes());
                let more = if (index + 1) * size < payload.len() {
                    0x2000
                } else {
                    0
                };
                fragment[6..8].copy_from_slice(&(more | (index * size / 8) as u16).to_be_bytes());
                fix_ip_checksum(&mut fragment);
                fragment
            })
            .collect()
    }
    fn valid_udp(packet: &[u8]) -> bool {
        let ip = ipv4(packet).unwrap();
        let mut pseudo = packet[12..20].to_vec();
        pseudo.extend([0, 17]);
        pseudo.extend((ip.payload.len() as u16).to_be_bytes());
        pseudo.extend(ip.payload);
        checksum(&pseudo) == 0
    }

    #[test]
    fn translation_preserves_checksums_and_fragment_reassembly_is_bounded() {
        let original = udp_packet(&vec![42; 3000]);
        assert!(valid_udp(&original));
        let mut translated = original.clone();
        rewrite(&mut translated, false, STACK_ADDRESS, 20000).unwrap();
        assert!(valid_udp(&translated));
        rewrite(&mut translated, false, Ipv4Addr::new(203, 0, 113, 5), 53).unwrap();
        assert_eq!(translated, original);
        let mut reassembly = Reassembly::default();
        let mut parts = fragments(&original, 1200);
        parts.reverse();
        let mut restored = None;
        for part in parts {
            restored = reassembly.receive(7, part).or(restored);
        }
        assert_eq!(restored.unwrap(), original);
        let mut fragments = fragments(&original, 1200);
        let first = fragments.remove(0);
        for id in 0..MAX_FRAGMENTS + 5 {
            let mut part = first.clone();
            part[4..6].copy_from_slice(&(id as u16).to_be_bytes());
            fix_ip_checksum(&mut part);
            assert!(reassembly.receive(7, part).is_none());
        }
        assert_eq!(reassembly.packets.len(), MAX_FRAGMENTS);
    }

    #[test]
    fn fragment_translation_preserves_udp_checksum_and_rejects_overlap() {
        let original = udp_packet(&vec![42; 3000]);
        let mut reassembly = Reassembly::default();
        let mut received = None;
        for mut part in fragments(&original, 1200) {
            rewrite(&mut part, true, Ipv4Addr::new(198, 51, 100, 2), 443).unwrap();
            received = reassembly.receive(8, part).or(received);
        }
        assert!(valid_udp(&received.unwrap()));
        let parts = fragments(&original, 1200);
        assert!(reassembly.receive(7, parts[0].clone()).is_none());
        let mut overlap = parts[1].clone();
        overlap[6..8].copy_from_slice(&(0x2000u16 | 100).to_be_bytes());
        fix_ip_checksum(&mut overlap);
        assert!(reassembly.receive(7, overlap).is_none());
        assert!(reassembly.packets.is_empty());
        let mut invalid = original;
        invalid[0] = 0x44;
        assert!(reassembly.receive(7, invalid).is_none());
    }

    #[tokio::test]
    async fn captured_exit_packets_never_fall_back_when_queue_is_closed() {
        let (input, receiver) = mpsc::channel(1);
        drop(receiver);
        let filter = Filter {
            input,
            address: Ipv4Addr::new(192, 0, 2, 2),
        };
        let mut packet = ZCPacket::new_with_payload(&udp_packet(b"dns"));
        packet.fill_peer_manager_hdr(1, 2, PacketType::Data as u8);
        packet
            .mut_peer_manager_header()
            .unwrap()
            .set_exit_node(true);
        assert!(filter.try_process_packet_from_peer(packet).await.is_none());
        let mut packet = ZCPacket::new_with_payload(&udp_packet(b"subnet"));
        packet.fill_peer_manager_hdr(1, 2, PacketType::Data as u8);
        assert!(filter.try_process_packet_from_peer(packet).await.is_some());
    }

    async fn policy_fixture() -> (ExitBridge, AbortOnDropHandle<()>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let bridge = ExitBridge {
            port: listener.local_addr().unwrap().port(),
            password: super::super::MeshSecret("fixture-exit-secret".into()),
        };
        let password = bridge.password.0.clone();
        let task = tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! {
                    socket = listener.accept() => {
                        let (mut socket, _) = socket.unwrap();
                        let password = password.clone();
                        tasks.spawn(async move {
                            let (command, destination) = super::super::service_gateway::read_request(&mut socket, &password).await.unwrap();
                            if command == 1 {
                                assert_eq!(destination, "203.0.113.5:443".parse().unwrap());
                                socket.write_all(&[5,0,0,1,127,0,0,1,0,0]).await.unwrap();
                                let mut body = [0; 4];
                                socket.read_exact(&mut body).await.unwrap();
                                match &body {
                                    b"ping" => socket.write_all(b"pong").await.unwrap(),
                                    b"bulk" => {
                                        let mut body = vec![0; 1024 * 1024];
                                        socket.read_exact(&mut body).await.unwrap();
                                        assert!(body.iter().all(|byte| *byte == 42));
                                        socket.write_all(&body).await.unwrap();
                                    }
                                    _ => panic!("unknown fixture request"),
                                }
                            } else {
                                assert_eq!(command, 3);
                                let relay = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
                                let port = relay.local_addr().unwrap().port().to_be_bytes();
                                socket.write_all(&[5,0,0,1,127,0,0,1,port[0],port[1]]).await.unwrap();
                                let mut buffer = [0; MAX_DATAGRAM];
                                let mut control = [0];
                                loop { tokio::select! {
                                    _ = socket.read(&mut control) => break,
                                    packet = relay.recv_from(&mut buffer) => {
                                        let (n, peer) = packet.unwrap();
                                        assert_eq!(&buffer[..10], &[0,0,0,1,203,0,113,5,0,53]);
                                        if &buffer[10..n] == b"burst" {
                                            for index in 0..32u8 {
                                                let mut packet = buffer[..10].to_vec();
                                                packet.extend(vec![index; 1200]);
                                                relay.send_to(&packet, peer).await.unwrap();
                                            }
                                        } else {
                                            relay.send_to(&buffer[..n], peer).await.unwrap();
                                        }
                                    }
                                }}
                            }
                        });
                    }
                    result = tasks.join_next(), if !tasks.is_empty() => { result.unwrap().unwrap(); }
                }
            }
        });
        (bridge, AbortOnDropHandle::new(task))
    }

    fn delayed_tunnel(
        tunnel: Box<dyn easytier::tunnel::Tunnel>,
    ) -> Box<dyn easytier::tunnel::Tunnel> {
        use futures_util::StreamExt;
        let info = tunnel.info();
        let (reader, writer) = tunnel.split();
        // Delay packets concurrently, preserving their order and the link bandwidth.
        let reader = reader
            .map(|packet| async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                packet
            })
            .buffered(1024);
        Box::new(easytier::tunnel::common::TunnelWrapper::new(
            reader, writer, info,
        ))
    }

    async fn receive_burst(udp: &SocksDatagram<TcpStream>, destination: SocketAddr) {
        let mut reply = [0; 1400];
        let mut seen = [false; 32];
        let burst = tokio::time::timeout(Duration::from_secs(2), async {
            while !seen.iter().all(|received| *received) {
                let (n, address) = udp.recv_from(&mut reply).await.unwrap();
                assert_eq!(address, AddrKind::Ip(destination));
                assert_eq!(n, 1200);
                assert!(reply[..n].iter().all(|byte| *byte == reply[0]));
                seen[usize::from(reply[0])] = true;
            }
        })
        .await;
        assert!(
            burst.is_ok(),
            "received {} of 32 UDP packets",
            seen.iter().filter(|received| **received).count()
        );
    }

    #[tokio::test]
    async fn official_exit_frames_forward_bulk_tcp_and_udp_bursts_through_policy_socks() {
        use super::super::{
            MeshProfile, MeshSecret, easytier::configuration, easytier_gateway::GatewayRoute,
        };
        use easytier::{
            common::{global_ctx::GlobalCtx, stun::MockStunInfoCollector},
            peers::{create_packet_recv_chan, peer_manager::RouteAlgoType},
            tunnel::ring::create_ring_tunnel_pair,
        };
        tokio::time::timeout(Duration::from_secs(20), async {
            let profile = |address: &str| MeshProfile {
                network_name: "fixture".into(),
                network_secret: MeshSecret("fixture".into()),
                peers: vec!["tcp://192.0.2.254:11010".into()],
                ipv4: address.into(),
                ..Default::default()
            };
            let manager = |profile: &MeshProfile| {
                let ctx = Arc::new(GlobalCtx::new(configuration(profile).unwrap()));
                ctx.replace_stun_info_collector(Box::new(MockStunInfoCollector {
                    udp_nat_type: Default::default(),
                }));
                let (tx, rx) = create_packet_recv_chan();
                (Arc::new(PeerManager::new(RouteAlgoType::Ospf, ctx, tx)), rx)
            };
            let (local, _local_packets) = manager(&profile("192.0.2.1/24"));
            let (remote, mut uncaptured) = manager(&profile("192.0.2.2/24"));
            local.run().await.unwrap();
            remote.run().await.unwrap();
            let (a, b) = create_ring_tunnel_pair();
            let (a, b) = tokio::join!(
                local.add_client_tunnel(delayed_tunnel(a), false),
                remote.add_tunnel_as_server(delayed_tunnel(b), true)
            );
            a.unwrap();
            b.unwrap();
            let (bridge, _policy) = policy_fixture().await;
            let _exit = Gateway::start(remote.clone(), Ipv4Addr::new(192, 0, 2, 2), bridge)
                .await
                .unwrap();
            let gateway = super::super::easytier_gateway::Gateway::start(
                local,
                "192.0.2.1/24".into(),
                0,
                MeshSecret("fixture".into()),
                GatewayRoute::SelectedExit,
            )
            .await
            .unwrap();
            gateway.set_exit_peer(Some(remote.my_peer_id()));
            let mut tcp = TcpStream::connect(gateway.listen).await.unwrap();
            async_socks5::connect(
                &mut tcp,
                "203.0.113.5:443".parse::<SocketAddr>().unwrap(),
                Some(Auth::new("xrose", "fixture")),
            )
            .await
            .unwrap();
            tcp.write_all(b"ping").await.unwrap();
            let mut reply = [0; 4];
            tcp.read_exact(&mut reply).await.unwrap();
            assert_eq!(&reply, b"pong");
            let mut download = TcpStream::connect(gateway.listen).await.unwrap();
            async_socks5::connect(
                &mut download,
                "203.0.113.5:443".parse::<SocketAddr>().unwrap(),
                Some(Auth::new("xrose", "fixture")),
            )
            .await
            .unwrap();
            download.write_all(b"bulk").await.unwrap();
            // At 40 ms RTT the old 8 KiB windows need over 10 seconds for this
            // round trip. Leave scheduling headroom for the larger, bounded
            // windows on both gateways, which complete it in a few seconds.
            tokio::time::timeout(Duration::from_secs(8), async {
                let mut body = vec![42; 1024 * 1024];
                download.write_all(&body).await.unwrap();
                body.fill(0);
                download.read_exact(&mut body).await.unwrap();
                assert!(body.iter().all(|byte| *byte == 42));
            })
            .await
            .expect("bulk TCP should not stall behind an 8 KiB window");
            let control = TcpStream::connect(gateway.listen).await.unwrap();
            let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let address = socket.local_addr().unwrap();
            let udp = SocksDatagram::associate(
                control,
                socket,
                Some(Auth::new("xrose", "fixture")),
                Some(address),
            )
            .await
            .unwrap();
            let body = vec![42; 3000];
            let destination = "203.0.113.5:53".parse::<SocketAddr>().unwrap();
            udp.send_to(&body, destination).await.unwrap();
            let mut reply = vec![0; 4000];
            let (n, address) = udp.recv_from(&mut reply).await.unwrap();
            assert_eq!(address, AddrKind::Ip(destination));
            assert_eq!(&reply[..n], body);
            udp.send_to(b"burst", destination).await.unwrap();
            receive_burst(&udp, destination).await;
            for index in 0..32u8 {
                udp.send_to(&[index; 1200], destination).await.unwrap();
            }
            receive_burst(&udp, destination).await;
            assert!(
                uncaptured.try_recv().is_err(),
                "exit packets must not reach the host interface"
            );
        })
        .await
        .expect("TCP and fragmented UDP complete through the policy bridge");
    }
}
