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
    last_error: Option<String>,
    due: Option<Instant>,
    cooldown_until: Option<Instant>,
    candidate_streak: Option<(String, u8)>,
}

#[derive(Clone, Debug, PartialEq)]
struct SwitchDecision {
    id: String,
    recover: bool,
    scores: Option<(f32, f32)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Search {
    Stay,
    Improve,
    Recover,
}

fn same_session(actual: &ProcessState, expected: &ProcessState) -> bool {
    actual.phase == ProcessPhase::Running
        && actual.pid == expected.pid
        && actual.started_at == expected.started_at
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

    fn observe_health_active(
        &mut self,
        group: &OutboundGroup,
        probe_ok: bool,
        live_active: bool,
        budget_exhausted: bool,
        now: Instant,
    ) -> Search {
        if !probe_ok {
            if live_active {
                self.candidate_streak = None;
                return Search::Stay;
            }
            self.failures = self.failures.saturating_add(1);
            if self.failures >= group.switch_policy.consecutive_failures.max(1) {
                return Search::Recover;
            }
            self.candidate_streak = None;
            return Search::Stay;
        }
        self.failures = 0;
        self.no_match_reported = false;
        if group.switch_policy.sticky_session
            || self.cooldown_until.is_some_and(|until| now < until)
            || budget_exhausted
        {
            self.candidate_streak = None;
            return Search::Stay;
        }
        Search::Improve
    }

    fn decide_health_switch(
        &mut self,
        search: Search,
        active_eval: Option<&crate::runtime::health::ProfileEvaluation>,
        candidates: &[(String, crate::runtime::health::ProfileEvaluation)],
    ) -> Option<SwitchDecision> {
        match search {
            Search::Stay => {
                self.candidate_streak = None;
                None
            }
            Search::Recover => {
                self.candidate_streak = None;
                let mut best: Option<&(String, crate::runtime::health::ProfileEvaluation)> = None;
                for cand in candidates {
                    if cand.1.gate.is_some() {
                        continue;
                    }
                    if best.is_none_or(|b| cand.1.score > b.1.score) {
                        best = Some(cand);
                    }
                }
                let best = best?;
                let active_score = active_eval.map(|e| e.score).unwrap_or(0.0);
                Some(SwitchDecision {
                    id: best.0.clone(),
                    recover: true,
                    scores: Some((active_score, best.1.score)),
                })
            }
            Search::Improve => {
                let active_short = active_eval.map(|e| e.short_score).unwrap_or(0.0);
                let mut best: Option<&(String, crate::runtime::health::ProfileEvaluation)> = None;
                for cand in candidates {
                    if cand.1.gate.is_some() || cand.1.confidence < 0.40 {
                        continue;
                    }
                    if cand.1.short_score - active_short < 12.0 {
                        continue;
                    }
                    if best.is_none_or(|b| cand.1.short_score > b.1.short_score) {
                        best = Some(cand);
                    }
                }
                let Some(best) = best else {
                    self.candidate_streak = None;
                    return None;
                };
                let count = match &self.candidate_streak {
                    Some((prev_id, prev_count)) if prev_id == &best.0 => {
                        prev_count.saturating_add(1)
                    }
                    _ => 1,
                };
                if count >= 3 {
                    self.candidate_streak = None;
                    Some(SwitchDecision {
                        id: best.0.clone(),
                        recover: false,
                        scores: Some((active_short, best.1.short_score)),
                    })
                } else {
                    self.candidate_streak = Some((best.0.clone(), count));
                    None
                }
            }
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
                    && g.members.iter().any(|id| {
                        model
                            .node(id)
                            .is_some_and(|n| model.node_usable_with(n, core))
                    })
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
        same_session(&actual, expected) && !self.is_busy("core") && !self.inner.reload.lock().dirty
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
                if service.probe_budget_exhausted() {
                    states.entry(group.id.clone()).or_default().due =
                        Instant::now().checked_add(Duration::from_secs(60));
                    continue;
                }
                if !service.try_begin_probe("auto-probe") {
                    continue;
                }
                let state = states.entry(group.id.clone()).or_default();
                // Dropping the round cancels its isolated probe when the user stops or applies.
                let result = tokio::select! {
                    res = service.measure_group(&service.inner.core, &process, &group, state, true) => res,
                    _ = service.wait_for_switch_cancel(&process) => Ok(None),
                };
                service.set_busy("auto-probe", false);
                match result {
                    Ok(Some(decision))
                        if service.switch_session_current(&process)
                            && service.inner.busy.write().insert("core".into()) =>
                    {
                        state.last_error = None;
                        let switched = service
                            .apply_switch(
                                &process,
                                &group,
                                &decision.id,
                                decision.recover,
                                decision.scores,
                            )
                            .await;
                        if switched {
                            state.failures = 0;
                            state.no_match_reported = false;
                            state.cooldown_until =
                                Instant::now().checked_add(Duration::from_secs(300));
                            state.candidate_streak = None;
                        }
                        // Rollback may also start a new PID. Wait a full interval before retrying.
                        let new = service.process_state();
                        session = Some((new.pid, new.started_at));
                        service.set_busy("core", false);
                    }
                    Err(err) => {
                        let msg = err.to_string();
                        if state.last_error.as_deref() != Some(msg.as_str()) {
                            service.notify(ToastLevel::Warning, msg.clone());
                            state.last_error = Some(msg);
                        }
                    }
                    Ok(_) => {
                        state.last_error = None;
                    }
                }
                state.due = Instant::now().checked_add(Duration::from_secs(
                    group.interval_secs.clamp(30, u32::MAX as u64),
                ));
            }
        });
    }

    /// The gateway owns this scheduler through its weak manager reference.
    /// Temporary stop/reload and recoverable failures do not remove scheduling.
    pub(super) fn start_exit_node_switch_scheduler(&self, core: Arc<CoreManager>) {
        let weak_service = Arc::downgrade(&self.inner);
        let weak_core = Arc::downgrade(&core);
        self.spawn(async move {
            let mut states = BTreeMap::<String, RoundState>::new();
            let mut session = None;
            loop {
                // Neither service nor manager is held alive between rounds.
                tokio::time::sleep(Duration::from_secs(2)).await;
                let (Some(inner), Some(core)) = (weak_service.upgrade(), weak_core.upgrade())
                else {
                    break;
                };
                let service = AppService { inner };
                let process = core.state();
                if !service.exit_switch_session_current(&core, &process) {
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
                let group = core
                    .managed_probe_groups()
                    .into_iter()
                    .filter(|g| g.members.len() > 1 && due(g).is_none_or(|t| t <= Instant::now()))
                    .min_by_key(due);
                let Some(group) = group else { continue };
                if service.probe_budget_exhausted() {
                    states.entry(group.id.clone()).or_default().due =
                        Instant::now().checked_add(Duration::from_secs(60));
                    continue;
                }
                if !service.try_begin_probe("auto-probe") {
                    continue;
                }
                let state = states.entry(group.id.clone()).or_default();
                let result = tokio::select! {
                    res = service.measure_group(&core, &process, &group, state, false) => res,
                    _ = async {
                        while service.exit_switch_session_current(&core, &process) {
                            tokio::time::sleep(Duration::from_millis(500)).await;
                        }
                    } => Ok(None),
                };
                service.set_busy("auto-probe", false);
                match result {
                    Ok(Some(decision)) if service.exit_switch_session_current(&core, &process) => {
                        state.last_error = None;
                        // The manager locks its operation and rechecks the process
                        // epoch and applied group before replacing its configuration.
                        match core
                            .switch_managed_node(&process, &group, &decision.id)
                            .await
                        {
                            Ok(true) => {
                                state.failures = 0;
                                state.no_match_reported = false;
                                state.cooldown_until =
                                    Instant::now().checked_add(Duration::from_secs(300));
                                state.candidate_streak = None;
                                let delta = decision
                                    .scores
                                    .map(|(old, new)| {
                                        format!(
                                            "（{} → {}）",
                                            old.round() as u32,
                                            new.round() as u32
                                        )
                                    })
                                    .unwrap_or_default();
                                if decision.recover {
                                    service.notify(
                                        ToastLevel::Success,
                                        format!("出口分流已切换到备用节点{delta}"),
                                    );
                                } else {
                                    let message = if !delta.is_empty() {
                                        format!("出口分流已按健康评分切换节点{delta}")
                                    } else {
                                        "出口分流已按性能优先策略切换节点".into()
                                    };
                                    service.inner.core.logs().push(
                                        LogStream::App,
                                        LogLevel::Info,
                                        message,
                                    );
                                }
                            }
                            Ok(false) => {}
                            Err(error) => service.notify(
                                ToastLevel::Error,
                                format!("出口分流自动切换失败：{error}"),
                            ),
                        }
                        let current = core.state();
                        session = Some((current.pid, current.started_at));
                    }
                    Err(error) => {
                        let msg = format!("出口分流检查失败：{error}");
                        if state.last_error.as_deref() != Some(msg.as_str()) {
                            service.notify(ToastLevel::Warning, msg.clone());
                            state.last_error = Some(msg);
                        }
                    }
                    Ok(_) => {
                        state.last_error = None;
                    }
                }
                state.due = Instant::now().checked_add(Duration::from_secs(
                    group.interval_secs.clamp(30, u32::MAX as u64),
                ));
            }
        });
    }

    fn exit_switch_session_current(&self, core: &CoreManager, expected: &ProcessState) -> bool {
        same_session(&core.state(), expected)
            && !self.is_busy("core")
            && !self.is_busy("exit-policy")
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
        scores: Option<(f32, f32)>,
    ) -> bool {
        self.inner.core.logs().push(
            LogStream::App,
            LogLevel::Info,
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
                *self.inner.proxies.write() = Default::default();
                let delta = scores
                    .map(|(old, new)| {
                        format!("（{} → {}）", old.round() as u32, new.round() as u32)
                    })
                    .unwrap_or_default();
                if recover {
                    self.notify(ToastLevel::Success, format!("已切换到备用节点{delta}"));
                } else {
                    let message = if !delta.is_empty() {
                        format!("已按健康评分切换节点{delta}")
                    } else {
                        "已按性能优先策略切换节点".into()
                    };
                    self.inner
                        .core
                        .logs()
                        .push(LogStream::App, LogLevel::Info, message);
                }
                true
            }
            Ok(false) => false,
            Err(err) => {
                self.notify(ToastLevel::Error, format!("自动切换失败：{err}"));
                false
            }
        }
    }

    async fn measure_group(
        &self,
        manager: &CoreManager,
        process: &ProcessState,
        group: &OutboundGroup,
        state: &mut RoundState,
        report_main: bool,
    ) -> Result<Option<SwitchDecision>> {
        let (Some(core), Some(binary), Some((model, settings, _))) = (
            process.core,
            process.binary.as_deref(),
            manager
                .statistics_context()
                .filter(|(model, _, _)| model.work_mode == crate::model::WorkMode::Proxy),
        ) else {
            return Ok(None);
        };
        let Some(active) = group.active_node().and_then(|id| model.node(id)) else {
            return Ok(None);
        };
        let url = group.url.as_deref().unwrap_or(&settings.urltest_url);
        let bypass_tun = self.inner.core.native_tun_active();
        let mode = group.switch_mode();
        let latency_mode = self.settings().http_latency_mode;
        let probe = GroupProbe {
            service: self,
            model: &model,
            binary: Path::new(binary),
            core,
            url,
            bypass_tun,
            mode,
            latency_mode,
        };
        let target = report_main.then(|| self.tray_probe_target()).flatten();
        let current = probe.measure(active).await;
        if report_main {
            self.record_proxy_probe(target.as_ref(), &active.id, current.is_some());
        }
        let is_health = mode == SwitchMode::Health;
        let active_eval = is_health
            .then(|| self.evaluate_node_health(&active.id, group.switch_policy.health_profile))
            .flatten();
        let search = if is_health {
            let live_active = self
                .inner
                .health_views
                .read()
                .get(&active.id)
                .is_some_and(|v| {
                    v.last_active_at > 0 && now_unix().saturating_sub(v.last_active_at) <= 30
                });
            state.observe_health_active(
                group,
                current.is_some(),
                live_active,
                self.probe_budget_exhausted(),
                Instant::now(),
            )
        } else {
            state.observe(group, current.is_some())
        };
        if search == Search::Stay {
            return Ok(None);
        }
        let scope = if search == Search::Recover {
            group.switch_policy.fallback
        } else {
            FallbackScope::Group
        };
        let country = if scope == FallbackScope::SameRegion {
            self.switch_country(&model, active, settings.language).await
        } else {
            None
        };
        // Keep ordering deterministic. Latency can probe 4 at once; health mode limits to 2.
        let concurrency = match mode {
            SwitchMode::Latency => 4,
            SwitchMode::Health => 2,
            _ => 1,
        };
        let tasks: Vec<_> = group
            .members
            .iter()
            .filter(|id| **id != active.id)
            .filter_map(|id| model.node(id))
            .map(|node| {
                let probe = &probe;
                let model = &model;
                let country = country.as_deref();
                async move {
                    let other_country = if scope == FallbackScope::SameRegion && country.is_some() {
                        self.switch_country(model, node, settings.language).await
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
                if mode == SwitchMode::Failover {
                    break;
                }
            }
        }
        let decision = if is_health {
            let profile = group.switch_policy.health_profile;
            let cand_evals: Vec<_> = samples
                .iter()
                .filter_map(|(id, _)| {
                    self.evaluate_node_health(id, profile)
                        .map(|eval| (id.clone(), eval))
                })
                .collect();
            state.decide_health_switch(search, active_eval.as_ref(), &cand_evals)
        } else {
            winner(group, current, &samples).map(|id| SwitchDecision {
                id: id.to_owned(),
                recover: search == Search::Recover,
                scores: None,
            })
        };
        if decision.is_none() && search == Search::Recover && !state.no_match_reported {
            self.notify(
                ToastLevel::Warning,
                if report_main {
                    "回退范围内没有可用节点，保留当前节点并在下轮重试"
                } else {
                    "出口分流的回退范围内没有可用节点，保留当前节点并在下轮重试"
                },
            );
            state.no_match_reported = true;
        }
        Ok(decision)
    }

    async fn switch_country(
        &self,
        model: &AppModel,
        node: &Node,
        language: crate::settings::Language,
    ) -> Option<String> {
        let server = model
            .chain_exit_node(node)
            .map(|n| n.server.as_str())
            .unwrap_or(&node.server);
        if server.trim().is_empty() {
            return None;
        }
        let key = location::cache_key(server, language);
        if let Some(entry) = self.inner.node_locations.read().get(&key) {
            if !entry.needs_refresh(now_unix()) {
                return entry.country.clone();
            }
        }
        let location = location::lookup(server, language).await;
        let country = location.country.clone();
        self.inner.node_locations.write().insert(key, location);
        country
    }
}

struct GroupProbe<'a> {
    service: &'a AppService,
    model: &'a AppModel,
    binary: &'a Path,
    core: CoreKind,
    url: &'a str,
    bypass_tun: bool,
    mode: SwitchMode,
    latency_mode: crate::settings::HttpLatencyMode,
}

impl GroupProbe<'_> {
    async fn measure(&self, node: &Node) -> Option<u64> {
        let route = self
            .model
            .resolve_probe_hops(node, self.core)
            .map(probe::ProbeRoute::Hops);
        if self.mode == SwitchMode::Throughput {
            let result = match route {
                Ok(route) => {
                    probe::download_route_sample(
                        self.binary,
                        self.core,
                        &route,
                        self.bypass_tun,
                        true,
                    )
                    .await
                }
                Err(err) => Err(err),
            };
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
            let err_stage = entry
                .error
                .as_deref()
                .map(crate::runtime::health::classify_error)
                .unwrap_or(crate::runtime::health::ErrStage::None);
            self.service
                .record_health_sample(crate::runtime::health::ProbeSampleRecord {
                    target_key: node.id.clone(),
                    kind: crate::runtime::health::ProbeKind::Download,
                    ts: entry.checked_at,
                    ok: entry.error.is_none() && entry.bytes_per_second > 0,
                    rtt_ms: None,
                    bps: speed,
                    bytes_used: entry.bytes,
                    err_stage,
                });
            self.service
                .inner
                .downloads
                .write()
                .insert(node.id.clone(), entry);
            speed
        } else {
            let result = match route {
                Ok(route) => {
                    probe::http_route_latency(
                        self.binary,
                        self.core,
                        &route,
                        self.url,
                        self.bypass_tun,
                        self.latency_mode,
                    )
                    .await
                }
                Err(err) => Err(err),
            };
            let ok = result.is_ok();
            let ms = result.as_ref().ok().copied();
            let checked_at = now_unix();
            let detail = result
                .err()
                .map(|e| e.to_string())
                .unwrap_or_else(|| self.latency_mode.detail_label().into());
            let err_stage = if ok {
                crate::runtime::health::ErrStage::None
            } else {
                crate::runtime::health::classify_error(&detail)
            };
            self.service
                .record_health_sample(crate::runtime::health::ProbeSampleRecord {
                    target_key: node.id.clone(),
                    kind: crate::runtime::health::ProbeKind::Http,
                    ts: checked_at,
                    ok,
                    rtt_ms: ms,
                    bps: None,
                    bytes_used: if ok {
                        self.latency_mode.estimated_bytes()
                    } else {
                        0
                    },
                    err_stage,
                });
            let entry = LatencyEntry {
                key: node.id.clone(),
                delay_ms: ms,
                checked_at,
                detail: Some(detail),
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
    use crate::model::{GroupKind, HealthProfile, Protocol};
    use crate::runtime::health::{HealthGate, HealthGrade, ProfileEvaluation};

    fn eval(
        score: f32,
        short_score: f32,
        confidence: f32,
        gate: Option<HealthGate>,
    ) -> ProfileEvaluation {
        ProfileEvaluation {
            profile: HealthProfile::General,
            score,
            short_score,
            long_score: score,
            grade: HealthGrade::from_score(score),
            gate,
            confidence,
            dims: Vec::new(),
            deductions: Vec::new(),
        }
    }

    #[test]
    fn switch_rounds_only_accept_results_from_the_same_running_process() {
        let expected = ProcessState {
            phase: ProcessPhase::Running,
            pid: Some(41),
            started_at: Some(100),
            ..Default::default()
        };
        assert!(same_session(&expected, &expected));
        for actual in [
            ProcessState {
                phase: ProcessPhase::Stopped,
                ..expected.clone()
            },
            ProcessState {
                phase: ProcessPhase::Failed,
                ..expected.clone()
            },
            ProcessState {
                pid: Some(42),
                ..expected.clone()
            },
            ProcessState {
                started_at: Some(101),
                ..expected.clone()
            },
        ] {
            assert!(!same_session(&actual, &expected));
        }
    }

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

    #[test]
    fn health_hysteresis_requires_twelve_point_lead_for_three_rounds_and_low_conf_filtered() {
        let mut group = OutboundGroup::new("健康切换", GroupKind::Selector);
        group.switch_policy.mode = SwitchMode::Health;
        let now = Instant::now();
        let mut state = RoundState::default();
        let active = eval(76.0, 76.0, 0.9, None);

        // Low-confidence or +11.9 lead is rejected and resets streak.
        let low_conf = vec![("cand".into(), eval(95.0, 95.0, 0.35, None))];
        let small_lead = vec![("cand".into(), eval(87.9, 87.9, 0.9, None))];
        let strong = vec![("cand".into(), eval(89.0, 89.0, 0.8, None))];

        assert_eq!(
            state.observe_health_active(&group, true, false, false, now),
            Search::Improve
        );
        assert!(
            state
                .decide_health_switch(Search::Improve, Some(&active), &strong)
                .is_none()
        );
        assert!(
            state
                .decide_health_switch(Search::Improve, Some(&active), &strong)
                .is_none()
        );
        // Interrupted by < +12 lead resets the streak back to 0.
        assert!(
            state
                .decide_health_switch(Search::Improve, Some(&active), &small_lead)
                .is_none()
        );
        assert!(
            state
                .decide_health_switch(Search::Improve, Some(&active), &low_conf)
                .is_none()
        );

        assert!(
            state
                .decide_health_switch(Search::Improve, Some(&active), &strong)
                .is_none()
        );
        assert!(
            state
                .decide_health_switch(Search::Improve, Some(&active), &strong)
                .is_none()
        );
        let decision = state
            .decide_health_switch(Search::Improve, Some(&active), &strong)
            .expect("third consecutive qualifying round triggers switch");
        assert_eq!(decision.id, "cand");
        assert!(!decision.recover);
        assert_eq!(decision.scores, Some((76.0, 89.0)));
    }

    #[test]
    fn health_sticky_and_cooldown_protect_working_nodes_and_recover_only_on_failure() {
        let mut group = OutboundGroup::new("AI 粘性", GroupKind::Selector);
        group.switch_policy.mode = SwitchMode::Health;
        group.switch_policy.health_profile = HealthProfile::Ai;
        group.switch_policy.sticky_session = true;
        group.switch_policy.consecutive_failures = 2;

        let now = Instant::now();
        let mut state = RoundState::default();
        assert_eq!(
            state.observe_health_active(&group, true, false, false, now),
            Search::Stay
        );

        // Even with cooldown active, a reachable node or a node carrying live download traffic
        // is never force-switched.
        state.cooldown_until = Some(now + Duration::from_secs(300));
        assert_eq!(
            state.observe_health_active(&group, true, false, false, now),
            Search::Stay
        );
        assert_eq!(
            state.observe_health_active(&group, false, true, false, now),
            Search::Stay
        );
        assert_eq!(
            state.observe_health_active(&group, false, true, false, now),
            Search::Stay
        );

        // Consecutive connectivity failures without live traffic trigger failover despite
        // sticky_session and cooldown, preferring a non-gated backup node.
        assert_eq!(
            state.observe_health_active(&group, false, false, false, now),
            Search::Stay
        );
        assert_eq!(
            state.observe_health_active(&group, false, false, false, now),
            Search::Recover
        );
        let active_gated = eval(25.0, 25.0, 0.8, Some(HealthGate::Down));
        let candidates = vec![
            (
                "restricted".into(),
                eval(40.0, 40.0, 0.9, Some(HealthGate::ServiceRestricted)),
            ),
            ("clean".into(), eval(82.0, 82.0, 0.5, None)),
        ];
        let recover = state
            .decide_health_switch(Search::Recover, Some(&active_gated), &candidates)
            .expect("connectivity failure recovers to non-gated node");
        assert_eq!(recover.id, "clean");
        assert!(recover.recover);

        // Without sticky_session, cooldown still blocks proactive improvement until expiry.
        group.switch_policy.sticky_session = false;
        assert_eq!(
            state.observe_health_active(&group, true, false, false, now),
            Search::Stay
        );
        assert_eq!(
            state.observe_health_active(&group, true, false, false, now + Duration::from_secs(301)),
            Search::Improve
        );
    }
}
