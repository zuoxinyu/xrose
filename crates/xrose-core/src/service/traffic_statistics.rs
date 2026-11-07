//! Serialized statistics ingestion, persistence and asynchronous query publication.

use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use parking_lot::{Mutex, RwLock};

use super::{AppService, ToastLevel};
use crate::model::CoreKind;
use crate::runtime::{ClashClient, XrayStatsClient};
use crate::traffic_statistics::{
    self as stats, CoreEpoch, TrafficAccumulator, TrafficQuery, TrafficStatisticsView,
};

pub(super) struct StatisticsState {
    operation: tokio::sync::Mutex<()>,
    data: Mutex<TrafficAccumulator>,
    pub view: RwLock<Arc<TrafficStatisticsView>>,
    generation: AtomicU64,
    query_generation: AtomicU64,
    last_flush: Mutex<std::time::Instant>,
}
impl StatisticsState {
    pub fn new(enabled: bool) -> Self {
        Self {
            operation: tokio::sync::Mutex::new(()),
            data: Mutex::new(TrafficAccumulator::new(enabled)),
            view: RwLock::new(Arc::new(TrafficStatisticsView::default())),
            generation: AtomicU64::new(0),
            query_generation: AtomicU64::new(0),
            last_flush: Mutex::new(std::time::Instant::now()),
        }
    }
}

impl AppService {
    /// Called after settings have been saved, including general settings updates.
    pub(super) fn statistics_setting_changed(&self, enabled: bool) {
        self.inner
            .statistics
            .generation
            .fetch_add(1, Ordering::SeqCst);
        self.inner
            .statistics
            .data
            .lock()
            .rebase(enabled, CoreEpoch::from_state(&self.inner.core.state()));
    }

    pub fn set_traffic_statistics_enabled(&self, enabled: bool) {
        if self.is_busy("traffic-setting") {
            return;
        }
        self.set_busy("traffic-setting", true);
        let service = self.clone();
        self.spawn(async move {
            let _guard = service.inner.statistics.operation.lock().await;
            let save = service.clone();
            let result = tokio::task::spawn_blocking(move || {
                let mut settings = save.settings();
                settings.traffic_statistics_enabled = enabled;
                save.save_settings(&settings)
            })
            .await;
            match result {
                Ok(Ok(())) => {
                    service.flush_statistics_locked().await;
                }
                _ => service.toast(
                    ToastLevel::Error,
                    "统计开关保存失败，请检查配置目录权限后重试",
                ),
            }
            service.set_busy("traffic-setting", false);
            drop(_guard);
            let query = service.inner.statistics.view.read().query.clone();
            service.query_traffic_statistics(query);
        });
    }

    pub fn query_traffic_statistics(&self, query: TrafficQuery) {
        let generation = self
            .inner
            .statistics
            .query_generation
            .fetch_add(1, Ordering::SeqCst)
            + 1;
        {
            let mut view = self.inner.statistics.view.write();
            let current = Arc::make_mut(&mut view);
            if current.query != query {
                *current = TrafficStatisticsView {
                    query: query.clone(),
                    ..Default::default()
                };
            }
            // Background refreshes retain the current layout and rows.
            current.loading = current.range_start.is_none();
            current.query_error = None;
        }
        self.set_busy("traffic-query", true);
        let service = self.clone();
        self.spawn(async move {
            let _guard = service.inner.statistics.operation.lock().await;
            if generation
                != service
                    .inner
                    .statistics
                    .query_generation
                    .load(Ordering::SeqCst)
            {
                return;
            }
            let (session, pending, save_error, collection_error) = {
                let data = service.inner.statistics.data.lock();
                (
                    data.session.clone(),
                    data.buckets.values().cloned().collect::<Vec<_>>(),
                    data.save_error.clone(),
                    data.collection_error,
                )
            };
            let store = service.inner.store.clone();
            let result = tokio::task::spawn_blocking(move || {
                let today = stats::today();
                let mut buckets: BTreeMap<_, _> = store
                    .read_traffic_statistics(&query, &session, today)?
                    .into_iter()
                    .map(|b| (b.identity(), b))
                    .collect();
                for bucket in pending {
                    buckets.insert(bucket.identity(), bucket);
                }
                Ok::<_, crate::Error>(stats::build_view(
                    query,
                    &session,
                    buckets.into_values(),
                    today,
                ))
            })
            .await;
            if generation
                != service
                    .inner
                    .statistics
                    .query_generation
                    .load(Ordering::SeqCst)
            {
                return;
            }
            let mut view = service.inner.statistics.view.write();
            match result {
                Ok(Ok(mut result)) => {
                    result.save_error = save_error;
                    result.collection_error = collection_error;
                    *view = Arc::new(result);
                }
                _ => {
                    let current = Arc::make_mut(&mut view);
                    current.loading = false;
                    current.query_error = Some("无法读取统计，请检查数据库权限后重试".into());
                }
            }
            service.set_busy("traffic-query", false);
        });
    }

    pub fn clear_traffic_statistics(&self) {
        if self.is_busy("traffic-clear") {
            return;
        }
        self.set_busy("traffic-clear", true);
        // Invalidate requests already in flight before waiting for the writer.
        self.inner
            .statistics
            .generation
            .fetch_add(1, Ordering::SeqCst);
        self.inner
            .statistics
            .query_generation
            .fetch_add(1, Ordering::SeqCst);
        let service = self.clone();
        self.spawn(async move {
            let _guard = service.inner.statistics.operation.lock().await;
            let store = service.inner.store.clone();
            let result =
                tokio::task::spawn_blocking(move || store.clear_traffic_statistics()).await;
            match result {
                Ok(Ok(())) => {
                    service
                        .inner
                        .statistics
                        .generation
                        .fetch_add(1, Ordering::SeqCst);
                    service
                        .inner
                        .statistics
                        .data
                        .lock()
                        .clear(CoreEpoch::from_state(&service.inner.core.state()));
                    service.toast(ToastLevel::Success, "流量统计已清空");
                }
                _ => service.toast(ToastLevel::Error, "清空统计失败，请检查数据库权限后重试"),
            }
            service.set_busy("traffic-clear", false);
            drop(_guard);
            let query = service.inner.statistics.view.read().query.clone();
            service.query_traffic_statistics(query);
        });
    }

    async fn flush_statistics_locked(&self) {
        let (session, started, records) = {
            let data = self.inner.statistics.data.lock();
            (
                data.session.clone(),
                data.started_at,
                data.dirty
                    .iter()
                    .filter_map(|key| data.buckets.get(key).cloned())
                    .collect::<Vec<_>>(),
            )
        };
        if records.is_empty() {
            return;
        }
        let store = self.inner.store.clone();
        let result = tokio::task::spawn_blocking(move || {
            store.write_traffic_statistics(&session, started, &records)
        })
        .await;
        let mut data = self.inner.statistics.data.lock();
        match result {
            Ok(Ok(())) => {
                data.dirty.clear();
                data.save_error = None;
            }
            _ => {
                data.save_error =
                    Some("统计尚未保存，正在重试；请检查磁盘空间和数据目录权限".into());
            }
        }
        Arc::make_mut(&mut self.inner.statistics.view.write()).save_error = data.save_error.clone();
        *self.inner.statistics.last_flush.lock() = std::time::Instant::now();
    }

    pub(super) async fn flush_traffic_statistics(&self) {
        let _guard = self.inner.statistics.operation.lock().await;
        self.flush_statistics_locked().await;
    }

    /// Serialize manual refreshes and monitor ticks against the same counter baselines.
    pub(super) async fn poll_traffic_statistics(&self) {
        let _guard = self.inner.statistics.operation.lock().await;
        let state = self.inner.core.state();
        let Some(epoch) = CoreEpoch::from_state(&state) else {
            return;
        };
        let Some((model, settings, inbound_tags)) = self.inner.core.statistics_context() else {
            return;
        };
        let generation = self.inner.statistics.generation.load(Ordering::SeqCst);
        let valid = || {
            generation == self.inner.statistics.generation.load(Ordering::SeqCst)
                && CoreEpoch::from_state(&self.inner.core.state()).as_ref() == Some(&epoch)
        };
        let at = crate::util::now_unix;
        let success = if epoch.core == CoreKind::SingBox {
            match ClashClient::new(settings.api_port, "").connections().await {
                Ok(response) if valid() => {
                    self.inner
                        .traffic
                        .lock()
                        .sample(response.upload_total, response.download_total);
                    self.inner.statistics.data.lock().observe_singbox(
                        epoch.clone(),
                        &model,
                        &response,
                        stats::today(),
                        at(),
                    );
                    self.update_connections(Some(epoch.clone()), response.connections);
                    true
                }
                _ => false,
            }
        } else if let Ok(binary) = self.inner.core.resolve_binary(CoreKind::Xray, &settings) {
            match XrayStatsClient::new(binary, settings.stats_port)
                .query()
                .await
            {
                Ok(traffic) if valid() => {
                    self.inner
                        .traffic
                        .lock()
                        .sample(traffic.uplink, traffic.downlink);
                    self.inner.statistics.data.lock().observe_xray(
                        epoch.clone(),
                        &model,
                        &traffic,
                        &inbound_tags,
                        stats::today(),
                        at(),
                    );
                    true
                }
                _ => false,
            }
        } else {
            false
        };
        if valid() {
            let mut data = self.inner.statistics.data.lock();
            if !success && data.enabled {
                data.mark_collection_gap();
            }
            Arc::make_mut(&mut self.inner.statistics.view.write()).collection_error =
                data.collection_error;
        }
        if self.inner.statistics.last_flush.lock().elapsed() >= std::time::Duration::from_secs(10) {
            self.flush_statistics_locked().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{paths::AppPaths, runtime::ConnectionsResponse};

    fn wait_query(service: &AppService) {
        service.inner.rt.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while service.is_busy("traffic-query")
                    || service.is_busy("traffic-clear")
                    || service.is_busy("traffic-setting")
                {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
        });
    }
    #[test]
    fn query_merges_pending_without_double_counting_and_clear_invalidates_old_work() {
        let dir = tempfile::tempdir().unwrap();
        let service = AppService::with_paths(AppPaths::from_root(dir.path())).unwrap();
        let epoch = CoreEpoch {
            core: CoreKind::SingBox,
            pid: Some(1),
            started_at: Some(1),
            restarts: 0,
            helper_session: None,
        };
        {
            let mut data = service.inner.statistics.data.lock();
            data.rebase(true, None);
            data.observe_singbox(
                epoch.clone(),
                &Default::default(),
                &ConnectionsResponse {
                    upload_total: 100,
                    ..Default::default()
                },
                stats::today(),
                1,
            );
        }
        service
            .inner
            .rt
            .block_on(service.flush_traffic_statistics());
        service.inner.statistics.data.lock().observe_singbox(
            epoch,
            &Default::default(),
            &ConnectionsResponse {
                upload_total: 150,
                ..Default::default()
            },
            stats::today(),
            2,
        );
        service.query_traffic_statistics(TrafficQuery::default());
        wait_query(&service);
        assert_eq!(service.snapshot().traffic_statistics.total.upload, 150);
        service.query_traffic_statistics(TrafficQuery::default());
        service.clear_traffic_statistics();
        wait_query(&service);
        assert_eq!(service.snapshot().traffic_statistics.total.upload, 0);
        service
            .inner
            .rt
            .block_on(service.flush_traffic_statistics());
        service.query_traffic_statistics(TrafficQuery::default());
        wait_query(&service);
        assert_eq!(service.snapshot().traffic_statistics.total.upload, 0);
    }
    #[test]
    fn setting_persists_without_dirtying_the_core() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path());
        let service = AppService::with_paths(paths.clone()).unwrap();
        service.set_traffic_statistics_enabled(true);
        wait_query(&service);
        assert!(
            crate::Settings::load(&paths.settings_file())
                .unwrap()
                .traffic_statistics_enabled
        );
        assert!(service.inner.statistics.data.lock().enabled);
        assert!(!service.snapshot().config_dirty);
        service.set_traffic_statistics_enabled(false);
        wait_query(&service);
        assert!(!service.inner.statistics.data.lock().enabled);
    }
}
