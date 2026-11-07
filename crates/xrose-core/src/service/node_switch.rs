//! Policy decisions use only this round's isolated measurements and applied group members.

use super::*;
use crate::core::ProcessPhase;
use crate::model::{FallbackScope, NodeSwitchPolicy, OutboundGroup, SwitchMode};
use crate::runtime::probe;
use futures_util::{StreamExt, stream};
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Default)]
struct RoundState {
    failures: u8,
    no_match_reported: bool,
    due: Option<Instant>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Search {
    Stay,
    Improve,
    Recover,
}

impl RoundState {
    fn observe(&mut self, group: &OutboundGroup, healthy: bool) -> Search {
        if healthy {
            self.failures = 0;
            self.no_match_reported = false;
            return match group.switch_mode() {
                SwitchMode::Latency | SwitchMode::Throughput => Search::Improve,
                _ => Search::Stay,
            };
        }
        self.failures = self.failures.saturating_add(1);
        if self.failures >= group.switch_policy.consecutive_failures.max(1) {
            Search::Recover
        } else {
            Search::Stay
        }
    }
}

/// Equal results retain the earlier member (or the current node).
fn winner<'a>(
    group: &OutboundGroup,
    current: Option<u64>,
    samples: &'a [(String, u64)],
) -> Option<&'a str> {
    let mode = group.switch_mode();
    let mut best: Option<&(String, u64)> = None;
    for sample in samples {
        if best.is_none_or(|(_, value)| match mode {
            SwitchMode::Latency => sample.1 < *value,
            SwitchMode::Throughput => sample.1 > *value,
            _ => false,
        }) {
            best = Some(sample);
        }
    }
    let (id, score) = best?;
    let better = current.is_none_or(|old| match mode {
        SwitchMode::Latency => old.saturating_sub(*score) >= u64::from(group.tolerance_ms.max(1)),
        SwitchMode::Throughput => *score > old && *score - old >= old.div_ceil(10),
        _ => false,
    });
    better.then_some(id)
}

fn in_scope(
    scope: FallbackScope,
    source: &Node,
    candidate: &Node,
    country: Option<&str>,
    other_country: Option<&str>,
) -> bool {
    let matches =
        |a: Option<&str>, b: Option<&str>| a.is_some_and(|v| !v.trim().is_empty() && Some(v) == b);
    match scope {
        FallbackScope::Group => true,
        FallbackScope::SameSubscription => matches(
            source
                .subscription_id
                .as_deref()
                .filter(|id| *id != PASTED_SOURCE),
            candidate.subscription_id.as_deref(),
        ),
        FallbackScope::SameRegion => matches(country, other_country),
    }
}

impl AppService {
    pub fn set_default_outbound_group(&self, group_id: &str) -> Result<()> {
        if self.is_busy("core") {
            return Err(Error::other("内核忙，请稍后重试"));
        }
        let core = self.inner.settings.read().core;
        self.commit_model_change(|model| {
            if !model.group(group_id).is_some_and(|g| {
                g.enabled
                    && g.members
                        .iter()
                        .any(|id| model.node(id).is_some_and(|n| n.usable_with(core)))
            }) {
                return Err(Error::config("请选择已启用且有可用节点的出站组"));
            }
            if model.default_group.as_deref() == Some(group_id) {
                return Ok(false);
            }
            model.default_group = Some(group_id.to_owned());
            model.updated_at = now_unix();
            Ok(true)
        })?;
        Ok(())
    }

    pub fn set_node_switch_policy(
        &self,
        group_id: &str,
        policy: NodeSwitchPolicy,
        interval_secs: u64,
    ) -> Result<()> {
        if self.is_busy("core") {
            return Err(Error::other("内核忙，请稍后重试"));
        }
        if !(1..=10).contains(&policy.consecutive_failures) || interval_secs < 30 {
            return Err(Error::config(
                "连续失败次数须为 1 至 10 次，检查间隔至少为 30 秒",
            ));
        }
        self.commit_model_change(|model| {
            let group = model
                .group_mut(group_id)
                .ok_or_else(|| Error::config("出站组不存在"))?;
            if group.switch_policy == policy && group.interval_secs == interval_secs {
                return Ok(false);
            }
            group.switch_policy = policy;
            group.interval_secs = interval_secs;
            model.updated_at = now_unix();
            Ok(true)
        })?;
        Ok(())
    }

    fn switch_session_current(&self, expected: &ProcessState) -> bool {
        let actual = self.process_state();
        actual.phase == ProcessPhase::Running
            && actual.pid == expected.pid
            && actual.started_at == expected.started_at
            && !self.is_busy("core")
            && !self.inner.reload.lock().dirty
    }

    pub(super) fn start_node_switch_scheduler(&self) {
        let weak = Arc::downgrade(&self.inner);
        self.spawn(async move {
            let mut states = BTreeMap::<String, RoundState>::new();
            let mut session = None;
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let Some(inner) = weak.upgrade() else { break };
                let service = AppService { inner };
                let process = service.process_state();
                if !service.switch_session_current(&process) {
                    states.clear();
                    session = None;
                    continue;
                }
                let key = (process.pid, process.started_at);
                if session != Some(key) {
                    states.clear();
                    session = Some(key);
                }
                let due = |g: &OutboundGroup| states.get(&g.id).and_then(|s| s.due);
                let group = service
                    .inner
                    .core
                    .managed_probe_groups()
                    .into_iter()
                    .filter(|g| g.members.len() > 1 && due(g).is_none_or(|t| t <= Instant::now()))
                    .min_by_key(due);
                let Some(group) = group else { continue };
                if !service.try_begin_probe("auto-probe") {
                    continue;
                }
                let state = states.entry(group.id.clone()).or_default();
                // Dropping the round cancels its isolated probe when the user stops or applies.
                let result = tokio::select! {
                    res = service.measure_group(&process, &group, state) => res,
                    _ = service.wait_for_switch_cancel(&process) => Ok(None),
                };
                service.set_busy("auto-probe", false);
                match result {
                    Ok(Some((id, recover)))
                        if service.switch_session_current(&process)
                            && service.inner.busy.write().insert("core".into()) =>
                    {
                        let switched = service.apply_switch(&process, &group, &id, recover).await;
                        if switched {
                            state.failures = 0;
                            state.no_match_reported = false;
                        }
                        // Rollback may also start a new PID. Wait a full interval before retrying.
                        let new = service.process_state();
                        session = Some((new.pid, new.started_at));
                        service.set_busy("core", false);
                    }
                    Err(err) => service.toast(ToastLevel::Warning, err.to_string()),
                    _ => (),
                }
                state.due = Instant::now().checked_add(Duration::from_secs(
                    group.interval_secs.clamp(30, u32::MAX as u64),
                ));
            }
        });
    }

    async fn wait_for_switch_cancel(&self, process: &ProcessState) {
        while self.switch_session_current(process) {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    async fn apply_switch(
        &self,
        process: &ProcessState,
        group: &OutboundGroup,
        id: &str,
        recover: bool,
    ) -> bool {
        self.toast(
            ToastLevel::Info,
            if recover {
                "当前节点不可用，正在切换备用节点并重载内核"
            } else {
                "发现性能更好的节点，正在切换并重载内核"
            },
        );
        match self
            .inner
            .core
            .switch_managed_node(process, group, id)
            .await
        {
            Ok(true) => {
                self.inner.traffic.lock().reset();
                self.inner.connections.write().clear();
                self.inner.proxies.write().clear();
                self.toast(
                    ToastLevel::Success,
                    if recover {
                        "已切换到备用节点"
                    } else {
                        "已按性能优先策略切换节点"
                    },
                );
                true
            }
            Ok(false) => false,
            Err(err) => {
                self.toast(ToastLevel::Error, format!("自动切换失败：{err}"));
                false
            }
        }
    }

    async fn measure_group(
        &self,
        process: &ProcessState,
        group: &OutboundGroup,
        state: &mut RoundState,
    ) -> Result<Option<(String, bool)>> {
        let (Some(core), Some(binary), Some(model)) = (
            process.core,
            process.binary.as_deref(),
            self.inner.core.managed_probe_model(),
        ) else {
            return Ok(None);
        };
        let Some(active) = group.active_node().and_then(|id| model.node(id)) else {
            return Ok(None);
        };
        let front = model.front_node(core)?;
        let settings = self.settings();
        let url = group.url.as_deref().unwrap_or(&settings.urltest_url);
        let bypass_tun = self.inner.core.native_tun_active();
        let probe = GroupProbe {
            service: self,
            binary: Path::new(binary),
            core,
            front,
            url,
            bypass_tun,
            mode: group.switch_mode(),
        };
        let current = probe.measure(active).await;
        let search = state.observe(group, current.is_some());
        if search == Search::Stay {
            return Ok(None);
        }
        let scope = if search == Search::Recover {
            group.switch_policy.fallback
        } else {
            FallbackScope::Group
        };
        let country = if scope == FallbackScope::SameRegion {
            self.switch_country(active, settings.language).await
        } else {
            None
        };
        // Keep ordering deterministic. Only latency comparisons can share the uplink.
        let concurrency = if group.switch_mode() == SwitchMode::Latency {
            4
        } else {
            1
        };
        let tasks: Vec<_> = group
            .members
            .iter()
            .filter(|id| **id != active.id)
            .filter_map(|id| model.node(id))
            .map(|node| {
                let probe = &probe;
                let country = country.as_deref();
                async move {
                    let other_country = if scope == FallbackScope::SameRegion && country.is_some() {
                        self.switch_country(node, settings.language).await
                    } else {
                        None
                    };
                    if !in_scope(scope, active, node, country, other_country.as_deref()) {
                        return None;
                    }
                    Some((node.id.clone(), probe.measure(node).await?))
                }
            })
            .collect();
        let mut measurements = stream::iter(tasks).buffered(concurrency);
        let mut samples = Vec::new();
        while let Some(measurement) = measurements.next().await {
            if let Some(sample) = measurement {
                samples.push(sample);
                if group.switch_mode() == SwitchMode::Failover {
                    break;
                }
            }
        }
        let best = winner(group, current, &samples);
        if best.is_none() && search == Search::Recover && !state.no_match_reported {
            self.toast(
                ToastLevel::Warning,
                "回退范围内没有可用节点，保留当前节点并在下轮重试",
            );
            state.no_match_reported = true;
        }
        Ok(best.map(|id| (id.to_owned(), search == Search::Recover)))
    }

    async fn switch_country(
        &self,
        node: &Node,
        language: crate::settings::Language,
    ) -> Option<String> {
        let key = location::cache_key(&node.server, language);
        if let Some(entry) = self.inner.node_locations.read().get(&key) {
            if !entry.needs_refresh(now_unix()) {
                return entry.country.clone();
            }
        }
        let location = location::lookup(&node.server, language).await;
        let country = location.country.clone();
        self.inner.node_locations.write().insert(key, location);
        country
    }
}

struct GroupProbe<'a> {
    service: &'a AppService,
    binary: &'a Path,
    core: CoreKind,
    front: Option<&'a Node>,
    url: &'a str,
    bypass_tun: bool,
    mode: SwitchMode,
}

impl GroupProbe<'_> {
    async fn measure(&self, node: &Node) -> Option<u64> {
        if self.mode == SwitchMode::Throughput {
            let result = probe::download_sample(
                self.binary,
                self.core,
                node,
                self.front,
                self.bypass_tun,
                true,
            )
            .await;
            let speed = result
                .as_ref()
                .ok()
                .map(|d| d.bytes_per_second)
                .filter(|s| *s > 0);
            let entry = result.unwrap_or_else(|e| probe::DownloadResult {
                bytes: 0,
                elapsed_ms: 0,
                bytes_per_second: 0,
                checked_at: now_unix(),
                error: Some(e.to_string()),
            });
            self.service
                .inner
                .downloads
                .write()
                .insert(node.id.clone(), entry);
            speed
        } else {
            let result = probe::http_latency(
                self.binary,
                self.core,
                node,
                self.url,
                self.front,
                self.bypass_tun,
            )
            .await;
            let ms = result.as_ref().ok().copied();
            let entry = LatencyEntry {
                key: node.id.clone(),
                delay_ms: ms,
                checked_at: now_unix(),
                detail: Some(
                    result
                        .err()
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "HTTP 延迟".into()),
                ),
            };
            self.service
                .inner
                .latency
                .write()
                .insert(node.id.clone(), entry);
            ms.map(u64::from)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GroupKind, Protocol};

    #[test]
    fn failover_waits_for_consecutive_failures_and_keeps_member_order() {
        let group = OutboundGroup::new("测试", GroupKind::Fallback);
        let mut state = RoundState::default();
        for (healthy, expected) in [
            (false, Search::Stay),
            (true, Search::Stay),
            (false, Search::Stay),
            (false, Search::Recover),
        ] {
            assert_eq!(state.observe(&group, healthy), expected);
        }
        let samples = [("first".into(), 240), ("fastest".into(), 85)];
        assert_eq!(winner(&group, None, &samples), Some("first"));
        assert_eq!(winner(&group, Some(700), &samples), None);
        assert_eq!(winner(&group, None, &[]), None);
    }

    #[test]
    fn performance_switch_uses_correct_direction_margin_and_stable_ties() {
        let mut group = OutboundGroup::new("测试", GroupKind::Selector);
        group.switch_policy.mode = SwitchMode::Latency;
        group.tolerance_ms = 50;
        let samples = [
            ("first".into(), 100),
            ("second".into(), 100),
            ("third".into(), 200),
        ];
        assert_eq!(winner(&group, Some(149), &samples), None);
        assert_eq!(winner(&group, Some(150), &samples), Some("first"));
        group.tolerance_ms = 0;
        assert_eq!(winner(&group, Some(100), &samples), None);
        group.switch_policy.mode = SwitchMode::Throughput;
        assert_eq!(winner(&group, Some(182), &samples), None);
        assert_eq!(winner(&group, Some(180), &samples), Some("third"));
        assert_eq!(winner(&group, None, &samples), Some("third"));
        assert_eq!(winner(&group, Some(u64::MAX), &samples), None);
    }

    #[test]
    fn fallback_scope_requires_positive_membership_or_known_matching_country() {
        let mut a = Node::new("A", Protocol::Http, "a.example", 80);
        let mut b = Node::new("B", Protocol::Http, "b.example", 80);
        let sub = FallbackScope::SameSubscription;
        let region = FallbackScope::SameRegion;
        assert!(!in_scope(sub, &a, &b, None, None));
        a.subscription_id = Some(PASTED_SOURCE.into());
        b.subscription_id = a.subscription_id.clone();
        assert!(!in_scope(sub, &a, &b, None, None));
        a.subscription_id = Some("one".into());
        b.subscription_id = Some("two".into());
        assert!(!in_scope(sub, &a, &b, Some("Japan"), Some("Japan")));
        b.subscription_id = a.subscription_id.clone();
        assert!(in_scope(sub, &a, &b, None, None));
        assert!(!in_scope(region, &a, &b, None, None));
        assert!(!in_scope(region, &a, &b, Some("Japan"), None));
        assert!(!in_scope(region, &a, &b, Some("Japan"), Some("Singapore")));
        assert!(in_scope(region, &a, &b, Some("Japan"), Some("Japan")));
    }
}
