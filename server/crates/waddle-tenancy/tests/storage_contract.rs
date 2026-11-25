use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tempfile::tempdir;
use waddle_broker_core::holder_pop::HolderKeypair;
use waddle_broker_core::identity::{AccountId, HolderInstance, SpiffeId, TenantId};
use waddle_broker_core::placement::FenceGeneration;
use waddle_broker_core::quotas::TenantResourceQuotas;
use waddle_broker_core::store::{TenantQueryScope, TenantStore, TenantStoreError};
use waddle_tenancy::data_ops::{
    RemoteParticipantRecord, TenantAuthIdentityRecord, TenantDataOps, TenantJobRecord,
    TenantSpaceRecord, TenantUserRecord,
};
use waddle_tenancy::postgres_store::{PostgresTenantStore, SqliteCompatPostgresStore};
use waddle_tenancy::registry::TenantRegistry;
use waddle_tenancy::turso_store::{TursoPoolConfig, TursoTenantStore};

fn make_holder(spiffe: &str, channel_binding: [u8; 32]) -> HolderInstance {
    let spiffe_id = SpiffeId::parse(spiffe).expect("valid spiffe");
    let kp = HolderKeypair::generate().expect("keypair");
    kp.sign_channel_binding(&spiffe_id, &channel_binding)
}

async fn run_shared_storage_contract<S: TenantDataOps>(store: &S) {
    let tenant_a = TenantId::parse("tenant-a.example").expect("valid");
    let tenant_b = TenantId::parse("tenant-b.example").expect("valid");
    let holder_a = make_holder(
        "spiffe://waddle.internal/ns/cells/sa/shared-pool",
        [0x11; 32],
    );
    let holder_b = make_holder(
        "spiffe://waddle.internal/ns/cells/sa/shared-pool",
        [0x22; 32],
    );

    store
        .provision_tenant(&tenant_a)
        .await
        .expect("provision tenant A");
    store
        .provision_tenant(&tenant_b)
        .await
        .expect("provision tenant B");
    assert!(matches!(
        store.provision_tenant(&tenant_a).await,
        Err(TenantStoreError::TenantAlreadyExists(_))
    ));

    let scope_a = TenantQueryScope {
        tenant_id: tenant_a.clone(),
        holder: holder_a.clone(),
        placement_generation: FenceGeneration::new(1),
    };
    let scope_b = TenantQueryScope {
        tenant_id: tenant_b.clone(),
        holder: holder_b.clone(),
        placement_generation: FenceGeneration::new(1),
    };

    store.open_tenant(&scope_a).await.expect("open A");
    store.open_tenant(&scope_b).await.expect("open B");

    // 1. Same localpart (`alice`) in two tenants -> distinct AccountIds, zero collision.
    let acct_a = AccountId::new_v4();
    let acct_b = AccountId::new_v4();
    store
        .upsert_user(
            &scope_a,
            &TenantUserRecord {
                account_id: acct_a,
                jid: "alice@tenant-a.example".to_owned(),
                username: "alice".to_owned(),
                xmpp_localpart: "alice".to_owned(),
                display_name: Some("Alice A".to_owned()),
            },
        )
        .await
        .expect("upsert alice A");
    store
        .upsert_user(
            &scope_b,
            &TenantUserRecord {
                account_id: acct_b,
                jid: "alice@tenant-b.example".to_owned(),
                username: "alice".to_owned(),
                xmpp_localpart: "alice".to_owned(),
                display_name: Some("Alice B".to_owned()),
            },
        )
        .await
        .expect("upsert alice B");

    let got_a = store
        .get_user_by_localpart(&scope_a, "alice")
        .await
        .expect("get A")
        .expect("alice A exists");
    let got_b = store
        .get_user_by_localpart(&scope_b, "alice")
        .await
        .expect("get B")
        .expect("alice B exists");
    assert_eq!(got_a.account_id, acct_a);
    assert_eq!(got_b.account_id, acct_b);
    assert_ne!(got_a.account_id, got_b.account_id);
    assert_eq!(got_a.display_name.as_deref(), Some("Alice A"));
    assert_eq!(got_b.display_name.as_deref(), Some("Alice B"));

    // Immutable account_id trigger must reject mutation.
    let mut_err = store
        .update_user_account_id_for_test(&scope_a, "alice@tenant-a.example", AccountId::new_v4())
        .await
        .expect_err("account_id mutation must be rejected by trigger");
    assert!(
        mut_err.to_string().contains("immutable"),
        "expected immutable error, got: {mut_err}"
    );

    // 2. Same OIDC (issuer, subject) across tenants -> isolated.
    store
        .upsert_auth_identity(
            &scope_a,
            &TenantAuthIdentityRecord {
                id: "id-a".to_owned(),
                user_jid: "alice@tenant-a.example".to_owned(),
                provider_id: "oidc".to_owned(),
                issuer: "https://idp.example".to_owned(),
                subject: "sub-123".to_owned(),
            },
        )
        .await
        .expect("auth identity A");
    store
        .upsert_auth_identity(
            &scope_b,
            &TenantAuthIdentityRecord {
                id: "id-b".to_owned(),
                user_jid: "alice@tenant-b.example".to_owned(),
                provider_id: "oidc".to_owned(),
                issuer: "https://idp.example".to_owned(),
                subject: "sub-123".to_owned(),
            },
        )
        .await
        .expect("auth identity B");
    assert_eq!(
        store
            .get_auth_identity(&scope_a, "https://idp.example", "sub-123")
            .await
            .expect("get auth A")
            .expect("exists")
            .user_jid,
        "alice@tenant-a.example"
    );
    assert_eq!(
        store
            .get_auth_identity(&scope_b, "https://idp.example", "sub-123")
            .await
            .expect("get auth B")
            .expect("exists")
            .user_jid,
        "alice@tenant-b.example"
    );

    // 3. Same space_node across tenants -> isolated.
    store
        .upsert_space(
            &scope_a,
            &TenantSpaceRecord {
                space_node: "general".to_owned(),
                space_jid: "general@spaces.tenant-a.example".to_owned(),
                name: "General A".to_owned(),
            },
        )
        .await
        .expect("space A");
    store
        .upsert_space(
            &scope_b,
            &TenantSpaceRecord {
                space_node: "general".to_owned(),
                space_jid: "general@spaces.tenant-b.example".to_owned(),
                name: "General B".to_owned(),
            },
        )
        .await
        .expect("space B");
    assert_eq!(
        store
            .get_space(&scope_a, "general")
            .await
            .expect("space A")
            .expect("exists")
            .name,
        "General A"
    );
    assert_eq!(
        store
            .get_space(&scope_b, "general")
            .await
            .expect("space B")
            .expect("exists")
            .name,
        "General B"
    );

    // 4. Same federated remote BareJid (`bob@remote.example`) across tenants -> isolated.
    store
        .record_remote_participant(
            &scope_a,
            &RemoteParticipantRecord {
                room_id: "lobby".to_owned(),
                remote_bare_jid: "bob@remote.example".to_owned(),
                affiliation: "owner".to_owned(),
            },
        )
        .await
        .expect("remote A");
    store
        .record_remote_participant(
            &scope_b,
            &RemoteParticipantRecord {
                room_id: "lobby".to_owned(),
                remote_bare_jid: "bob@remote.example".to_owned(),
                affiliation: "member".to_owned(),
            },
        )
        .await
        .expect("remote B");
    let remotes_a = store
        .list_remote_participants(&scope_a, "lobby")
        .await
        .expect("list A");
    let remotes_b = store
        .list_remote_participants(&scope_b, "lobby")
        .await
        .expect("list B");
    assert_eq!(remotes_a.len(), 1);
    assert_eq!(remotes_a[0].affiliation, "owner");
    assert_eq!(remotes_b.len(), 1);
    assert_eq!(remotes_b[0].affiliation, "member");

    // 5. Jobs queued under the same queue name stay tenant-isolated.
    store
        .enqueue_job(
            &scope_a,
            &TenantJobRecord {
                job_id: "job-1".to_owned(),
                queue: "outbox".to_owned(),
                payload_json: r#"{"tenant":"a"}"#.to_owned(),
                status: "pending".to_owned(),
            },
        )
        .await
        .expect("enqueue A");
    assert!(store
        .claim_next_job(&scope_b, "outbox")
        .await
        .expect("claim B")
        .is_none());
    let claimed_a = store
        .claim_next_job(&scope_a, "outbox")
        .await
        .expect("claim A")
        .expect("job A");
    assert_eq!(claimed_a.payload_json, r#"{"tenant":"a"}"#);

    // 6. Stale generation & mismatched holder rejected.
    let scope_a_gen2 = TenantQueryScope {
        tenant_id: tenant_a.clone(),
        holder: holder_b.clone(),
        placement_generation: FenceGeneration::new(2),
    };
    store
        .open_tenant(&scope_a_gen2)
        .await
        .expect("advance to gen 2");

    assert!(matches!(
        store.verify_write_fence(&scope_a).await,
        Err(TenantStoreError::StalePlacementGeneration { .. })
    ));
    let wrong_holder_gen2 = TenantQueryScope {
        tenant_id: tenant_a.clone(),
        holder: holder_a.clone(),
        placement_generation: FenceGeneration::new(2),
    };
    assert!(matches!(
        store.verify_write_fence(&wrong_holder_gen2).await,
        Err(TenantStoreError::UnauthorizedHolder(_))
    ));

    // 7. delete_tenant purges tenant A without affecting tenant B.
    store.delete_tenant(&tenant_a).await.expect("delete A");
    assert!(matches!(
        store.open_tenant(&scope_a_gen2).await,
        Err(TenantStoreError::TenantNotFound(_))
    ));
    assert_eq!(
        store
            .get_user_by_localpart(&scope_b, "alice")
            .await
            .expect("B intact")
            .expect("alice B still exists")
            .account_id,
        acct_b
    );
}

#[tokio::test]
async fn turso_tenant_store_satisfies_shared_storage_contract() {
    let dir = tempdir().expect("tempdir");
    let store = TursoTenantStore::new(
        dir.path(),
        TursoPoolConfig {
            rlimit_nofile_override: Some(64),
            reserved_fds: 16,
        },
        None,
    )
    .expect("turso store");

    run_shared_storage_contract(&store).await;

    // Verify .db, -wal, and -shm were purged for tenant-a.example.
    let db_path = dir.path().join("tenant-a.example.db");
    assert!(!db_path.exists());
    assert!(!dir.path().join("tenant-a.example.db-wal").exists());
    assert!(!dir.path().join("tenant-a.example.db-shm").exists());
}

#[tokio::test]
async fn postgres_compat_store_satisfies_shared_storage_contract() {
    let store = SqliteCompatPostgresStore::new_in_memory(None)
        .await
        .expect("compat store");
    run_shared_storage_contract(&store).await;
}

#[tokio::test]
async fn live_postgres_tenant_store_contract_when_env_configured() {
    let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        return;
    };
    let pool = sqlx::PgPool::connect(&url)
        .await
        .expect("connect WADDLE_TEST_POSTGRES_URL");
    let store = PostgresTenantStore::new(pool, None);
    store.ensure_schema().await.expect("ensure schema");
    let _ = store
        .delete_tenant(&TenantId::parse("tenant-a.example").expect("valid"))
        .await;
    let _ = store
        .delete_tenant(&TenantId::parse("tenant-b.example").expect("valid"))
        .await;
    run_shared_storage_contract(&store).await;
    let _ = store
        .delete_tenant(&TenantId::parse("tenant-b.example").expect("valid"))
        .await;
}

#[tokio::test]
async fn turso_lru_fd_pool_enforces_rlimit_and_checkpoints_on_eviction() {
    let dir = tempdir().expect("tempdir");
    // (16 - 10) / 3 = 2 open Turso WAL DBs max.
    let store = TursoTenantStore::new(
        dir.path(),
        TursoPoolConfig {
            rlimit_nofile_override: Some(16),
            reserved_fds: 10,
        },
        None,
    )
    .expect("turso store");
    assert_eq!(store.pool_capacity(), 2);

    let t1 = TenantId::parse("t1.example").expect("valid");
    let t2 = TenantId::parse("t2.example").expect("valid");
    let t3 = TenantId::parse("t3.example").expect("valid");
    let holder = make_holder(
        "spiffe://waddle.internal/ns/cells/sa/shared-pool",
        [0x55; 32],
    );

    for t in [&t1, &t2, &t3] {
        store.provision_tenant(t).await.expect("provision");
        let scope = TenantQueryScope {
            tenant_id: t.clone(),
            holder: holder.clone(),
            placement_generation: FenceGeneration::new(1),
        };
        store.open_tenant(&scope).await.expect("open");
    }

    // Write data into t1 so WAL has real content before eviction.
    let scope1 = TenantQueryScope {
        tenant_id: t1.clone(),
        holder: holder.clone(),
        placement_generation: FenceGeneration::new(1),
    };
    let acct1 = AccountId::new_v4();
    store
        .upsert_user(
            &scope1,
            &TenantUserRecord {
                account_id: acct1,
                jid: "alice@t1.example".to_owned(),
                username: "alice".to_owned(),
                xmpp_localpart: "alice".to_owned(),
                display_name: Some("Persisted Across Eviction".to_owned()),
            },
        )
        .await
        .expect("upsert t1");

    // Pin t1 and t2 simultaneously -> pool is 100% pinned.
    let pin1 = store.acquire_pinned_conn(&t1).await.expect("pin t1");
    let pin2 = store.acquire_pinned_conn(&t2).await.expect("pin t2");

    // Opening t3 while both slots are pinned must fail with FdPoolExhausted { limit: 2 }.
    let err = store
        .acquire_pinned_conn(&t3)
        .await
        .err()
        .expect("pool exhausted");
    assert!(matches!(
        err,
        TenantStoreError::FdPoolExhausted { limit: 2 }
    ));

    // Drop pin1 -> t1 becomes LRU unpinned; opening t3 checkpoints & evicts t1 cleanly.
    drop(pin1);
    let pin3 = store
        .acquire_pinned_conn(&t3)
        .await
        .expect("t3 succeeds after eviction");
    let (open_count, pinned_count, evictions) = store.pool_stats().await;
    assert_eq!(open_count, 2);
    assert_eq!(pinned_count, 2);
    assert!(evictions >= 1);

    drop(pin2);
    drop(pin3);

    // Re-opening t1 proves WAL checkpoint persisted all data across eviction.
    let loaded = store
        .get_user_by_localpart(&scope1, "alice")
        .await
        .expect("reload t1")
        .expect("alice persisted");
    assert_eq!(loaded.account_id, acct1);
}

#[tokio::test]
async fn tenant_registry_is_sole_handle_constructor_with_scoped_cache() {
    let dir = tempdir().expect("tempdir");
    let store = Arc::new(
        TursoTenantStore::new(
            dir.path(),
            TursoPoolConfig {
                rlimit_nofile_override: Some(64),
                reserved_fds: 16,
            },
            None,
        )
        .expect("turso store"),
    );
    let registry =
        TenantRegistry::new(store, None, TenantResourceQuotas::default()).expect("registry");

    let t1 = TenantId::parse("alpha.example").expect("valid");
    let t2 = TenantId::parse("beta.example").expect("valid");
    let holder = make_holder(
        "spiffe://waddle.internal/ns/cells/sa/shared-pool",
        [0x99; 32],
    );

    registry.provision_tenant(&t1).await.expect("provision t1");
    registry.provision_tenant(&t2).await.expect("provision t2");

    let scope1 = TenantQueryScope {
        tenant_id: t1.clone(),
        holder: holder.clone(),
        placement_generation: FenceGeneration::new(1),
    };
    let guard1 = registry.acquire_tenant(&scope1).await.expect("guard t1");
    assert_eq!(guard1.tenant_id(), &t1);

    registry
        .metadata_cache()
        .insert(t1.clone(), "bob@remote.example".to_owned(), "v1".to_owned());
    registry
        .metadata_cache()
        .insert(t2.clone(), "bob@remote.example".to_owned(), "v2".to_owned());

    registry.evict_tenant(&t1).await.expect("evict t1");
    assert_eq!(
        registry
            .metadata_cache()
            .get_cloned(&t1, &"bob@remote.example".to_owned()),
        None
    );
    assert_eq!(
        registry
            .metadata_cache()
            .get_cloned(&t2, &"bob@remote.example".to_owned())
            .as_deref(),
        Some("v2")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_cold_open_coalesces_without_fd_pool_leak() {
    let dir = tempdir().expect("tempdir");
    // (22 - 10) / 3 = 4 open Turso DB slots.
    let store = Arc::new(
        TursoTenantStore::new(
            dir.path(),
            TursoPoolConfig {
                rlimit_nofile_override: Some(22),
                reserved_fds: 10,
            },
            None,
        )
        .expect("turso store"),
    );
    assert_eq!(store.pool_capacity(), 4);

    let cold = TenantId::parse("cold-race.example").expect("valid");
    let t2 = TenantId::parse("peer-2.example").expect("valid");
    let t3 = TenantId::parse("peer-3.example").expect("valid");
    let t4 = TenantId::parse("peer-4.example").expect("valid");
    for tenant in [&cold, &t2, &t3, &t4] {
        store.provision_tenant(tenant).await.expect("provision");
    }

    // Spawn 8 concurrent tasks opening the exact same uncached tenant on a capacity-4 pool.
    let barrier = Arc::new(tokio::sync::Barrier::new(8));
    let mut join_handles = Vec::with_capacity(8);
    for _ in 0..8 {
        let store = Arc::clone(&store);
        let tenant = cold.clone();
        let barrier = Arc::clone(&barrier);
        join_handles.push(tokio::spawn(async move {
            barrier.wait().await;
            store.acquire_pinned_conn(&tenant).await
        }));
    }

    let mut pinned_conns = Vec::with_capacity(8);
    for handle in join_handles {
        let conn = handle
            .await
            .expect("task join")
            .expect("all 8 concurrent cold opens must succeed without FdPoolExhausted");
        pinned_conns.push(conn);
    }

    let (open_held, pinned_held, _) = store.pool_stats().await;
    assert_eq!(
        open_held, 1,
        "coalesced cold open must occupy only 1 open slot"
    );
    assert_eq!(pinned_held, 1, "all 8 guards share 1 pinned pool slot");

    drop(pinned_conns);
    // Allow any async fallback drop task to release pins if mutex was contended during drop.
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let (open_after, pinned_after, _) = store.pool_stats().await;
    assert_eq!(open_after, 1, "cold tenant remains cached as 1 open slot");
    assert_eq!(
        pinned_after, 0,
        "zero FD pool pins must remain after all guards drop"
    );

    // 3 more distinct tenants can still be opened and held simultaneously without FdPoolExhausted.
    let p2 = store.acquire_pinned_conn(&t2).await.expect("open t2");
    let p3 = store.acquire_pinned_conn(&t3).await.expect("open t3");
    let p4 = store.acquire_pinned_conn(&t4).await.expect("open t4");
    let (open_four, pinned_four, _) = store.pool_stats().await;
    assert_eq!(open_four, 4);
    assert_eq!(pinned_four, 3);
    drop((p2, p3, p4));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn turso_concurrent_claim_next_job_claims_all_jobs_uniquely_without_errors() {
    let dir = tempdir().expect("tempdir");
    let store = Arc::new(
        TursoTenantStore::new(
            dir.path(),
            TursoPoolConfig {
                rlimit_nofile_override: Some(64),
                reserved_fds: 16,
            },
            None,
        )
        .expect("turso store"),
    );

    let tenant = TenantId::parse("jobs-race.example").expect("valid");
    let holder = make_holder(
        "spiffe://waddle.internal/ns/cells/sa/shared-pool",
        [0x55; 32],
    );
    let scope = TenantQueryScope {
        tenant_id: tenant.clone(),
        holder,
        placement_generation: FenceGeneration::new(1),
    };

    store.provision_tenant(&tenant).await.expect("provision");
    store.open_tenant(&scope).await.expect("open");

    const JOB_COUNT: usize = 20;
    const WORKER_COUNT: usize = 8;

    for i in 0..JOB_COUNT {
        store
            .enqueue_job(
                &scope,
                &TenantJobRecord {
                    job_id: format!("job-{i:02}"),
                    queue: "q1".to_owned(),
                    payload_json: format!(r#"{{"index":{i}}}"#),
                    status: "pending".to_owned(),
                },
            )
            .await
            .expect("enqueue job");
    }

    let barrier = Arc::new(tokio::sync::Barrier::new(WORKER_COUNT));
    let mut handles = Vec::with_capacity(WORKER_COUNT);
    for _ in 0..WORKER_COUNT {
        let store = Arc::clone(&store);
        let scope = scope.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            let mut claimed = Vec::new();
            loop {
                match store.claim_next_job(&scope, "q1").await {
                    Ok(Some(job)) => claimed.push(job.job_id),
                    Ok(None) => break,
                    Err(err) => panic!("concurrent claim_next_job failed: {err}"),
                }
            }
            claimed
        }));
    }

    let mut all_claimed = Vec::with_capacity(JOB_COUNT);
    for handle in handles {
        all_claimed.extend(handle.await.expect("worker join"));
    }

    let unique: HashSet<String> = all_claimed.iter().cloned().collect();
    assert_eq!(
        all_claimed.len(),
        JOB_COUNT,
        "all {JOB_COUNT} jobs must be claimed across {WORKER_COUNT} workers"
    );
    assert_eq!(
        unique.len(),
        JOB_COUNT,
        "every job must be claimed uniquely (exactly once)"
    );

    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let (_, pinned, _) = store.pool_stats().await;
    assert_eq!(pinned, 0, "no pins may remain after all workers finish");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_cold_open_neither_leaks_fd_pool_nor_livelocks_waiters() {
    let dir = tempdir().expect("tempdir");
    // (16 - 10) / 3 = 2 open Turso DB slots.
    let store = Arc::new(
        TursoTenantStore::new(
            dir.path(),
            TursoPoolConfig {
                rlimit_nofile_override: Some(16),
                reserved_fds: 10,
            },
            None,
        )
        .expect("turso store"),
    );
    assert_eq!(store.pool_capacity(), 2);

    let target = TenantId::parse("cancel-target.example").expect("valid");
    let peer_a = TenantId::parse("cancel-peer-a.example").expect("valid");
    let peer_b = TenantId::parse("cancel-peer-b.example").expect("valid");
    for tenant in [&target, &peer_a, &peer_b] {
        store.provision_tenant(tenant).await.expect("provision");
    }

    // Repeatedly abort/drop cold-open leaders while a waiter races on the same tenant.
    for round in 0..16u64 {
        let _ = store.checkpoint_and_release(&target).await;

        let leader_store = Arc::clone(&store);
        let leader_tenant = target.clone();
        let leader = tokio::spawn(async move {
            let _ = leader_store.acquire_pinned_conn(&leader_tenant).await;
        });

        let waiter_store = Arc::clone(&store);
        let waiter_tenant = target.clone();
        let waiter = tokio::spawn(async move {
            if round % 2 == 0 {
                tokio::task::yield_now().await;
            }
            waiter_store.acquire_pinned_conn(&waiter_tenant).await
        });

        // Also exercise dropping an in-flight future directly via select!.
        {
            let fut = store.acquire_pinned_conn(&target);
            tokio::select! {
                biased;
                _ = tokio::task::yield_now() => {}
                res = fut => {
                    drop(res);
                }
            }
        }

        // Abort the leader task mid-open.
        leader.abort();
        let _ = leader.await;

        // Waiter MUST complete promptly without livelocking on a dead watch channel.
        let waiter_conn = tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("waiter must not livelock after leader cancellation")
            .expect("waiter join")
            .expect("waiter must acquire pinned conn cleanly");
        drop(waiter_conn);

        // Subsequent open for the same tenant must also succeed without hanging.
        let followup =
            tokio::time::timeout(Duration::from_secs(5), store.acquire_pinned_conn(&target))
                .await
                .expect("follow-up open must not livelock")
                .expect("follow-up open must succeed");
        drop(followup);

        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        let (_, pinned, _) = store.pool_stats().await;
        assert_eq!(pinned, 0, "round {round}: pinned must return to 0");
    }

    // Release target and prove both slots of the capacity-2 pool remain usable
    // simultaneously (zero leaked open/pinned FD slots).
    store
        .checkpoint_and_release(&target)
        .await
        .expect("release target");
    let p_a = store
        .acquire_pinned_conn(&peer_a)
        .await
        .expect("open peer_a");
    let p_b = store
        .acquire_pinned_conn(&peer_b)
        .await
        .expect("open peer_b");
    let (open_both, pinned_both, _) = store.pool_stats().await;
    assert_eq!(open_both, 2, "both FD pool slots must be available");
    assert_eq!(pinned_both, 2, "both peer slots must be pinned");
    drop((p_a, p_b));

    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let (_, pinned_final, _) = store.pool_stats().await;
    assert_eq!(pinned_final, 0, "zero pins must remain after cleanup");
}
