//! Offline persistence and mode-transition regressions.

use super::AppService;
use crate::{
    mesh::{MeshProfile, MeshProvider, MeshSecret},
    model::{MeshDnsMode, MeshExitTarget, Node, Protocol, WorkMode},
    paths::AppPaths,
    storage::Store,
};

fn service() -> (AppService, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let service = AppService::with_paths(AppPaths::from_root(directory.path())).unwrap();
    service
        .commit_model_change(|model| {
            model.add_nodes([Node::new(
                "Fixture proxy",
                Protocol::Socks,
                "192.0.2.10",
                1080,
            )]);
            Ok(true)
        })
        .unwrap();
    (service, directory)
}

fn profile() -> MeshProfile {
    MeshProfile {
        provider: MeshProvider::Tailscale,
        hostname: "xrose-fixture".into(),
        exit_node: "100.64.0.10".into(),
        outbound_port: 19081,
        outbound_secret: MeshSecret("fixture-secret-only".into()),
        ..Default::default()
    }
}

fn target(profile: &MeshProfile) -> MeshExitTarget {
    MeshExitTarget {
        profile_id: profile.id.clone(),
        peer_id: "fixture-peer".into(),
        provider: profile.provider,
        name: "Fixture system exit".into(),
        address: "100.64.0.20".into(),
        gateway_id: format!("mesh-exit-{}", profile.id),
    }
}

#[test]
fn system_exit_commit_preserves_split_target_and_proxy_group_selection() {
    let (service, _directory) = service();
    let profile = profile();
    service.inner.mesh.save(profile.clone()).unwrap();
    let original = service.model();
    let active = service.inner.active_profile.read().clone().unwrap();
    let target = target(&profile);
    service
        .inner
        .mesh
        .commit_system_exit(
            profile.clone(),
            &mut service.inner.model.write(),
            target.clone(),
            &active,
        )
        .unwrap();
    let selected = service.model();
    assert_eq!(selected.work_mode, WorkMode::Mesh);
    assert_eq!(selected.groups, original.groups);
    assert_eq!(selected.default_group, original.default_group);
    assert_eq!(selected.routing, original.routing);
    assert_eq!(selected.mesh_mode.target.as_ref(), Some(&target));
    assert_eq!(
        service.inner.mesh.profile(&profile.id).unwrap().exit_node,
        profile.exit_node
    );
    let stored = service
        .inner
        .store
        .load_profile(&active)
        .unwrap()
        .unwrap()
        .model;
    assert_eq!(stored, selected);

    let db = rusqlite::Connection::open(service.paths().database()).unwrap();
    db.execute_batch("CREATE TRIGGER fixture_fail_mesh_update BEFORE UPDATE ON profiles BEGIN SELECT RAISE(FAIL, 'fixture storage failure'); END;").unwrap();
    let mut replacement = target.clone();
    replacement.address = "100.64.0.30".into();
    assert!(
        service
            .inner
            .mesh
            .commit_system_exit(
                profile.clone(),
                &mut service.inner.model.write(),
                replacement,
                &active
            )
            .is_err()
    );
    assert_eq!(service.model(), selected);
    assert_eq!(service.inner.mesh.profile(&profile.id).unwrap(), profile);
}

#[test]
fn mesh_dns_and_mode_switches_preserve_saved_local_policy() {
    let (service, _directory) = service();
    let profile = profile();
    let target = target(&profile);
    service
        .commit_model_change(|model| {
            model.work_mode = WorkMode::Mesh;
            model.mesh_mode.target = Some(target.clone());
            Ok(true)
        })
        .unwrap();
    let original = service.model();
    assert_eq!(original.routing.rules_for_mode(WorkMode::Mesh).count(), 0);
    service.set_mesh_dns_mode(MeshDnsMode::Custom).unwrap();
    service.set_mesh_allow_lan(true).unwrap();
    assert_eq!(service.model().dns, original.dns);
    assert_eq!(service.model().routing, original.routing);
    service.select_direct_mode().unwrap();
    assert_eq!(service.model().work_mode, WorkMode::Direct);
    service.select_proxy_mode().unwrap();
    let restored = service.model();
    assert_eq!(restored.work_mode, WorkMode::Proxy);
    assert_eq!(restored.groups, original.groups);
    assert_eq!(restored.routing, original.routing);
    assert_eq!(restored.mesh_mode.target, original.mesh_mode.target);
    assert_eq!(restored.mesh_mode.dns, MeshDnsMode::Custom);
    assert!(restored.mesh_mode.allow_lan);
    assert!(restored.routing.rules_for_mode(WorkMode::Proxy).count() > 0);
    let active = service.inner.active_profile.read().clone().unwrap();
    assert_eq!(
        service
            .inner
            .store
            .load_profile(&active)
            .unwrap()
            .unwrap()
            .model,
        restored
    );
}

#[test]
fn release_schema_two_upgrades_without_enabling_mesh_takeover() {
    let directory = tempfile::tempdir().unwrap();
    let paths = AppPaths::from_root(directory.path());
    let store = Store::open(&paths).unwrap();
    let active = store.active_profile_id().unwrap();
    let original = store.load_profile(&active).unwrap().unwrap().model;
    let db = rusqlite::Connection::open(paths.database()).unwrap();
    db.pragma_update(None, "user_version", 2).unwrap();
    drop(db);
    drop(store);
    let reopened = Store::open(&paths).unwrap();
    let model = reopened.load_profile(&active).unwrap().unwrap().model;
    assert_eq!(model, original);
    assert_eq!(model.work_mode, WorkMode::Direct);
    assert_eq!(model.mesh_mode.dns, MeshDnsMode::Exit);
    assert!(model.mesh_mode.target.is_none());
    let db = rusqlite::Connection::open(paths.database()).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        crate::storage::SCHEMA_VERSION
    );
}

#[test]
fn named_group_mesh_gateway_selection_remains_a_proxy_split_target() {
    let (service, _directory) = service();
    let profile = profile();
    service.inner.mesh.save(profile.clone()).unwrap();
    let gateway = service.inner.mesh.outbound_node(&profile.id).unwrap();
    service
        .commit_model_change(|model| {
            model.add_nodes([gateway.clone()]);
            model.work_mode = WorkMode::Direct;
            Ok(true)
        })
        .unwrap();
    let group = service.model().default_group_id().unwrap();
    service.select_node(&group, &gateway.id);
    assert_eq!(service.model().work_mode, WorkMode::Proxy);
    assert_eq!(
        service.model().group(&group).unwrap().selected.as_deref(),
        Some(gateway.id.as_str())
    );
    assert!(service.model().mesh_mode.target.is_none());
    assert!(!service.is_busy("mesh"));
}

#[test]
fn profile_identity_cannot_change_during_a_mode_transition() {
    let (service, _directory) = service();
    let original = service.inner.active_profile.read().clone().unwrap();
    service.create_profile("Fixture alternate").unwrap();
    let current = service.model();
    let active = service.inner.active_profile.read().clone();
    service.set_busy("core", true);
    assert!(service.switch_profile(&original).is_err());
    assert!(service.create_profile("Fixture unexpected").is_err());
    assert!(service.delete_profile(&original).is_err());
    assert_eq!(service.model(), current);
    assert_eq!(*service.inner.active_profile.read(), active);
    assert_eq!(service.inner.store.active_profile_id(), active);
    service.set_busy("core", false);
}
