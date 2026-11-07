//! User-initiated UDP reachability and NAT classification, independent of the running core.
use std::net::IpAddr;
use std::time::Duration;

use easytier::common::stun::{StunInfoCollector, UdpNatTypeDetector};
use serde::{Deserialize, Serialize};

use crate::settings::Language;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NatProbe {
    pub responding_servers: usize,
    pub classification: String,
    pub mapped_addresses: Vec<IpAddr>,
}

impl NatProbe {
    pub(crate) fn visible(&self, private: bool) -> Self {
        let mut result = self.clone();
        if private {
            result.mapped_addresses.clear();
        }
        result
    }

    pub fn summary(&self, lang: Language) -> String {
        if self.responding_servers == 0 {
            return match lang {
                Language::Zh => "未收到 STUN 响应，请检查 UDP 连通性后重试",
                Language::En => "No STUN reply. Check UDP connectivity and try again.",
            }
            .into();
        }
        let mut summary = match lang {
            Language::Zh => format!(
                "UDP 可达 · {} 个 STUN 服务响应 · NAT {}",
                self.responding_servers, self.classification
            ),
            Language::En => format!(
                "UDP reachable · {} STUN servers replied · NAT {}",
                self.responding_servers, self.classification
            ),
        };
        for address in &self.mapped_addresses {
            summary.push_str(&format!(" · {address}"));
        }
        summary
    }
}

pub async fn probe() -> NatProbe {
    let detector = UdpNatTypeDetector::new(StunInfoCollector::get_default_servers(), 1);
    match tokio::time::timeout(Duration::from_secs(8), detector.detect_nat_type(0)).await {
        Ok(Ok(result)) => NatProbe {
            classification: format!("{:?}", result.nat_type()),
            responding_servers: result.collect_available_stun_server().len(),
            mapped_addresses: result.public_ips(),
        },
        _ => NatProbe::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_summary_hides_observed_addresses() {
        let probe = NatProbe {
            responding_servers: 2,
            classification: "FullCone".into(),
            mapped_addresses: vec!["192.0.2.3".parse().unwrap(), "2001:db8::7".parse().unwrap()],
        };
        let private = probe.visible(true).summary(Language::Zh);
        assert!(private.contains("FullCone"));
        for ip in probe.mapped_addresses.iter() {
            assert!(!private.contains(&ip.to_string()));
            assert!(
                probe
                    .visible(false)
                    .summary(Language::En)
                    .contains(&ip.to_string())
            );
        }
    }
}
