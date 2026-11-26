//! T4-S03 (Release Evidence Gate 1): Noisy-neighbor saturation under RLIMIT_NOFILE & Wasm memory caps.

use std::collections::HashSet;
use std::sync::Arc;

use tempfile::tempdir;
use waddle_broker_core::holder_pop::HolderKeypair;
use waddle_broker_core::identity::{CellRole, SpiffeId, TenantId};
use waddle_broker_core::placement::{PlacementLeaseStore, PlacementMode};
use waddle_broker_core::quotas::{TenantResourceController, TenantResourceQuotas};
use waddle_broker_core::store::TenantQueryScope;
use waddle_extensions::config::RuntimeLimits;
use waddle_extensions::host_tools::{DenyingExtensionHostTools, InvocationContext, InvocationKind};
use waddle_extensions::runtime::{LoadedExtension, WasmRuntime};
use waddle_extensions::types::{
    CommandAction, CommandInvocation, CommandNode, ExtensionEvent, FullJidValue,
};
use waddle_extensions::{PluginId, WaddleId};
use waddle_server::db::tenant_registry::ServerTenantRegistry;
use waddle_server::db::turso_tenant_store::ServerTursoTenantStore;
use waddle_tenancy::placement_turso::TursoPlacementLeaseStore;
use waddle_tenancy::turso_store::TursoPoolConfig;

const BASE_COMPONENT_WAT: &str =
    include_str!("../../waddle-extensions/tests/fixtures/message_hook.wat");

fn grow_memory_wat(extra_pages: u32) -> String {
    let needle = "      i32.const 1024\n    )";
    BASE_COMPONENT_WAT.replacen(
        needle,
        &format!(
            "      (drop (memory.grow (i32.const {extra_pages})))\n      i32.const 1024\n    )"
        ),
        1,
    )
}

fn event_ctx(tenant: &TenantId) -> (ExtensionEvent, InvocationContext) {
    let event = ExtensionEvent::Command(CommandInvocation {
        waddle_id: WaddleId::try_new(tenant.as_str()).expect("waddle"),
        room: None,
        requester: FullJidValue::try_new("alice@example.org/web").expect("requester"),
        command_node: CommandNode::try_new("urn:waddle:extension:1:gate1").expect("node"),
        session_id: None,
        action: Some(CommandAction::Execute),
        form: None,
        fields: Vec::new(),
    });
    let ctx = InvocationContext {
        tenant_id: Some(tenant.clone()),
        waddle_id: WaddleId::try_new(tenant.as_str()).expect("waddle"),
        plugin_id: PluginId::try_new("gate1-guest").expect("plugin"),
        requester: Some("alice@example.org".parse().expect("bare")),
        source_room: None,
        kind: InvocationKind::Command,
        provider_room_grants: Vec::new(),
    };
    (event, ctx)
}

#[tokio::test]
async fn gate1_noisy_neighbor_saturation_under_rlimit_and_wasm_memory_caps() {
    let dir = tempdir().expect("tempdir");
    let lease_store = Arc::new(
        TursoPlacementLeaseStore::open(&dir.path().join("placement.db"))
            .await
            .expect("placement db"),
    );

    // Capacity = (15 - 9) / 3 = 2 WAL DBs max open concurrently.
    let tenant_store = ServerTursoTenantStore::new(
        dir.path().join("tenants"),
        TursoPoolConfig {
            rlimit_nofile_override: Some(15),
            reserved_fds: 9,
        },
        Some(Arc::clone(&lease_store) as Arc<dyn PlacementLeaseStore>),
    )
    .expect("store");
    let controller = TenantResourceController::new(TenantResourceQuotas {
        max_mailbox_permits: 2,
        max_concurrent_tasks: 2,
        max_outstanding_streams: 2,
        max_wasm_instances: 2,
        max_wasm_memory_bytes: 1_048_576,
        max_db_concurrency: 2,
        reserved_control_plane_permits: 2,
        ..TenantResourceQuotas::default()
    })
    .expect("quotas");
    let registry = ServerTenantRegistry::new(
        tenant_store.clone().into_store_arc(),
        None,
        TenantResourceQuotas {
            max_mailbox_permits: 2,
            max_concurrent_tasks: 2,
            max_outstanding_streams: 2,
            max_wasm_instances: 2,
            max_wasm_memory_bytes: 1_048_576,
            max_db_concurrency: 2,
            reserved_control_plane_permits: 2,
            ..TenantResourceQuotas::default()
        },
    )
    .expect("registry");

    let noisy = TenantId::parse("noisy.example").expect("noisy");
    let victim = TenantId::parse("victim.example").expect("victim");
    let churn1 = TenantId::parse("churn-a.example").expect("churn1");
    let churn2 = TenantId::parse("churn-b.example").expect("churn2");
    let spiffe =
        SpiffeId::parse("spiffe://waddle.internal/ns/cells/sa/shared-pool").expect("spiffe");
    let role = CellRole::parse("shared-pool").expect("role");
    let holder = HolderKeypair::generate()
        .expect("kp")
        .sign_channel_binding(&spiffe, &[0x66; 32]);

    let mut victim_scope = None;
    for tid in [&noisy, &churn1, &churn2, &victim] {
        registry.provision_tenant(tid).await.expect("provision");
        let lease = lease_store
            .claim(tid, &role, PlacementMode::SharedPool, &holder, 10_000)
            .await
            .expect("claim");
        let scope = TenantQueryScope {
            tenant_id: tid.clone(),
            holder: holder.clone(),
            placement_generation: lease.generation,
        };
        let guard = registry.acquire_tenant(&scope).await.expect("acquire");
        assert_eq!(guard.tenant_id(), tid);
        if tid == &victim {
            victim_scope = Some(scope);
        }
    }

    let (open_count, _, evictions) = tenant_store.pool_stats().await;
    assert!(
        open_count <= 2,
        "open_count {open_count} exceeds RLIMIT cap 2"
    );
    assert!(
        evictions >= 2,
        "expected LRU evictions >= 2, got {evictions}"
    );

    // Noisy saturates its quotas & hits Wasm 1 MiB memory cap.
    let _np1 = controller.try_acquire_mailbox(&noisy).expect("np1");
    let _np2 = controller.try_acquire_mailbox(&noisy).expect("np2");
    assert!(controller.try_acquire_mailbox(&noisy).is_err());

    let grow_wat_path = dir.path().join("noisy_grow.wat");
    let healthy_wat_path = dir.path().join("victim_ok.wat");
    std::fs::write(&grow_wat_path, grow_memory_wat(16)).expect("write grow");
    std::fs::write(&healthy_wat_path, BASE_COMPONENT_WAT).expect("write healthy");

    let runtime = WasmRuntime::try_new().expect("wasm runtime");
    let tools = Arc::new(DenyingExtensionHostTools);

    let noisy_ext = LoadedExtension::load(&runtime, &grow_wat_path)
        .expect("load noisy")
        .with_limits(RuntimeLimits {
            memory_bytes: 1_048_576,
            ..RuntimeLimits::default()
        })
        .expect("limits");
    let noisy_tracker = controller.memory_tracker(&noisy);
    let (nev, nctx) = event_ctx(&noisy);
    assert!(noisy_ext
        .call_handle_event_with_memory_tracker(
            nev,
            tools.clone(),
            nctx,
            "{}".to_owned(),
            HashSet::new(),
            Vec::new(),
            Some(Arc::clone(&noisy_tracker)),
        )
        .await
        .is_err());
    assert_eq!(noisy_tracker.allocated_bytes(), 0);

    // Victim continues functioning cleanly: registry handle + semaphore + healthy Wasm execution.
    let _vm = controller
        .try_acquire_mailbox(&victim)
        .expect("victim mailbox");
    let _vs = controller
        .try_acquire_stream(&victim)
        .expect("victim stream");
    let _vh = registry
        .acquire_tenant(victim_scope.as_ref().expect("victim scope"))
        .await
        .expect("victim handle after noisy LRU churn");

    let victim_ext = LoadedExtension::load(&runtime, &healthy_wat_path)
        .expect("load victim")
        .with_limits(RuntimeLimits {
            memory_bytes: 1_048_576,
            ..RuntimeLimits::default()
        })
        .expect("limits");
    let (vev, vctx) = event_ctx(&victim);
    victim_ext
        .call_handle_event_with_memory_tracker(
            vev,
            tools,
            vctx,
            "{}".to_owned(),
            HashSet::new(),
            Vec::new(),
            Some(controller.memory_tracker(&victim)),
        )
        .await
        .expect("victim extension completes cleanly despite noisy neighbor");
    assert_eq!(controller.memory_tracker(&victim).allocated_bytes(), 0);
}
