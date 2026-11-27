use std::sync::Arc;
use std::time::Duration;

use tempfile::tempdir;
use waddle_broker_core::boot_clock::{BootClock, FakeBootClock};
use waddle_broker_core::holder_pop::{verify_channel_bound_holder, HolderKeypair, HolderPopError};
use waddle_broker_core::identity::{CellRole, HolderInstance, SpiffeId, TenantId};
use waddle_broker_core::placement::{
    FenceGeneration, LeaseError, PlacementLeaseStore, PlacementMode,
};
use waddle_broker_core::store::{TenantQueryScope, TenantStore, TenantStoreError};
use waddle_broker_core::trace_verifier::{assert_secret_free, verify_jsonl_trace};
use waddle_tenancy::placement_postgres::{
    PostgresPlacementLeaseStore, SqliteCompatPlacementLeaseStore,
};
use waddle_tenancy::placement_turso::TursoPlacementLeaseStore;
use waddle_tenancy::postgres_store::SqliteCompatPostgresStore;
use waddle_tenancy::quiesce::{
    LocalLeaseGuard, PlacementTraceEmitter, QuiesceController, QuiesceError,
};
use waddle_tenancy::turso_store::{TursoPoolConfig, TursoTenantStore};

fn mk_holder(spiffe: &str, cb: [u8; 32]) -> (HolderKeypair, HolderInstance) {
    let spiffe_id = SpiffeId::parse(spiffe).expect("valid spiffe");
    let kp = HolderKeypair::generate().expect("keypair");
    let holder = kp.sign_channel_binding(&spiffe_id, &cb);
    (kp, holder)
}

async fn run_all_seven_placement_scenarios<L: PlacementLeaseStore + 'static, S: TenantStore>(
    lease_store: Arc<L>,
    tenant_store: &S,
) {
    let shared_spiffe = "spiffe://waddle.internal/ns/cells/sa/shared-pool";
    let dedicated_spiffe = "spiffe://waddle.internal/ns/cells/sa/dedicated-acme";
    let shared_role = CellRole::parse("shared-pool").expect("valid role");
    let dedicated_role = CellRole::parse("dedicated-acme").expect("valid role");

    // -----------------------------------------------------------------------
    // Scenario 1: Lease expiry & crash reclaim + PObserve JSONL verification
    // -----------------------------------------------------------------------
    let t_expiry = TenantId::parse("expiry.example").expect("valid");
    tenant_store
        .provision_tenant(&t_expiry)
        .await
        .expect("provision expiry");
    let (_, holder_a) = mk_holder(shared_spiffe, [0x01; 32]);
    let (_, holder_b) = mk_holder(shared_spiffe, [0x02; 32]);

    let trace1 = PlacementTraceEmitter::new();
    let lease_a = lease_store
        .claim(
            &t_expiry,
            &shared_role,
            PlacementMode::SharedPool,
            &holder_a,
            500,
        )
        .await
        .expect("claim A");
    assert_eq!(lease_a.generation, FenceGeneration::new(1));
    trace1.emit_claim("cell-a", "t-expiry", &holder_a, lease_a.generation);

    let scope_a = TenantQueryScope {
        tenant_id: t_expiry.clone(),
        holder: holder_a.clone(),
        placement_generation: lease_a.generation,
    };
    tenant_store.open_tenant(&scope_a).await.expect("open A");
    tenant_store
        .verify_write_fence(&scope_a)
        .await
        .expect("write fence A");
    trace1.emit_tenant_write("cell-a", "t-expiry", &holder_a, lease_a.generation);

    // Wait past authoritative DB TTL (500ms).
    tokio::time::sleep(Duration::from_millis(620)).await;

    assert!(matches!(
        lease_store
            .renew(&t_expiry, &holder_a, lease_a.generation, 5_000)
            .await,
        Err(LeaseError::LeaseExpired(_))
    ));
    // Stale write after expiry fails fence check because lease_store.current() is None.
    assert!(tenant_store.verify_write_fence(&scope_a).await.is_err());

    let lease_b = lease_store
        .claim(
            &t_expiry,
            &shared_role,
            PlacementMode::SharedPool,
            &holder_b,
            5_000,
        )
        .await
        .expect("reclaim B");
    assert_eq!(lease_b.generation, FenceGeneration::new(2));
    trace1.emit_claim("cell-b", "t-expiry", &holder_b, lease_b.generation);

    let scope_b = TenantQueryScope {
        tenant_id: t_expiry.clone(),
        holder: holder_b.clone(),
        placement_generation: lease_b.generation,
    };
    tenant_store.open_tenant(&scope_b).await.expect("open B");
    tenant_store
        .verify_write_fence(&scope_b)
        .await
        .expect("write fence B");
    trace1.emit_tenant_write("cell-b", "t-expiry", &holder_b, lease_b.generation);

    let jsonl1 = trace1.to_jsonl();
    let summary1 =
        verify_jsonl_trace(&jsonl1).expect("Scenario 1 trace must pass PlacementLeaseMonitor");
    assert!(
        summary1.records_processed >= 4 && summary1.tenant_writes_observed >= 2,
        "Scenario 1 trace must be non-vacuous: {summary1:?}"
    );
    let proof_hex = hex::encode(holder_a.tls_exporter_proof.as_bytes());
    assert_secret_free(
        &jsonl1,
        &["sk-test-secret", &proof_hex, "alice@expiry.example"],
    )
    .expect("Scenario 1 trace must be secret-free");

    // -----------------------------------------------------------------------
    // Scenario 2: Suspend-aware BootInstant local lease guard
    // -----------------------------------------------------------------------
    let boot_clock = Arc::new(FakeBootClock::new(Duration::from_secs(1_000)));
    let sent_at = boot_clock.now().expect("clock available");
    let local_guard = LocalLeaseGuard::from_sent(
        Arc::clone(&boot_clock) as Arc<dyn waddle_broker_core::boot_clock::BootClock>,
        t_expiry.clone(),
        lease_b.generation,
        sent_at,
        Duration::from_millis(5_000),
        Duration::from_millis(1_000),
    )
    .expect("local guard");
    assert!(local_guard.is_valid());

    // Simulate host suspend of 4,500ms (exceeding 5,000 - 1,000 = 4,000ms usable window).
    boot_clock.advance_suspend(Duration::from_millis(4_500));
    assert!(
        !local_guard.is_valid(),
        "BootInstant guard must immediately self-fence after suspend past deadline"
    );

    // -----------------------------------------------------------------------
    // Scenario 3: Client clock skew immunity (authoritative DB clock only)
    // -----------------------------------------------------------------------
    // Even if a client's wall clock were skewed by +/- 10 minutes, `claim`/`renew`
    // accept only `ttl_ms` (never client timestamps) and compute `expires_at_ms`
    // exclusively from the DB clock.
    let t_skew = TenantId::parse("skew.example").expect("valid");
    let lease_skew = lease_store
        .claim(
            &t_skew,
            &shared_role,
            PlacementMode::SharedPool,
            &holder_a,
            10_000,
        )
        .await
        .expect("claim skew");
    let renewed_skew = lease_store
        .renew(&t_skew, &holder_a, lease_skew.generation, 15_000)
        .await
        .expect("renew skew");
    assert!(renewed_skew.expires_at_ms >= lease_skew.expires_at_ms);
    // Zero or >MAX_LEASE_TTL_MS rejected.
    assert!(matches!(
        lease_store
            .claim(
                &t_skew,
                &shared_role,
                PlacementMode::SharedPool,
                &holder_a,
                0
            )
            .await,
        Err(LeaseError::InvalidTtl(0))
    ));

    // -----------------------------------------------------------------------
    // Scenario 4: Network partition & stale writer fencing
    // -----------------------------------------------------------------------
    let t_part = TenantId::parse("partition.example").expect("valid");
    tenant_store
        .provision_tenant(&t_part)
        .await
        .expect("provision partition");
    let trace4 = PlacementTraceEmitter::new();
    let part_a = lease_store
        .claim(
            &t_part,
            &shared_role,
            PlacementMode::SharedPool,
            &holder_a,
            300,
        )
        .await
        .expect("claim part A");
    trace4.emit_claim("cell-a", "t-part", &holder_a, part_a.generation);
    let part_scope_a = TenantQueryScope {
        tenant_id: t_part.clone(),
        holder: holder_a.clone(),
        placement_generation: part_a.generation,
    };
    tenant_store
        .open_tenant(&part_scope_a)
        .await
        .expect("open part A");

    // Cell A partitioned; lease expires; Cell B claims gen=2 and opens storage.
    tokio::time::sleep(Duration::from_millis(420)).await;
    let part_b = lease_store
        .claim(
            &t_part,
            &shared_role,
            PlacementMode::SharedPool,
            &holder_b,
            5_000,
        )
        .await
        .expect("claim part B");
    assert_eq!(part_b.generation, FenceGeneration::new(2));
    trace4.emit_claim("cell-b", "t-part", &holder_b, part_b.generation);
    let part_scope_b = TenantQueryScope {
        tenant_id: t_part.clone(),
        holder: holder_b.clone(),
        placement_generation: part_b.generation,
    };
    tenant_store
        .open_tenant(&part_scope_b)
        .await
        .expect("open part B");
    tenant_store
        .verify_write_fence(&part_scope_b)
        .await
        .expect("write B ok");
    trace4.emit_tenant_write("cell-b", "t-part", &holder_b, part_b.generation);

    // Partition heals: Cell A's delayed write under gen=1 MUST be rejected.
    assert!(matches!(
        tenant_store.verify_write_fence(&part_scope_a).await,
        Err(TenantStoreError::StalePlacementGeneration { .. })
    ));
    let summary4 = verify_jsonl_trace(&trace4.to_jsonl()).expect("Scenario 4 trace valid");
    assert!(
        summary4.records_processed >= 3 && summary4.tenant_writes_observed >= 1,
        "Scenario 4 trace must be non-vacuous: {summary4:?}"
    );

    // -----------------------------------------------------------------------
    // Scenario 5: Duplicate claim race (N = 8 concurrent contenders)
    // -----------------------------------------------------------------------
    let t_race = TenantId::parse("race.example").expect("valid");
    let mut handles = Vec::new();
    for i in 0..8u8 {
        let store = Arc::clone(&lease_store);
        let tenant = t_race.clone();
        let role = shared_role.clone();
        let (_, contender) = mk_holder(shared_spiffe, [i + 10; 32]);
        handles.push(tokio::spawn(async move {
            store
                .claim(
                    &tenant,
                    &role,
                    PlacementMode::SharedPool,
                    &contender,
                    10_000,
                )
                .await
        }));
    }
    let mut winners = 0usize;
    let mut already_held = 0usize;
    for h in handles {
        match h.await.expect("join") {
            Ok(lease) => {
                assert_eq!(lease.generation, FenceGeneration::new(1));
                winners += 1;
            }
            Err(LeaseError::AlreadyHeld { .. }) => already_held += 1,
            Err(other) => panic!("unexpected race error: {other}"),
        }
    }
    assert_eq!(winners, 1, "exactly 1 contender must win the claim race");
    assert_eq!(
        already_held, 7,
        "all other contenders must receive AlreadyHeld"
    );

    // -----------------------------------------------------------------------
    // Scenario 6: Same-role / same-SPIFFE-ID cell race + RFC 9266 replay + DedicatedCell
    // -----------------------------------------------------------------------
    let t_same = TenantId::parse("same-sa.example").expect("valid");
    let cb_1 = [0xAA; 32];
    let cb_2 = [0xBB; 32];
    let spiffe_parsed = SpiffeId::parse(shared_spiffe).expect("valid");
    let (_, same_a) = mk_holder(shared_spiffe, cb_1);
    let (_, same_b) = mk_holder(shared_spiffe, cb_2);

    // Verify RFC 9266 channel binding rejects replay of same_a's proof on cb_2.
    assert!(verify_channel_bound_holder(&spiffe_parsed, &cb_1, &same_a).is_ok());
    assert_eq!(
        verify_channel_bound_holder(&spiffe_parsed, &cb_2, &same_a),
        Err(HolderPopError::InvalidSignature)
    );

    let same_lease = lease_store
        .claim(
            &t_same,
            &shared_role,
            PlacementMode::SharedPool,
            &same_a,
            10_000,
        )
        .await
        .expect("same_a claim");
    // same_b shares identical SPIFFE ID & role, but different incarnation_pubkey -> rejected!
    assert!(matches!(
        lease_store
            .renew(&t_same, &same_b, same_lease.generation, 10_000)
            .await,
        Err(LeaseError::HolderMismatch(_))
    ));
    assert!(matches!(
        lease_store
            .release(&t_same, &same_b, same_lease.generation)
            .await,
        Err(LeaseError::HolderMismatch(_))
    ));
    assert!(matches!(
        lease_store
            .transfer(&t_same, &same_b, &same_a, same_lease.generation, 10_000)
            .await,
        Err(LeaseError::HolderMismatch(_))
    ));

    // DedicatedCell policy: shared-pool SA MUST NOT claim a dedicated cell lease.
    let t_ded = TenantId::parse("acme.example").expect("valid");
    assert!(matches!(
        lease_store
            .claim(
                &t_ded,
                &dedicated_role,
                PlacementMode::DedicatedCell,
                &same_a,
                10_000
            )
            .await,
        Err(LeaseError::HolderMismatch(_))
    ));
    let (_, ded_holder) = mk_holder(dedicated_spiffe, [0xCC; 32]);
    let ded_lease = lease_store
        .claim(
            &t_ded,
            &dedicated_role,
            PlacementMode::DedicatedCell,
            &ded_holder,
            10_000,
        )
        .await
        .expect("dedicated SA succeeds");
    // Release preserves monotonic generation so next claim gets gen=2.
    lease_store
        .release(&t_ded, &ded_holder, ded_lease.generation)
        .await
        .expect("release");
    let ded_reclaimed = lease_store
        .claim(
            &t_ded,
            &dedicated_role,
            PlacementMode::DedicatedCell,
            &ded_holder,
            10_000,
        )
        .await
        .expect("reclaim after release");
    assert_eq!(ded_reclaimed.generation, FenceGeneration::new(2));

    // -----------------------------------------------------------------------
    // Scenario 7: Graceful quiesce within WADDLE_DRAIN_TIMEOUT_SECS + transfer
    // -----------------------------------------------------------------------
    let t_handoff = TenantId::parse("handoff.example").expect("valid");
    tenant_store
        .provision_tenant(&t_handoff)
        .await
        .expect("provision handoff");
    let trace7 = PlacementTraceEmitter::new();

    let h_lease = lease_store
        .claim(
            &t_handoff,
            &shared_role,
            PlacementMode::SharedPool,
            &holder_a,
            10_000,
        )
        .await
        .expect("claim handoff A");
    trace7.emit_claim("cell-a", "t-handoff", &holder_a, h_lease.generation);

    let h_scope_a = TenantQueryScope {
        tenant_id: t_handoff.clone(),
        holder: holder_a.clone(),
        placement_generation: h_lease.generation,
    };
    tenant_store
        .open_tenant(&h_scope_a)
        .await
        .expect("open handoff A");
    trace7.emit_tenant_write("cell-a", "t-handoff", &holder_a, h_lease.generation);

    let q_clock = Arc::new(FakeBootClock::new(Duration::from_secs(500)));
    let q_controller = QuiesceController::new(
        Arc::clone(&q_clock) as Arc<dyn waddle_broker_core::boot_clock::BootClock>,
        Duration::from_secs(2),
    );

    // 7a: Exceeding drain budget aborts handoff WITHOUT calling transfer.
    let guard_timeout = LocalLeaseGuard::from_sent(
        Arc::clone(&q_clock) as Arc<dyn waddle_broker_core::boot_clock::BootClock>,
        t_handoff.clone(),
        h_lease.generation,
        q_clock.now().expect("now"),
        Duration::from_secs(10),
        Duration::from_secs(1),
    )
    .expect("guard");
    let clock_for_slow = Arc::clone(&q_clock);
    let err = q_controller
        .graceful_handoff(
            lease_store.as_ref(),
            tenant_store,
            &guard_timeout,
            &t_handoff,
            &holder_a,
            &holder_b,
            h_lease.generation,
            10_000,
            move |_deadline| async move {
                clock_for_slow.advance(Duration::from_secs(3));
                false
            },
        )
        .await
        .expect_err("over-budget drain must abort handoff");
    assert!(matches!(err, QuiesceError::DrainTimeoutExceeded(_)));
    assert!(
        !guard_timeout.is_valid(),
        "local guard must be revoked on timeout"
    );
    assert_eq!(
        lease_store
            .current(&t_handoff)
            .await
            .expect("current")
            .expect("still held")
            .generation,
        FenceGeneration::new(1),
        "aborted drain must NOT have transferred the lease"
    );

    // 7b: Timely quiesce checkpoints storage & transfers to Cell B at gen=2.
    let guard_ok = LocalLeaseGuard::from_sent(
        Arc::clone(&q_clock) as Arc<dyn waddle_broker_core::boot_clock::BootClock>,
        t_handoff.clone(),
        h_lease.generation,
        q_clock.now().expect("now"),
        Duration::from_secs(10),
        Duration::from_secs(1),
    )
    .expect("guard");
    let clock_for_fast = Arc::clone(&q_clock);
    let outcome = q_controller
        .graceful_handoff(
            lease_store.as_ref(),
            tenant_store,
            &guard_ok,
            &t_handoff,
            &holder_a,
            &holder_b,
            h_lease.generation,
            10_000,
            move |_deadline| async move {
                clock_for_fast.advance(Duration::from_millis(250));
                true
            },
        )
        .await
        .expect("timely handoff succeeds");
    assert_eq!(outcome.new_lease.generation, FenceGeneration::new(2));
    assert!(!guard_ok.is_valid());
    trace7.emit_transfer(
        "cell-a",
        "t-handoff",
        &holder_a,
        &holder_b,
        outcome.new_lease.generation,
    );

    let h_scope_b = TenantQueryScope {
        tenant_id: t_handoff.clone(),
        holder: holder_b.clone(),
        placement_generation: outcome.new_lease.generation,
    };
    tenant_store
        .open_tenant(&h_scope_b)
        .await
        .expect("open handoff B");
    tenant_store
        .verify_write_fence(&h_scope_b)
        .await
        .expect("write B");
    trace7.emit_tenant_write(
        "cell-b",
        "t-handoff",
        &holder_b,
        outcome.new_lease.generation,
    );

    // Cell A's stale gen=1 write is now rejected.
    assert!(tenant_store.verify_write_fence(&h_scope_a).await.is_err());

    let jsonl7 = trace7.to_jsonl();
    let summary7 = verify_jsonl_trace(&jsonl7).expect("Scenario 7 trace valid");
    assert!(
        summary7.records_processed >= 4 && summary7.tenant_writes_observed >= 2,
        "Scenario 7 trace must be non-vacuous: {summary7:?}"
    );
    assert!(
        summary1.records_processed + summary4.records_processed + summary7.records_processed >= 10,
        "combined placement traces must process >= 10 records"
    );
    assert_secret_free(&jsonl7, &["sk-live-secret", &proof_hex]).expect("secret-free");
}

#[tokio::test]
async fn turso_placement_lease_store_passes_all_seven_scenarios() {
    let dir = tempdir().expect("tempdir");
    let lease_store = Arc::new(
        TursoPlacementLeaseStore::open(&dir.path().join("placement.db"))
            .await
            .expect("open placement.db"),
    );
    let tenant_store = TursoTenantStore::new(
        dir.path().join("tenants"),
        TursoPoolConfig {
            rlimit_nofile_override: Some(64),
            reserved_fds: 16,
        },
        Some(Arc::clone(&lease_store) as Arc<dyn PlacementLeaseStore>),
    )
    .expect("turso tenant store");

    run_all_seven_placement_scenarios(lease_store, &tenant_store).await;
}

#[tokio::test]
async fn postgres_compat_placement_lease_store_passes_all_seven_scenarios() {
    let lease_store = Arc::new(
        SqliteCompatPlacementLeaseStore::new_in_memory()
            .await
            .expect("compat placement store"),
    );
    let tenant_store = SqliteCompatPostgresStore::new_in_memory(Some(
        Arc::clone(&lease_store) as Arc<dyn PlacementLeaseStore>
    ))
    .await
    .expect("compat tenant store");

    run_all_seven_placement_scenarios(lease_store, &tenant_store).await;
}

#[tokio::test]
async fn live_postgres_placement_lease_store_when_env_configured() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let pool = sqlx::PgPool::connect(&url)
        .await
        .expect("connect WADDLE_TEST_POSTGRES_URL");
    let lease_store = Arc::new(PostgresPlacementLeaseStore::new(pool.clone()));
    lease_store.ensure_schema().await.expect("ensure schema");
    let tenant_store = waddle_tenancy::postgres_store::PostgresTenantStore::new(
        pool,
        Some(Arc::clone(&lease_store) as Arc<dyn PlacementLeaseStore>),
    );
    tenant_store.ensure_schema().await.expect("ensure schema");
    run_all_seven_placement_scenarios(lease_store, &tenant_store).await;
}
