//! Redact shared copies of log messages, keeping the original buffer and log metadata intact.
use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap};
use std::net::IpAddr;
use std::ops::Range;
use std::path::Path;
use std::sync::LazyLock;

use aho_corasick::AhoCorasick;
use ipnet::IpNet;
use regex::Regex;

use crate::mesh::MeshProfile;
use crate::model::Node;
use crate::subscription::Subscription;
use crate::{Error, Result};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Literal {
    Secret,
    Uuid,
    Subscription,
    Host(&'static str),
    Path(&'static str),
}

impl Literal {
    fn replacement(self) -> &'static str {
        match self {
            Self::Secret | Self::Uuid => "***",
            Self::Subscription => "<subscription-url>",
            Self::Host(label) | Self::Path(label) => label,
        }
    }

    fn matches(self, line: &str, range: &Range<usize>, literal: &str) -> bool {
        let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
        let before = line[..range.start].chars().next_back();
        let after = line[range.end..].chars().next();
        let matched = &line[range.clone()];
        match self {
            Self::Host(_) => {
                let host = |c: char| word(c) || c == '-' || c == '.';
                let suffix = &line[range.end..];
                let continues_host = match suffix.strip_prefix('.') {
                    Some(rest) => rest.starts_with(host),
                    None => after.is_some_and(host),
                };
                !before.is_some_and(host) && !continues_host
            }
            Self::Path(_) => {
                (matched == literal
                    || literal.contains('\\')
                    || literal.as_bytes().get(1) == Some(&b':'))
                    && after.is_none_or(|c| {
                        matches!(
                            c,
                            '/' | '\\'
                                | '"'
                                | '\''
                                | ':'
                                | ';'
                                | ','
                                | ')'
                                | ']'
                                | '}'
                                | '。'
                                | '，'
                                | '；'
                        ) || c.is_whitespace()
                    })
            }
            Self::Subscription => matched == literal,
            Self::Secret | Self::Uuid => {
                (self == Self::Uuid || matched == literal)
                    && !(before.is_some_and(word) && literal.starts_with(word))
                    && !(after.is_some_and(word) && literal.ends_with(word))
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct RedactionRules {
    literals: BTreeSet<(String, Literal)>,
    addresses: HashMap<IpAddr, &'static str>,
    pub(crate) fake_ip: Option<IpNet>,
}

impl RedactionRules {
    fn add(&mut self, value: &str, kind: Literal) {
        if !value.trim().is_empty() {
            self.literals.insert((value.to_owned(), kind));
        }
    }

    pub(crate) fn secret(&mut self, value: &str) {
        self.add(value, Literal::Secret);
    }

    pub(crate) fn path(&mut self, path: &Path, label: &'static str) {
        let Some(path) = path.to_str() else { return };
        let path = path.trim_end_matches(['/', '\\']);
        if path.is_empty() || path.ends_with(':') {
            return;
        }
        self.add(path, Literal::Path(label));
        if path.contains('\\') {
            self.add(&path.replace('\\', "/"), Literal::Path(label));
            self.add(&path.replace('\\', "\\\\"), Literal::Path(label));
        }
    }

    fn host(&mut self, host: &str, peer: bool) {
        let host = host.trim_matches(['[', ']']);
        let label = if peer { "<mesh-peer>" } else { "<node-server>" };
        if let Ok(ip) = host.parse::<IpAddr>() {
            if !peer || !diagnostic_address(ip, self.fake_ip) {
                self.addresses.entry(ip).or_insert(label);
            }
        } else {
            self.add(host.trim_end_matches('.'), Literal::Host(label));
        }
    }

    pub(crate) fn node(&mut self, node: &Node) {
        self.host(&node.server, false);
        let credentials = &node.credentials;
        for value in [
            &credentials.password,
            &credentials.obfs_password,
            &credentials.username,
        ]
        .into_iter()
        .flatten()
        {
            self.secret(value);
        }
        if let Some(value) = &credentials.uuid {
            if let Ok(uuid) = uuid::Uuid::parse_str(value) {
                self.add(&uuid.hyphenated().to_string(), Literal::Uuid);
                self.add(&uuid.simple().to_string(), Literal::Uuid);
            }
            self.secret(value);
        }
        if let Some(wg) = &credentials.wireguard {
            for key in [&wg.private_key, &wg.pre_shared_key, &wg.public_key] {
                self.secret(key);
            }
        }
        if let Some(vpn) = &credentials.openconnect {
            self.secret(&vpn.cookie);
        }
        if let Some(vpn) = &credentials.openvpn {
            for line in vpn
                .control_key
                .lines()
                .filter(|line| !line.starts_with(['-', '#']))
            {
                self.secret(line);
            }
        }
        for cert in credentials
            .openvpn
            .iter()
            .map(|v| &v.certificates)
            .chain(credentials.openconnect.iter().map(|v| &v.certificates))
        {
            self.secret(&cert.private_key_password);
            for line in cert
                .private_key_pem
                .lines()
                .filter(|line| !line.starts_with('-'))
            {
                self.secret(line);
            }
        }
    }

    pub(crate) fn subscription(&mut self, sub: &Subscription) {
        self.add(&sub.url, Literal::Subscription);
        if let Ok(url) = url::Url::parse(&sub.url) {
            self.add(url.as_str(), Literal::Subscription);
        }
        for (key, value) in &sub.headers {
            if matches!(
                key.to_ascii_lowercase().as_str(),
                "authorization" | "proxy-authorization" | "cookie" | "x-api-key"
            ) {
                self.secret(value);
            }
        }
    }

    pub(crate) fn mesh_profile(&mut self, mesh: &MeshProfile) {
        self.secret(&mesh.network_secret.0);
        self.secret(&mesh.outbound_secret.0);
        for endpoint in &mesh.peers {
            if let Ok(url) = url::Url::parse(endpoint)
                && let Some(host) = url.host_str()
            {
                self.host(host, true);
            }
        }
    }

    pub(crate) fn peer_address(&mut self, value: &str) {
        if let Ok(ip) = value.parse::<IpAddr>() {
            if !diagnostic_address(ip, self.fake_ip) {
                self.addresses.entry(ip).or_insert("<mesh-peer>");
            }
        }
    }

    pub(crate) fn build(self) -> Result<LogRedactor> {
        let literals: Vec<_> = self.literals.into_iter().collect();
        let matcher = AhoCorasick::builder()
            .ascii_case_insensitive(true)
            .build(literals.iter().map(|(s, _)| s))
            .map_err(|_| Error::other("无法准备日志脱敏规则，请重试"))?;
        Ok(LogRedactor {
            literals,
            matcher,
            addresses: self.addresses,
            fake_ip: self.fake_ip,
        })
    }
}

fn diagnostic_address(ip: IpAddr, fake_ip: Option<IpNet>) -> bool {
    if ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || fake_ip.is_some_and(|net| net.contains(&ip))
    {
        return true;
    }
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, ..] = ip.octets();
            ip.is_private()
                || ip.is_link_local()
                || ip.is_broadcast()
                || (a == 100 && (64..128).contains(&b))
                || (a == 198 && matches!(b, 18 | 19))
        }
        IpAddr::V6(ip) => {
            ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|v| diagnostic_address(v.into(), fake_ip))
        }
    }
}

static QUERY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
    r#"(?i)\b(?:token|(?:access|refresh|api)[_-]?token|(?:api[_-]?)?key|password|passwd|secret|signature|auth)=([^&#\s"'<>，；。]+)"#
).unwrap()
});
static USERINFO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)[a-z][a-z0-9+.-]*://([^/?#@\s"'<>]+)@"#).unwrap());
static ENCRYPTED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"enc:v1:[A-Za-z0-9+/=_-]+").unwrap());
static IPV4: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b(?:[0-9]{1,3}\.){3}[0-9]{1,3}\b)").unwrap());
static IPV6: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:(?-u:\b)[0-9a-f]{1,4}:[0-9a-f:.]*:|::)[0-9a-f:.]*").unwrap()
});

pub(crate) struct LogRedactor {
    literals: Vec<(String, Literal)>,
    matcher: AhoCorasick,
    addresses: HashMap<IpAddr, &'static str>,
    fake_ip: Option<IpNet>,
}

type Mask = (Range<usize>, Cow<'static, str>);

impl LogRedactor {
    fn ip_mask(&self, ip: IpAddr) -> Option<Cow<'static, str>> {
        if let Some(label) = self.addresses.get(&ip) {
            return Some(Cow::Borrowed(*label));
        }
        if diagnostic_address(ip, self.fake_ip) {
            return None;
        }
        Some(Cow::Owned(match ip {
            IpAddr::V4(ip) => {
                let [a, b, ..] = ip.octets();
                format!("{a}.{b}.*.*")
            }
            IpAddr::V6(ip) => {
                if let Some(v4) = ip.to_ipv4_mapped() {
                    return self
                        .ip_mask(v4.into())
                        .map(|mask| format!("::ffff:{mask}").into());
                }
                let [a, b, ..] = ip.segments();
                format!("{a:x}:{b:x}:•••")
            }
        }))
    }

    pub(crate) fn message<'a>(&self, line: &'a str) -> Cow<'a, str> {
        let mut masks: Vec<Mask> = Vec::new();
        for m in self.matcher.find_overlapping_iter(line) {
            let (value, kind) = &self.literals[m.pattern().as_usize()];
            let range = m.start()..m.end();
            if kind.matches(line, &range, value) {
                masks.push((range, kind.replacement().into()));
            }
        }
        for pattern in [&*QUERY, &*USERINFO] {
            for caps in pattern.captures_iter(line) {
                masks.push((caps.get(1).unwrap().range(), "***".into()));
            }
        }
        masks.extend(ENCRYPTED.find_iter(line).map(|m| (m.range(), "***".into())));
        for pattern in [&*IPV6, &*IPV4] {
            for m in pattern.find_iter(line) {
                let addr = m.as_str().trim_end_matches('.');
                // A bare IPv6 literal can be followed by the colon introducing an error.
                let parsed = addr.parse().ok().map(|ip| (addr, ip)).or_else(|| {
                    let addr = addr.strip_suffix(':')?;
                    Some((addr, addr.parse().ok()?))
                });
                if let Some((addr, ip)) = parsed
                    && let Some(mask) = self.ip_mask(ip)
                {
                    masks.push((m.start()..m.start() + addr.len(), mask));
                }
            }
        }
        if masks.is_empty() {
            return Cow::Borrowed(line);
        }
        // Match against the original text so nested secrets can never split a URL or ciphertext match.
        masks.sort_by_key(|(range, _)| (range.start, Reverse(range.end)));
        let mut out = String::with_capacity(line.len());
        let mut offset = 0;
        let mut iter = masks.into_iter().peekable();
        while let Some((mut range, mut replacement)) = iter.next() {
            while let Some((overlap, _)) = iter.next_if(|(r, _)| r.start < range.end) {
                if overlap.end > range.end {
                    range.end = overlap.end;
                    replacement = "***".into();
                }
            }
            out.push_str(&line[offset..range.start]);
            out.push_str(&replacement);
            offset = range.end;
        }
        out.push_str(&line[offset..]);
        Cow::Owned(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::MeshSecret;
    use crate::model::Protocol;

    #[test]
    fn suppresses_configured_values_without_changing_neighbors_or_paths() {
        let mut rules = RedactionRules::default();
        let mut node = Node::new("fixture", Protocol::Vless, "edge.example", 443);
        node.credentials.uuid = Some("00000000-0000-4000-8000-000000000001".into());
        node.credentials.password = Some("pass".into());
        rules.node(&node);
        rules.subscription(&Subscription::new(
            "fixture",
            "https://feed.example/path?token=pass",
        ));
        rules.mesh_profile(&MeshProfile {
            peers: vec!["udp://203.0.113.8:11010".into()],
            network_secret: MeshSecret("fixture-network-key".into()),
            outbound_secret: MeshSecret("fixture-outbound-key".into()),
            ..Default::default()
        });
        rules.path(Path::new("/home/fixture"), "~");
        rules.path(Path::new("/home/fixture/App Data"), "$XROSE_HOME");
        rules.path(Path::new(r"C:\Users\Fixture"), "~");
        let r = rules.build().unwrap();
        assert_eq!(
            r.message("tcp:EDGE.EXAMPLE:443 sub.edge.example edge.example.invalid bypass pass"),
            "tcp:<node-server>:443 sub.edge.example edge.example.invalid bypass ***"
        );
        assert_eq!(
            r.message(
                "udp://203.0.113.8:11010 203.0.113.80:443 fixture-network-key fixture-outbound-key"
            ),
            "udp://<mesh-peer>:11010 203.0.*.*:443 *** ***"
        );
        assert_eq!(
            r.message("00000000-0000-4000-8000-000000000001 https://feed.example/path?token=pass"),
            "*** <subscription-url>"
        );
        assert_eq!(
            r.message("/home/fixture/App Data/logs /home/fixture/tmp /home/fixture2/n"),
            "$XROSE_HOME/logs ~/tmp /home/fixture2/n"
        );
        assert_eq!(
            r.message(r"C:\USERS\fixture\file C:/Users/Fixture/file C:\\Users\\Fixture\\file"),
            r"~\file ~/file ~\\file"
        );
    }

    #[test]
    fn keeps_route_and_transport_evidence_around_public_ips() {
        let mut rules = RedactionRules {
            fake_ip: Some("192.0.2.0/24".parse().unwrap()),
            ..Default::default()
        };
        rules.host("2001:db8:1::9", false);
        let r = rules.build().unwrap();
        let local = "127.0.0.1 [::1]:53 0.0.0.0/0 ::/0 10.1.2.3 172.31.0.1 192.168.1.5 169.254.1.2 100.127.0.1 198.18.1.2 198.19.255.254 192.0.2.9 fc00::2 fe80::1%eth0 [::ffff:192.168.1.1]:80";
        assert_eq!(r.message(local), local);
        assert_eq!(
            r.message("tcp 203.0.113.40:443 -> [2001:DB8:2::4]:8443 via 2001:db8::8/64"),
            "tcp 203.0.*.*:443 -> [2001:db8:•••]:8443 via 2001:db8:•••/64"
        );
        assert_eq!(
            r.message("peer=[2001:0DB8:0001:0:0:0:0:9]:45 mapped=[::ffff:203.0.113.4]:53"),
            "peer=[<node-server>]:45 mapped=[::ffff:203.0.*.*]:53"
        );
    }

    #[test]
    fn hides_complete_query_values_userinfo_and_encrypted_payloads() {
        let mut rules = RedactionRules::default();
        rules.secret("fixture");
        let r = rules.build().unwrap();
        assert_eq!(r.message("GET https://demo:p%40ss@host.example/p?ToKeN=fixture%2Ftail&route=tun&KEY=a+b/c==&PASSWORD=x#info"),
                   "GET https://***@host.example/p?ToKeN=***&route=tun&KEY=***&PASSWORD=***#info");
        assert_eq!(
            r.message("invalid enc:v1:YWJj+/12==, retry using token=example-token"),
            "invalid ***, retry using token=***"
        );
    }
}
