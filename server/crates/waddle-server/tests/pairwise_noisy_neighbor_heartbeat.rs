//! T3-08: Pairwise noisy-neighbor saturation + Turso LRU FD cap + reserved heartbeat capacity.

use std::sync::Arc;
use std::time::Duration;

use tempfile::tempdir;
use waddle_broker_core::boot_clock::{BootClock, FakeBootClock};
use waddle_broker_core::holder_pop::HolderKeypair;
use waddle_broker_core::identity::{CellRole, SpiffeId, TenantId};
use waddle_broker_core::placement::{PlacementLeaseStore, PlacementMode};
use waddle_broker_core::quotas::{TenantResourceController, TenantResourceQuotas};
use waddle_broker_core::store::{TenantQueryScope, TenantStore};
use waddle_server::db::turso_tenant_store::ServerTursoTenantStore;
use waddle_tenancy::placement_turso::TursoPlacementLeaseStore;
use waddle_tenancy::quiesce::LocalLeaseGuard;
use waddle_tenancy::turso_store::TursoPoolConfig;

#[tokio::test]
async fn pairwise_noisy_neighbor_saturation_preserves_fd_cap_victim_and_heartbeat() {
    let dir = tempdir().expect("tempdir");
    let lease_store = Arc::new(
        TursoPlacementLeaseStore::open(&dir.path().join("placement.db"))
            .await
            .expect("open placement db"),
    );

    // Capacity = (18 - 9) / 3 = 3 concurrent WAL DBs in pool.
    let tenant_store = ServerTursoTenantStore::new(
        dir.path().join("tenants"),
        TursoPoolConfig {
            rlimit_nofile_override: Some(18),
            reserved_fds: 9,
        },
        Some(Arc::clone(&lease_store) as Arc<dyn PlacementLeaseStore>),
    )
    .expect("bounded turso store");

    let controller = TenantResourceController::new(TenantResourceQuotas {
        max_mailbox_permits: 2,
        max_concurrent_tasks: 2,
        max_outstanding_streams: 2,
        max_wasm_instances: 2,
        max_db_concurrency: 2,
        reserved_control_plane_permits: 2,
        ..TenantResourceQuotas::default()
    })
    .expect("quotas");

    let noisy = TenantId::parse("noisy.example").expect("noisy");
    let victim = TenantId::parse("victim.example").expect("victim");
    let spiffe =
        SpiffeId::parse("spiffe://waddle.internal/ns/cells/sa/shared-pool").expect("spiffe");
    let role = CellRole::parse("shared-pool").expect("role");
    let holder = HolderKeypair::generate()
        .expect("kp")
        .sign_channel_binding(&spiffe, &[0x55; 32]);

    let mut scopes = Vec::new();
    for tid in [
        noisy.clone(),
        victim.clone(),
        TenantId::parse("churn1.example").expect("c1"),
        TenantId::parse("churn2.example").expect("c2"),
        TenantId::parse("churn3.example").expect("c3"),
    ] {
        tenant_store
            .provision_tenant(&tid)
            .await
            .expect("provision");
        let lease = lease_store
            .claim(&tid, &role, PlacementMode::SharedPool, &holder, 10_000)
            .await
            .expect("claim");
        let scope = TenantQueryScope {
            tenant_id: tid,
            holder: holder.clone(),
            placement_generation: lease.generation,
        };
        tenant_store.open_tenant(&scope).await.expect("open");
        scopes.push(scope);
    }

    let (open_count, pinned_count, evictions) = tenant_store.pool_stats().await;
    assert!(
        open_count <= 3,
        "open_count {open_count} must not exceed capacity 3"
    );
    assert_eq!(pinned_count, 0);
    assert!(
        evictions >= 2,
        "expected >= 2 LRU evictions, got {evictions}"
    );

    // Noisy saturates its data-plane semaphores.
    let _nm1 = controller.try_acquire_mailbox(&noisy).expect("nm1");
    let _nm2 = controller.try_acquire_mailbox(&noisy).expect("nm2");
    let _ns1 = controller.try_acquire_stream(&noisy).expect("ns1");
    let _ns2 = controller.try_acquire_stream(&noisy).expect("ns2");
    let _nd1 = controller.try_acquire_db(&noisy).expect("nd1");
    let _nd2 = controller.try_acquire_db(&noisy).expect("nd2");
    assert!(controller.try_acquire_mailbox(&noisy).is_err());
    assert!(controller.try_acquire_db(&noisy).is_err());

    // Control-plane heartbeat + cancellation + lease renew succeed under full noisy saturation.
    let _hb = controller
        .try_acquire_heartbeat(&noisy)
        .expect("noisy heartbeat");
    let _cancel = controller
        .try_acquire_cancellation(&noisy)
        .expect("noisy cancel");
    let clock = Arc::new(FakeBootClock::new(Duration::from_secs(20)));
    let renewed = lease_store
        .renew(&noisy, &holder, scopes[0].placement_generation, 10_000)
        .await
        .expect("renew noisy lease");
    let guard = LocalLeaseGuard::from_sent(
        clock.clone() as Arc<dyn BootClock>,
        noisy.clone(),
        renewed.generation,
        clock.now().expect("now"),
        Duration::from_secs(10),
        Duration::from_secs(2),
    )
    .expect("guard");
    assert!(guard.is_valid());

    // Victim continues to acquire semaphores and pass Turso write fence unaffected.
    let _vm = controller
        .try_acquire_mailbox(&victim)
        .expect("victim mailbox");
    let _vd = controller.try_acquire_db(&victim).expect("victim db");
    tenant_store
        .verify_write_fence(&scopes[1])
        .await
        .expect("victim write fence unaffected");
}
