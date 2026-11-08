//! Configurations are shared by explicit, expiring offers and committed after review.
use super::{AppService, ToastLevel, mesh_discovery::Target};
use crate::{
    Error, Result,
    mesh::{
        MeshPhase,
        sync::{
            self, Hello, Offer, ShareMode,
            data::{Bundle, Category, Difference, ReviewRow},
        },
    },
};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncView {
    pub sharing_profile: Option<String>,
    #[serde(default)]
    pub sharing_mode: ShareMode,
    #[serde(default)]
    pub catalog: Option<Arc<ShareCatalog>>,
    pub review: Option<Arc<Review>>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    pub id: String,
    pub profile: String,
    pub peer: String,
    pub rows: Vec<ReviewRow>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShareCatalog {
    pub id: String,
    pub profile: String,
    pub rows: Vec<crate::mesh::sync::data::ShareRow>,
}
struct Draft {
    created: Instant,
    view: Arc<ShareCatalog>,
    active: Option<String>,
    bundle: Bundle,
}
struct Pending {
    view: Arc<Review>,
    active: String,
    baseline: Bundle,
    incoming: Bundle,
}
#[derive(Default)]
pub(super) struct SyncState {
    pub(super) discovered: RwLock<BTreeMap<Target, Hello>>,
    pub(super) offer: Mutex<Option<Arc<Offer>>>,
    pending: Mutex<Option<Arc<Pending>>>,
    draft: Mutex<Option<Arc<Draft>>>,
}
impl SyncState {
    pub(super) fn expire_shares(&self, mesh: &crate::mesh::MeshSnapshot) {
        let connected = |id: &str| {
            mesh.networks
                .iter()
                .any(|n| n.profile_id == id && n.phase == MeshPhase::Connected)
        };
        self.offer
            .lock()
            .take_if(|o| o.expires <= Instant::now() || !connected(&o.profile));
        self.draft.lock().take_if(|d| {
            d.created.elapsed() >= Duration::from_secs(600) || !connected(&d.view.profile)
        });
    }
}
impl AppService {
    pub(super) fn sync_view(&self) -> SyncView {
        let (sharing_profile, sharing_mode) = self
            .inner
            .mesh_sync
            .offer
            .lock()
            .as_ref()
            .filter(|o| o.expires > Instant::now())
            .map(|o| (Some(o.profile.clone()), o.mode))
            .unwrap_or_default();
        SyncView {
            catalog: self
                .inner
                .mesh_sync
                .draft
                .lock()
                .as_ref()
                .map(|d| d.view.clone()),
            sharing_profile,
            sharing_mode,
            review: self
                .inner
                .mesh_sync
                .pending
                .lock()
                .as_ref()
                .map(|p| p.view.clone()),
        }
    }
    fn sync_capture(&self, cats: &BTreeSet<Category>) -> Bundle {
        let model = self.inner.model.read();
        let subs = self.inner.subs.read();
        let mesh = self.inner.mesh.profiles.read();
        Bundle::capture(&model, &subs, &mesh, cats)
    }
    fn sync_baseline(&self) -> Bundle {
        let model = self.inner.model.read();
        let subs = self.inner.subs.read();
        let mesh = self.inner.mesh.profiles.read();
        Bundle::local(&model, &subs, &mesh)
    }
    pub fn prepare_mesh_share(&self, profile: String) {
        let service = self.clone();
        if !self.inner.busy.write().insert("mesh-share".into()) {
            return;
        }
        self.inner.rt.spawn_blocking(move || {
            let active = service.inner.active_profile.read().clone();
            let bundle = service.sync_capture(&Category::ALL.into());
            let view = Arc::new(ShareCatalog {
                id: crate::util::new_id("share"),
                profile,
                rows: bundle.catalog(),
            });
            *service.inner.mesh_sync.draft.lock() = Some(Arc::new(Draft {
                created: Instant::now(),
                view,
                active,
                bundle,
            }));
            service.set_busy("mesh-share", false);
        });
    }
    pub fn close_mesh_share(&self, profile: &str) {
        self.inner
            .mesh_sync
            .draft
            .lock()
            .take_if(|d| d.view.profile == profile);
    }
    pub fn offer_mesh_data(&self, profile: String, draft_id: String, keys: BTreeSet<String>) {
        self.offer_mesh_data_with_mode(profile, draft_id, keys, ShareMode::SharingCode);
    }
    pub fn offer_mesh_data_with_mode(
        &self,
        profile: String,
        draft_id: String,
        keys: BTreeSet<String>,
        mode: ShareMode,
    ) {
        let service = self.clone();
        if !self.inner.busy.write().insert("mesh-share".into()) {
            return;
        }
        self.inner.rt.spawn_blocking(move || {
            let result = (|| -> Result<()> {
                if !service
                    .inner
                    .mesh
                    .snapshot()
                    .networks
                    .iter()
                    .any(|n| n.profile_id == profile && n.phase == MeshPhase::Connected)
                {
                    return Err(Error::config("请先连接要共享的组网"));
                }
                let draft = service
                    .inner
                    .mesh_sync
                    .draft
                    .lock()
                    .as_ref()
                    .filter(|d| d.view.id == draft_id && d.view.profile == profile)
                    .cloned()
                    .ok_or_else(|| Error::config("请重新打开共享配置"))?;
                let bundle = draft.bundle.selected(&keys)?;
                let categories = draft
                    .view
                    .rows
                    .iter()
                    .filter(|r| keys.contains(&r.key))
                    .map(|r| r.category)
                    .collect();
                if draft.active != *service.inner.active_profile.read()
                    || service.sync_capture(&categories).selected(&keys)? != bundle
                {
                    return Err(Error::config(
                        "所选配置已更改，请重新打开共享配置并核对项目",
                    ));
                }
                let offer = Offer::new(profile.clone(), &bundle, mode)?;
                service.close_mesh_share(&profile);
                *service.inner.mesh_sync.offer.lock() = Some(Arc::new(offer));
                Ok(())
            })();
            service.set_busy("mesh-share", false);
            if let Err(e) = result {
                service.toast(ToastLevel::Error, e.to_string())
            }
        });
    }
    pub fn mesh_share_code(&self, profile: &str) -> Option<String> {
        self.inner
            .mesh_sync
            .offer
            .lock()
            .as_ref()
            .filter(|o| o.profile == profile && o.expires > Instant::now())
            .and_then(|o| o.code())
    }
    pub fn stop_mesh_share(&self, profile: &str) {
        self.inner
            .mesh_sync
            .offer
            .lock()
            .take_if(|o| o.profile == profile);
    }
    pub fn receive_mesh_data(&self, profile: String, peer: String, code: String) {
        if !self.inner.busy.write().insert("mesh-sync".into()) {
            return;
        }
        let service = self.clone();
        self.spawn(async move {
            let result = async {
                let target = service.sync_target(&profile, &peer)?;
                let active = service
                    .inner
                    .active_profile
                    .read()
                    .clone()
                    .ok_or_else(|| Error::config("请先选择本地配置"))?;
                let mut incoming = tokio::time::timeout(Duration::from_secs(20), async {
                    sync::fetch(target.connect().await?, &code).await
                })
                .await
                .map_err(|_| Error::other("接收超时，请确认对端在线"))?
                .inspect_err(|_| {
                    // Show the code field immediately if a cached open offer has ended.
                    if code.trim().is_empty()
                        && let Some(hello) =
                            service.inner.mesh_sync.discovered.write().get_mut(&target)
                    {
                        hello.open_share = false;
                    }
                })?;
                if target != service.sync_target(&profile, &peer)?
                    || service.inner.active_profile.read().as_ref() != Some(&active)
                {
                    return Err(Error::config("对端或本地配置已变化，请重新接收"));
                }
                let baseline = service.sync_baseline();
                incoming.resolve_local_ids(&baseline);
                let view = Review {
                    id: crate::util::new_id("sync"),
                    profile,
                    peer,
                    rows: incoming.review(&baseline),
                };
                *service.inner.mesh_sync.pending.lock() = Some(Arc::new(Pending {
                    view: Arc::new(view),
                    active,
                    baseline,
                    incoming,
                }));
                Ok(())
            }
            .await;
            service.set_busy("mesh-sync", false);
            if let Err(e) = result {
                service.toast(ToastLevel::Error, e.to_string())
            }
        });
    }
    pub fn cancel_mesh_review(&self, id: &str) {
        self.inner
            .mesh_sync
            .pending
            .lock()
            .take_if(|p| p.view.id == id);
    }
    pub fn merge_mesh_data(&self, id: String, keys: BTreeSet<String>) {
        if !self.inner.busy.write().insert("mesh-sync".into()) {
            return;
        }
        let service = self.clone();
        self.spawn(async move {
            let _guard = service.inner.mesh.operation.lock().await;
            let result = service.commit_mesh_review(&id, &keys);
            service.set_busy("mesh-sync", false);
            match result {
                Ok(()) => service.toast(ToastLevel::Success, "已合并所选配置"),
                Err(e) => service.toast(ToastLevel::Error, e.to_string()),
            };
        });
    }
    fn commit_mesh_review(&self, id: &str, keys: &BTreeSet<String>) -> Result<()> {
        let plan = self
            .inner
            .mesh_sync
            .pending
            .lock()
            .as_ref()
            .filter(|p| p.view.id == id)
            .cloned()
            .ok_or_else(|| Error::config("预览已失效，请重新接收配置"))?;
        if self.inner.active_profile.read().as_ref() != Some(&plan.active) {
            return Err(Error::config("当前配置已切换，请重新接收"));
        }
        let active_mesh = self
            .inner
            .mesh
            .snapshot()
            .networks
            .into_iter()
            .filter(|n| !matches!(n.phase, MeshPhase::Disconnected | MeshPhase::Failed))
            .map(|n| n.profile_id)
            .collect();
        let mut model = self.inner.model.write();
        let mut subs = self.inner.subs.write();
        let mut mesh = self.inner.mesh.profiles.write();
        let current = Bundle::local(&model, &subs, &mesh);
        if current != plan.baseline {
            return Err(Error::config("本机配置已更改，请重新接收并核对差异"));
        }
        if plan
            .view
            .rows
            .iter()
            .any(|r| keys.contains(&r.key) && r.difference == Difference::Identical)
        {
            return Err(Error::config("相同项目无需合并"));
        }
        let (mut m, mut s, mut n) = (model.clone(), subs.clone(), mesh.clone());
        plan.incoming
            .merge(keys, &mut m, &mut s, &mut n, &active_mesh)?;
        let changed = m != *model;
        if changed {
            m.updated_at = crate::util::now_unix();
        }
        self.inner.store.commit_mesh_sync(
            &plan.active,
            changed.then_some(&m),
            (&subs, &s),
            (&mesh, &n),
        )?;
        *model = m;
        *subs = s;
        *mesh = n;
        drop(mesh);
        drop(subs);
        drop(model);
        self.inner
            .mesh_sync
            .pending
            .lock()
            .take_if(|p| p.view.id == id);
        if changed {
            self.mark_dirty();
            self.refresh_profiles();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        mesh::{MeshProfile, MeshProvider},
        model::{AppModel, Matchers, Node, Protocol, RouteAction, RouteRule},
        paths::AppPaths,
        subscription::Subscription,
    };
    fn preview(service: &AppService, bundle: Bundle) -> Review {
        let local = service.sync_baseline();
        let view = Review {
            id: crate::util::new_id("sync"),
            profile: "network".into(),
            peer: "peer".into(),
            rows: bundle.review(&local),
        };
        *service.inner.mesh_sync.pending.lock() = Some(Arc::new(Pending {
            view: Arc::new(view.clone()),
            active: service.inner.active_profile.read().clone().unwrap(),
            baseline: local,
            incoming: bundle,
        }));
        view
    }
    #[test]
    fn selective_merge_is_atomic_durable_and_repeatable() {
        let dir = tempfile::tempdir().unwrap();
        let service = AppService::with_paths(AppPaths::from_root(dir.path())).unwrap();
        let mut source = AppModel::default();
        let node = Node::new("test", Protocol::Socks, "proxy.example.test", 1080);
        source.add_nodes([node.clone()]);
        let mut matches = Matchers::default();
        matches.domains = vec!["example.test".into()];
        source
            .routing
            .rules
            .push(RouteRule::new("test", matches, RouteAction::direct()));
        let sub = Subscription::new("fixture", "https://example.test/list");
        let mesh = MeshProfile {
            provider: MeshProvider::Tailscale,
            hostname: "sender".into(),
            advertise_exit_node: true,
            ..Default::default()
        };
        let bundle = Bundle::capture(
            &source,
            &[sub],
            std::slice::from_ref(&mesh),
            &Category::ALL.into(),
        );
        let review = preview(&service, bundle.clone());
        assert_eq!(review.rows.len(), 4);
        let keys = review
            .rows
            .iter()
            .filter(|r| r.category != Category::Subscriptions)
            .map(|r| r.key.clone())
            .collect();
        service.inner.store.with_sync_test_failure();
        assert!(service.commit_mesh_review(&review.id, &keys).is_err());
        assert!(service.model().node(&node.id).is_none());
        let active = service.inner.active_profile.read().clone().unwrap();
        assert!(
            service
                .inner
                .store
                .load_profile(&active)
                .unwrap()
                .unwrap()
                .model
                .node(&node.id)
                .is_none()
        );
        service.inner.store.remove_sync_test_failure();
        service.commit_mesh_review(&review.id, &keys).unwrap();
        assert!(
            service
                .inner
                .store
                .load_profile(&active)
                .unwrap()
                .unwrap()
                .model
                .node(&node.id)
                .is_some()
        );
        assert!(service.inner.store.subscriptions().unwrap().is_empty());
        let received = service.inner.store.mesh_profiles().unwrap().pop().unwrap();
        assert_eq!(received.id, mesh.id);
        assert_ne!(received.hostname, mesh.hostname);
        assert!(!received.advertise_exit_node);
        let again = preview(&service, bundle.clone());
        assert!(
            again
                .rows
                .iter()
                .filter(|r| r.category != Category::Subscriptions)
                .all(|r| r.difference == Difference::Identical)
        );
        let mut changed = source;
        changed.node_mut(&node.id).unwrap().name = "changed".into();
        let diff = preview(
            &service,
            Bundle::capture(&changed, &[], &[], &[Category::Nodes].into()),
        );
        assert_eq!(diff.rows[0].difference, Difference::Conflict);
        service.inner.model.write().node_mut(&node.id).unwrap().name = "concurrent".into();
        assert!(
            service
                .commit_mesh_review(&diff.id, &[diff.rows[0].key.clone()].into())
                .is_err()
        );
        assert_eq!(service.model().node(&node.id).unwrap().name, "concurrent");
    }
}
