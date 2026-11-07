//! Independent A/AAAA lookups. The core probe sends a DNS datagram through its
//! authenticated SOCKS listener, where the applied DNS policy intercepts it.
use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{
    Error, Result,
    mesh::coexist::MeshDns,
    model::{DnsConfig, DnsServer, DnsServerType},
    runtime::location,
    settings::Language,
    updates::LocalProxy,
};
use futures_util::{StreamExt, future::join_all, stream};
use hickory_proto::{
    op::{Message, MessageType, Query, ResponseCode},
    rr::{Name, RecordType},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
};
use tokio_rustls::rustls;

const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Default)]
pub struct Comparison {
    pub host: String,
    pub rows: Vec<ProbeRow>,
    pub different_subnets: bool,
}

#[derive(Debug, Clone)]
pub struct ProbeRow {
    pub resolver: String,
    pub elapsed_ms: u64,
    pub addresses: Vec<IpAddr>,
    pub locations: Vec<String>,
    pub reserved: bool,
    pub error: Option<String>,
}

enum Resolver {
    System,
    Server(DnsServer, Option<LocalProxy>),
    Core(Option<LocalProxy>),
}

pub(crate) async fn compare(
    host: String,
    config: DnsConfig,
    mesh: Vec<MeshDns>,
    running: bool,
    direct: bool,
    socks: Option<LocalProxy>,
    http: Option<LocalProxy>,
    language: Language,
    private: bool,
) -> Comparison {
    let tr = |zh, en| if language == Language::En { en } else { zh };
    let mut targets = vec![(tr("系统解析器", "System resolver").into(), Resolver::System)];
    let public = config
        .servers
        .iter()
        .find(|s| {
            matches!(s.server_type, DnsServerType::Udp | DnsServerType::Tcp)
                && !s.requires_proxy()
                && !matches!(s.address.as_str(), "system" | "local")
        })
        .cloned()
        .unwrap_or_else(|| DnsServer::udp("AliDNS", "223.5.5.5"));
    targets.push((
        format!("{} · {}", public.tag, public.server_type.as_str()),
        Resolver::Server(public, None),
    ));
    let magic = mesh
        .iter()
        .find(|m| host == m.suffix || host.ends_with(&format!(".{}", m.suffix)))
        .or_else(|| mesh.first());
    if let Some(dns) = magic {
        targets.push((
            format!("MagicDNS · {}", dns.suffix),
            Resolver::Server(DnsServer::udp("mesh", &dns.address), None),
        ));
    } else {
        let s = config
            .servers
            .iter()
            .filter(|s| {
                matches!(s.server_type, DnsServerType::Https | DnsServerType::Tls)
                    && (!s.requires_proxy()
                        || (!direct && http.is_some() && s.server_type == DnsServerType::Https))
            })
            .min_by_key(|s| s.requires_proxy())
            .cloned()
            .unwrap_or_else(|| DnsServer::https("DNSPod", "doh.pub"));
        targets.push((
            format!("{} · {}", s.tag, s.server_type.as_str()),
            Resolver::Server(s, http),
        ));
    }
    if running && config.enabled {
        targets.push((tr("内核 DNS", "Core DNS").into(), Resolver::Core(socks)));
    }
    let mut rows = join_all(
        targets
            .into_iter()
            .map(|(name, target)| probe_one(&host, name, target, language)),
    )
    .await;
    let different_subnets = different_subnets(&rows);
    let ips: BTreeSet<_> = rows
        .iter()
        .flat_map(|r| r.addresses.iter().copied())
        .collect();
    let mut labels = BTreeMap::new();
    if !private {
        let mut jobs = stream::iter(
            ips.into_iter()
                .filter(|ip| location::is_public(*ip))
                .take(16)
                .map(|ip| async move {
                    let result =
                        tokio::time::timeout(TIMEOUT, location::lookup(&ip.to_string(), language))
                            .await;
                    (ip, result.ok().and_then(|r| r.label))
                }),
        )
        .buffer_unordered(8);
        while let Some((ip, label)) = jobs.next().await {
            if let Some(label) = label {
                labels.insert(ip, label);
            }
        }
    }
    for row in &mut rows {
        row.locations = row
            .addresses
            .iter()
            .map(|ip| {
                if !location::is_public(*ip) {
                    tr("本地 / 保留地址", "Local / reserved").to_owned()
                } else {
                    labels.get(ip).cloned().unwrap_or_else(|| {
                        tr("公网 · 归属地未知", "Public · location unknown").into()
                    })
                }
            })
            .collect();
    }
    Comparison {
        host,
        rows,
        different_subnets,
    }
}

// Only compare corresponding families. An IPv6-only resolver is not evidence
// of an inconsistent IPv4 answer. CDNs legitimately return multiple prefixes.
fn different_subnets(rows: &[ProbeRow]) -> bool {
    let networks = |row: &ProbeRow, v4| -> BTreeSet<_> {
        row.addresses
            .iter()
            .filter(|ip| ip.is_ipv4() == v4)
            .filter_map(|ip| {
                ipnet::IpNet::new(*ip, if v4 { 24 } else { 48 })
                    .ok()
                    .map(|n| n.trunc())
            })
            .collect()
    };
    [true, false].into_iter().any(|v4| {
        let sets: Vec<_> = rows
            .iter()
            .map(|r| networks(r, v4))
            .filter(|s| !s.is_empty())
            .collect();
        sets.iter()
            .enumerate()
            .any(|(i, s)| sets[i + 1..].iter().any(|t| s.is_disjoint(t)))
    })
}

async fn probe_one(host: &str, resolver: String, source: Resolver, lang: Language) -> ProbeRow {
    let started = Instant::now();
    let timed = |result: std::result::Result<Result<Vec<IpAddr>>, tokio::time::error::Elapsed>| {
        result.unwrap_or_else(|_| {
            Err(Error::other(if lang == Language::En {
                "Timed out after 3 s"
            } else {
                "解析超时（3 秒）"
            }))
        })
    };
    let (mut addresses, error) = if matches!(source, Resolver::System) {
        match timed(
            tokio::time::timeout(TIMEOUT, async {
                Ok(tokio::net::lookup_host((host, 0))
                    .await?
                    .map(|a| a.ip())
                    .collect())
            })
            .await,
        ) {
            Ok(ips) => (ips, None),
            Err(e) => (Vec::new(), Some(e.to_string())),
        }
    } else {
        let query_family = |ty| tokio::time::timeout(TIMEOUT, query(&source, host, ty));
        let (a, aaaa) = tokio::join!(query_family(RecordType::A), query_family(RecordType::AAAA));
        let mut addresses = Vec::new();
        let mut errors = Vec::new();
        for (label, result) in [("A", timed(a)), ("AAAA", timed(aaaa))] {
            match result {
                Ok(values) => addresses.extend(values),
                Err(e) => errors.push(format!("{label}: {e}")),
            }
        }
        (addresses, (!errors.is_empty()).then(|| errors.join(" · ")))
    };
    addresses.sort();
    addresses.dedup();
    ProbeRow {
        resolver,
        elapsed_ms: started.elapsed().as_millis() as u64,
        reserved: addresses.iter().any(|ip| !location::is_public(*ip)),
        addresses,
        locations: Vec::new(),
        error,
    }
}

async fn query(source: &Resolver, host: &str, ty: RecordType) -> Result<Vec<IpAddr>> {
    let mut name = Name::from_ascii(host).map_err(|_| Error::config("请输入有效域名"))?;
    name.set_fqdn(true);
    let id = uuid::Uuid::new_v4().as_u128() as u16;
    let mut request = Message::new();
    request
        .set_id(id)
        .set_recursion_desired(true)
        .add_query(Query::query(name, ty));
    let bytes = request
        .to_vec()
        .map_err(|_| Error::other("无法构建 DNS 查询"))?;
    let response = match source {
        Resolver::System => unreachable!(),
        Resolver::Core(proxy) => {
            let proxy = proxy
                .as_ref()
                .ok_or_else(|| Error::other("请启用本地 SOCKS 或混合入站"))?;
            let stream = TcpStream::connect((proxy.host.as_str(), proxy.port)).await?;
            let bind = if stream.local_addr()?.is_ipv4() {
                "0.0.0.0:0"
            } else {
                "[::]:0"
            };
            let auth = proxy.account.as_ref().map(|a| async_socks5::Auth {
                username: a.username.clone(),
                password: a.password.clone(),
            });
            let tunnel = async_socks5::SocksDatagram::associate(
                stream,
                UdpSocket::bind(bind).await?,
                auth,
                None::<SocketAddr>,
            )
            .await
            .map_err(|_| Error::other("无法通过本地入站建立 UDP 中继"))?;
            // The reserved destination avoids querying a real external resolver
            // if the core does not intercept this DNS datagram.
            let dest = SocketAddr::from(([192, 0, 2, 53], 53));
            tunnel
                .send_to(&bytes, dest)
                .await
                .map_err(|_| Error::other("无法发送内核 DNS 查询"))?;
            let mut buf = vec![0; 65535];
            let (n, _) = tunnel
                .recv_from(&mut buf)
                .await
                .map_err(|_| Error::other("未收到内核 DNS 响应"))?;
            buf.truncate(n);
            buf
        }
        Resolver::Server(s, proxy) if s.server_type == DnsServerType::Https => {
            let mut builder = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none());
            if s.requires_proxy() {
                let proxy = proxy
                    .as_ref()
                    .ok_or_else(|| Error::other("此服务器需要运行中的代理入站"))?;
                builder = builder.proxy(proxy.http_proxy()?);
            }
            let mut response = builder
                .build()?
                .post(s.display_address())
                .header("Content-Type", "application/dns-message")
                .header("Accept", "application/dns-message")
                .body(bytes)
                .send()
                .await
                .map_err(|_| Error::other("DoH 请求失败，请检查地址与出站"))?
                .error_for_status()
                .map_err(|_| Error::other("DoH 服务返回错误状态"))?;
            let mut data = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| Error::other("读取 DoH 响应失败"))?
            {
                if data.len() + chunk.len() > 65535 {
                    return Err(Error::other("DoH 响应过长"));
                }
                data.extend_from_slice(&chunk);
            }
            data
        }
        Resolver::Server(s, _) => {
            if s.requires_proxy() {
                return Err(Error::other("此 DNS 上游要求代理出站，不能直接探测"));
            }
            let tls = s.server_type == DnsServerType::Tls;
            let (host, port) = endpoint(&s.address, if tls { 853 } else { 53 })?;
            let addr = tokio::net::lookup_host((host.as_str(), port))
                .await?
                .next()
                .ok_or_else(|| Error::other("无法解析 DNS 服务器地址"))?;
            if s.server_type == DnsServerType::Udp {
                let socket = UdpSocket::bind(if addr.is_ipv4() {
                    "0.0.0.0:0"
                } else {
                    "[::]:0"
                })
                .await?;
                socket.connect(addr).await?;
                socket.send(&bytes).await?;
                let mut buf = vec![0; 65535];
                let n = socket.recv(&mut buf).await?;
                buf.truncate(n);
                if Message::from_vec(&buf).is_ok_and(|m| m.id() == id && m.truncated()) {
                    stream_query(TcpStream::connect(addr).await?, &bytes).await?
                } else {
                    buf
                }
            } else if tls {
                let roots = rustls::RootCertStore::from_iter(
                    webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
                );
                let config = rustls::ClientConfig::builder_with_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .map_err(|_| Error::other("初始化 TLS 失败"))?
                .with_root_certificates(roots)
                .with_no_client_auth();
                let name = rustls::pki_types::ServerName::try_from(host)
                    .map_err(|_| Error::config("DoT 服务器名无效"))?;
                let tls = tokio_rustls::TlsConnector::from(Arc::new(config))
                    .connect(name, TcpStream::connect(addr).await?)
                    .await?;
                stream_query(tls, &bytes).await?
            } else {
                stream_query(TcpStream::connect(addr).await?, &bytes).await?
            }
        }
    };
    answers(&request, &response)
}

fn endpoint(value: &str, default_port: u16) -> Result<(String, u16)> {
    if let Ok(addr) = value.parse::<SocketAddr>() {
        return Ok((addr.ip().to_string(), addr.port()));
    }
    if let Ok(ip) = value.parse::<IpAddr>() {
        return Ok((ip.to_string(), default_port));
    }
    let url = url::Url::parse(&format!("dns://{value}"))
        .map_err(|_| Error::config("DNS 服务器地址无效"))?;
    let host = url
        .host_str()
        .ok_or_else(|| Error::config("DNS 服务器地址为空"))?;
    Ok((
        host.trim_matches(['[', ']']).into(),
        url.port().unwrap_or(default_port),
    ))
}

async fn stream_query(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    bytes: &[u8],
) -> Result<Vec<u8>> {
    stream.write_u16(bytes.len() as u16).await?;
    stream.write_all(bytes).await?;
    let len = stream.read_u16().await?;
    let mut buf = vec![0; len as usize];
    stream.read_exact(&mut buf).await?;
    Ok(buf)
}

fn answers(request: &Message, packet: &[u8]) -> Result<Vec<IpAddr>> {
    let message = Message::from_vec(packet).map_err(|_| Error::other("DNS 响应格式无效"))?;
    if message.message_type() != MessageType::Response
        || message.id() != request.id()
        || message.queries() != request.queries()
    {
        return Err(Error::other("DNS 回复与查询不匹配"));
    }
    if message.response_code() != ResponseCode::NoError {
        return Err(Error::other(format!(
            "DNS 返回 {}",
            message.response_code()
        )));
    }
    Ok(message
        .answers()
        .iter()
        .filter(|r| r.record_type() == request.queries()[0].query_type())
        .filter_map(|r| r.data().ip_addr())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_proto::rr::{RData, Record, rdata::A};

    #[tokio::test]
    async fn queries_both_families_through_reserved_loopback_fixture() {
        let socket = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = socket.local_addr().unwrap();
        let responder = tokio::spawn(async move {
            let mut seen = BTreeSet::new();
            let mut buf = vec![0u8; 2048];
            for _ in 0..2 {
                let (n, peer) = socket.recv_from(&mut buf).await.unwrap();
                let mut message = Message::from_vec(&buf[..n]).unwrap();
                let question = message.queries()[0].clone();
                seen.insert(question.query_type());
                message.set_message_type(MessageType::Response);
                if question.query_type() == RecordType::A {
                    message.add_answer(Record::from_rdata(
                        question.name().clone(),
                        30,
                        RData::A(A::new(192, 0, 2, 7)),
                    ));
                }
                socket
                    .send_to(&message.to_vec().unwrap(), peer)
                    .await
                    .unwrap();
            }
            seen
        });
        let server = Resolver::Server(DnsServer::udp("fixture", &addr.to_string()), None);
        let row = probe_one("example.org", "fixture".into(), server, Language::Zh).await;
        assert!(row.error.is_none(), "{:?}", row.error);
        assert_eq!(row.addresses, vec![IpAddr::from([192, 0, 2, 7])]);
        assert!(row.reserved);
        assert_eq!(
            responder.await.unwrap(),
            BTreeSet::from([RecordType::A, RecordType::AAAA])
        );
    }

    #[test]
    fn validates_query_identity_and_reads_wire_answer() {
        let name = Name::from_ascii("example.org.").unwrap();
        let mut request = Message::new();
        request
            .set_id(12)
            .add_query(Query::query(name.clone(), RecordType::A));
        let mut response = request.clone();
        response
            .set_message_type(MessageType::Response)
            .add_answer(Record::from_rdata(name, 60, RData::A(A::new(192, 0, 2, 3))));
        assert_eq!(
            answers(&request, &response.to_vec().unwrap()).unwrap(),
            vec![IpAddr::from([192, 0, 2, 3])]
        );
        response.set_id(13);
        assert!(answers(&request, &response.to_vec().unwrap()).is_err());
    }
}
