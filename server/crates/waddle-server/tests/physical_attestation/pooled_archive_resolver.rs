use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use waddle_server::config::{IngressConfig, LineageConfig};
use waddle_server::db::{DarkMigrationOwnerConfig, DatabasePool};
use waddle_server::pending_delivery::{
    flush_for_resource, ArchiveResolveError, ArchiveResolver, FlushContext, FlushOutcome,
    PendingDispatchGate, PendingDispatchReadiness,
};
use waddle_server::test_support::SharedPendingResolverFixture;
use waddle_xmpp::pending_delivery::storage::PendingStorageError;
use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow, PendingRowId, SmSessionId};
use waddle_xmpp::registry::ConnectionRegistry;
use waddle_xmpp_core::xep0359::StanzaId;

struct ReadyGate(AtomicUsize);

#[async_trait::async_trait]
impl PendingDispatchGate for ReadyGate {
    async fn check_turn(
        &self,
        _row: &PendingRow,
        _resource: &jid::FullJid,
        _stream: Option<&SmSessionId>,
    ) -> Result<PendingDispatchReadiness, PendingStorageError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(PendingDispatchReadiness::Ready)
    }
}

struct PausedResolver {
    inner: Arc<SharedPendingResolverFixture>,
    entered: Arc<Notify>,
    proceed: Arc<Notify>,
}

struct StalledRollbackResolver {
    inner: Arc<SharedPendingResolverFixture>,
    entered: Arc<Notify>,
    proceed: Arc<Notify>,
}

#[async_trait::async_trait]
impl ArchiveResolver for StalledRollbackResolver {
    async fn resolve(
        &self,
        stanza: &StanzaId,
    ) -> Result<Option<xmpp_parsers::message::Message>, ArchiveResolveError> {
        self.inner
            .resolve_with_rollback_gate(
                stanza,
                Arc::clone(&self.entered),
                Arc::clone(&self.proceed),
                Duration::from_secs(2),
            )
            .await
    }
}

#[async_trait::async_trait]
impl ArchiveResolver for PausedResolver {
    async fn resolve(
        &self,
        stanza: &StanzaId,
    ) -> Result<Option<xmpp_parsers::message::Message>, ArchiveResolveError> {
        self.entered.notify_one();
        self.proceed.notified().await;
        self.inner.resolve(stanza).await
    }
}

fn row(id: &str, stamp: &StanzaId) -> PendingRow {
    PendingRow {
        id: PendingRowId::new(id),
        recipient: stamp.by.to_bare(),
        original_receipt_at: chrono::Utc::now(),
        payload: PendingPayload::Archived(stamp.clone()),
        flushed_in_session: None,
        outbound_sequence: None,
    }
}

fn archived_xml(body: &str) -> String {
    let mut message =
        xmpp_parsers::message::Message::new(Some("bob@example.test".parse().expect("recipient")));
    message.from = Some("alice@example.test/phone".parse().expect("sender"));
    message.bodies.insert(Default::default(), body.to_owned());
    waddle_xmpp_core::xep0359::add_stanza_id(
        &mut message,
        &StanzaId::new(body, "bob@example.test".parse().expect("archive")),
    );
    String::from(&xmpp_parsers::minidom::Element::from(message))
}

async fn flush<R: ArchiveResolver + ?Sized>(
    fixture: &SharedPendingResolverFixture,
    resolver: &R,
    registry: &ConnectionRegistry,
    resource: &jid::FullJid,
    owner: &Arc<std::sync::atomic::AtomicBool>,
    gate: &ReadyGate,
) -> FlushOutcome {
    let session = SmSessionId::new("pool-resolver-stream");
    flush_for_resource(
        fixture.pending(),
        registry,
        &resource.to_bare(),
        resource,
        FlushContext {
            server_domain: "example.test",
            sm_session: Some(&session),
            blocking_storage: None,
            owner: Some(owner),
            archive_resolver: resolver,
            dispatch_gate: Some(gate),
        },
    )
    .await
}

async fn clear(fixture: &SharedPendingResolverFixture, stamp: &StanzaId) {
    for row in fixture
        .pending()
        .list(&stamp.by.to_bare())
        .await
        .expect("list")
    {
        fixture
            .pending()
            .delete_row(&row.id)
            .await
            .expect("cleanup");
    }
}

async fn lookup_cases(fixture: &SharedPendingResolverFixture, stamp: &StanzaId, origin: &str) {
    let by_full = StanzaId::new(
        stamp.id.clone(),
        "bob@example.test/device".parse().expect("jid"),
    );
    let resolved = fixture
        .resolve(&by_full)
        .await
        .expect("resolve")
        .expect("row");
    assert_eq!(
        resolved.bodies.values().next().map(String::as_str),
        Some(stamp.id.as_str())
    );
    let origin_only = StanzaId::new(origin, stamp.by.clone());
    assert!(fixture
        .resolve(&origin_only)
        .await
        .expect("origin lookup")
        .is_none());
    let missing = StanzaId::new("not-present", stamp.by.clone());
    assert!(fixture
        .resolve(&missing)
        .await
        .expect("missing lookup")
        .is_none());
    let wrong_archive = StanzaId::new(stamp.id.clone(), "eve@example.test".parse().expect("jid"));
    assert!(fixture
        .resolve(&wrong_archive)
        .await
        .expect("wrong archive lookup")
        .is_none());
}

async fn common(fixture: &Arc<SharedPendingResolverFixture>, stamp: &StanzaId, origin: &str) {
    lookup_cases(fixture, stamp, origin).await;
    let registry = Arc::new(ConnectionRegistry::new());
    let resource: jid::FullJid = "bob@example.test/laptop".parse().expect("resource");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let owner = registry.register(resource.clone(), tx);
    registry.update_presence(&resource, true, 0);
    let gate = ReadyGate(AtomicUsize::new(0));
    fixture
        .pending()
        .insert(row("resolver-success", stamp))
        .await
        .expect("pending row");
    let outcome = flush(
        fixture,
        fixture.as_ref(),
        &registry,
        &resource,
        &owner,
        &gate,
    )
    .await;
    assert_eq!(
        (outcome.claimed, outcome.pushed, outcome.unresolved),
        (1, 1, 0)
    );
    assert_eq!(gate.0.load(Ordering::SeqCst), 1);
    let outbound = rx.try_recv().expect("replayed archive");
    let waddle_xmpp::Stanza::Message(message) = outbound.stanza else {
        panic!("expected archived message");
    };
    assert_eq!(
        message.bodies.values().next().map(String::as_str),
        Some(stamp.id.as_str())
    );
    assert!(waddle_xmpp_core::xep0359::extract_stanza_ids(&message).contains(stamp));
    clear(fixture, stamp).await;

    let missing = StanzaId::new("not-present", stamp.by.clone());
    fixture
        .pending()
        .insert(row("resolver-poison", &missing))
        .await
        .expect("missing archive row");
    fixture
        .pending()
        .insert(row("resolver-after-poison", stamp))
        .await
        .expect("valid archive row");
    let outcome = flush(
        fixture,
        fixture.as_ref(),
        &registry,
        &resource,
        &owner,
        &gate,
    )
    .await;
    assert_eq!(
        (outcome.claimed, outcome.pushed, outcome.unresolved),
        (2, 1, 1)
    );
    assert!(rx.try_recv().is_ok());
    let retained = fixture
        .pending()
        .list(&stamp.by.to_bare())
        .await
        .expect("list");
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].id.as_str(), "resolver-after-poison");
    clear(fixture, stamp).await;

    // Replace the same full-JID owner after the claim commits and before the
    // lookup returns. The old generation cannot enqueue on either channel.
    fixture
        .pending()
        .insert(row("resolver-owner", stamp))
        .await
        .expect("pending row");
    let entered = Arc::new(Notify::new());
    let proceed = Arc::new(Notify::new());
    let paused = PausedResolver {
        inner: Arc::clone(fixture),
        entered: Arc::clone(&entered),
        proceed: Arc::clone(&proceed),
    };
    let running = {
        let fixture = Arc::clone(fixture);
        let registry = registry.clone();
        let resource = resource.clone();
        let owner = Arc::clone(&owner);
        tokio::spawn(async move {
            let gate = ReadyGate(AtomicUsize::new(0));
            let outcome = flush(&fixture, &paused, &registry, &resource, &owner, &gate).await;
            (outcome, gate.0.load(Ordering::SeqCst))
        })
    };
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("claim completed");
    let (replacement, mut replacement_rx) = tokio::sync::mpsc::channel(8);
    registry.register(resource.clone(), replacement);
    proceed.notify_one();
    let (outcome, checks) = running.await.expect("flush task");
    assert_eq!(checks, 1, "the archived dispatch gate was checked");
    assert_eq!(outcome.pushed, 0);
    assert!(rx.try_recv().is_err() && replacement_rx.try_recv().is_err());
    let retained = fixture
        .pending()
        .list(&stamp.by.to_bare())
        .await
        .expect("list");
    assert_eq!(retained.len(), 1);
    assert!(retained[0].flushed_in_session.is_none());
    clear(fixture, stamp).await;
}

pub(super) async fn exercise_sqlite(pool: &DatabasePool, stamp: &StanzaId) {
    let guard = pool.global().guard().await.expect("seed archive XML");
    for id in ["sqlite-archive", "sqlite-second"] {
        guard
            .execute(
                "UPDATE main.mam_messages SET stanza_xml = ? WHERE id = ?",
                waddle_server::db_params![archived_xml(id), id],
            )
            .await
            .expect("seed archive XML");
    }
    drop(guard);
    let fixture = Arc::new(
        SharedPendingResolverFixture::from_pool(
            pool,
            &IngressConfig {
                pool_size: 1,
                retry_attempts: 2,
            },
            LineageConfig::default(),
            None,
        )
        .await
        .expect("same-pool fixture"),
    );
    lookup_cases(&fixture, stamp, "sqlite-origin").await;

    let guard = pool.global().guard().await.expect("seed aliases");
    guard.execute(
        "UPDATE mam_messages SET stanza_id = 'shared-wire-id' WHERE id IN ('sqlite-archive','sqlite-second')",
        (),
    ).await.expect("duplicate wire identifier");
    drop(guard);
    let shared = StanzaId::new("shared-wire-id", stamp.by.clone());
    let newest = fixture
        .resolve(&shared)
        .await
        .expect("lookup")
        .expect("newest");
    assert_eq!(
        newest.bodies.values().next().map(String::as_str),
        Some("sqlite-second")
    );
    let guard = pool.global().guard().await.expect("corrupt archive");
    guard
        .execute(
            "UPDATE mam_messages SET parent_thread_id='orphan' WHERE id='sqlite-archive'",
            (),
        )
        .await
        .expect("corrupt row");
    drop(guard);
    assert!(fixture
        .resolve(stamp)
        .await
        .expect("permanent decode failure")
        .is_none());
    let guard = pool.global().guard().await.expect("restore archive");
    guard
        .execute(
            "UPDATE mam_messages SET parent_thread_id=NULL WHERE id='sqlite-archive'",
            (),
        )
        .await
        .expect("restore row");
    drop(guard);

    let guard = pool.global().guard().await.expect("SQLite TEMP shadow");
    guard
        .execute("CREATE TEMP TABLE mam_messages (id TEXT)", ())
        .await
        .expect("create shadow");
    drop(guard);
    assert!(fixture
        .resolve(stamp)
        .await
        .expect("main-qualified lookup")
        .is_some());
    let guard = pool.global().guard().await.expect("SQLite TEMP shadow");
    guard
        .execute("DROP TABLE temp.mam_messages", ())
        .await
        .expect("drop shadow");
    drop(guard);

    // A short PostgreSQL test deadline must not cancel the only SQLite
    // connection while it is in flight.
    let entered = Arc::new(Notify::new());
    let proceed = Arc::new(Notify::new());
    let mut in_flight = {
        let fixture = Arc::clone(&fixture);
        let stamp = stamp.clone();
        let entered = Arc::clone(&entered);
        let proceed = Arc::clone(&proceed);
        tokio::spawn(async move {
            fixture
                .resolve_with_rollback_gate(&stamp, entered, proceed, Duration::from_millis(20))
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .expect("SQLite read reached gate");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut in_flight)
            .await
            .is_err()
    );
    proceed.notify_one();
    assert!(in_flight
        .await
        .expect("SQLite task")
        .expect("lookup")
        .is_some());
    let guard = pool.global().guard().await.expect("hide SQLite MAM");
    guard
        .execute(
            "ALTER TABLE main.mam_messages RENAME TO hidden_mam_messages",
            (),
        )
        .await
        .expect("hide relation");
    drop(guard);
    assert!(matches!(
        fixture.resolve(stamp).await,
        Err(ArchiveResolveError::Storage(
            waddle_xmpp::mam::MamStorageError::Database(_)
        ))
    ));
    let guard = pool.global().guard().await.expect("restore SQLite MAM");
    guard
        .execute(
            "ALTER TABLE main.hidden_mam_messages RENAME TO mam_messages",
            (),
        )
        .await
        .expect("restore relation");
    drop(guard);

    let held = pool
        .global()
        .begin()
        .await
        .expect("occupy SQLite connection");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), fixture.resolve(stamp))
            .await
            .is_err()
    );
    drop(held);
    assert!(
        tokio::time::timeout(Duration::from_secs(3), fixture.resolve(stamp))
            .await
            .expect("connection returned")
            .expect("lookup recovered")
            .is_some()
    );
}

pub(super) async fn exercise_postgres(
    pool: &DatabasePool,
    owner: &DarkMigrationOwnerConfig,
    config: &IngressConfig,
    lineage: &LineageConfig,
    db: &super::Fixture,
    stamp: &StanzaId,
) {
    for id in ["pg-archive", "lost-archive"] {
        sqlx::query(&format!(
            "UPDATE {}.mam_messages SET stanza_xml=$1 WHERE id=$2",
            db.target
        ))
        .bind(archived_xml(id))
        .bind(id)
        .execute(&db.admin)
        .await
        .expect("seed archive XML");
    }
    let fixture = Arc::new(
        SharedPendingResolverFixture::from_pool(pool, config, lineage.clone(), Some(owner))
            .await
            .expect("same-pool fixture"),
    );
    common(&fixture, stamp, "pg-origin").await;

    sqlx::query(&format!("UPDATE {}.mam_messages SET stanza_id='shared-wire-id' WHERE id IN ('pg-archive','lost-archive')", db.target))
        .execute(&db.admin).await.expect("duplicate wire identifier");
    let shared = StanzaId::new("shared-wire-id", stamp.by.clone());
    let newest = fixture
        .resolve(&shared)
        .await
        .expect("lookup")
        .expect("newest");
    assert_eq!(
        newest.bodies.values().next().map(String::as_str),
        Some("lost-archive")
    );
    sqlx::query(&format!(
        "UPDATE {}.mam_messages SET parent_thread_id='orphan' WHERE id='pg-archive'",
        db.target
    ))
    .execute(&db.admin)
    .await
    .expect("corrupt row");
    assert!(fixture
        .resolve(stamp)
        .await
        .expect("permanent decode failure")
        .is_none());
    let entered = Arc::new(Notify::new());
    let proceed = Arc::new(Notify::new());
    let rollback = {
        let fixture = Arc::clone(&fixture);
        let stamp = stamp.clone();
        let entered = Arc::clone(&entered);
        let proceed = Arc::clone(&proceed);
        tokio::spawn(async move {
            fixture
                .resolve_with_rollback_gate(&stamp, entered, proceed, Duration::from_secs(15))
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .expect("read completed before rollback");
    let pid: i32 = sqlx::query_scalar(
        "SELECT pid FROM pg_catalog.pg_stat_activity WHERE datname=$1 AND usename=$2 AND state='idle in transaction' AND query LIKE '%mam_messages%'",
    ).bind(&db.database_name).bind(&db.runtime).fetch_one(&db.admin).await.expect("identify read backend");
    let killed: bool = sqlx::query_scalar("SELECT pg_catalog.pg_terminate_backend($1)")
        .bind(pid)
        .fetch_one(&db.admin)
        .await
        .expect("terminate read backend");
    assert!(killed);
    proceed.notify_one();
    assert!(
        matches!(
            rollback.await.expect("resolver task"),
            Err(ArchiveResolveError::Storage(
                waddle_xmpp::mam::MamStorageError::Database(_)
            ))
        ),
        "rollback failure must override a permanent decode error"
    );
    sqlx::query(&format!(
        "UPDATE {}.mam_messages SET parent_thread_id=NULL WHERE id='pg-archive'",
        db.target
    ))
    .execute(&db.admin)
    .await
    .expect("restore row");

    let other_stamp = StanzaId::new("lost-archive", stamp.by.clone());
    for (id, archived) in [
        ("resolver-failure-a", stamp),
        ("resolver-failure-b", &other_stamp),
    ] {
        fixture
            .pending()
            .insert(row(id, archived))
            .await
            .expect("pending row");
    }
    let registry = Arc::new(ConnectionRegistry::new());
    let resource: jid::FullJid = "bob@example.test/laptop".parse().expect("resource");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let token = registry.register(resource.clone(), tx);
    registry.update_presence(&resource, true, 0);
    let entered = Arc::new(Notify::new());
    let proceed = Arc::new(Notify::new());
    let paused = PausedResolver {
        inner: Arc::clone(&fixture),
        entered: Arc::clone(&entered),
        proceed: Arc::clone(&proceed),
    };
    let running = {
        let fixture = Arc::clone(&fixture);
        let registry = registry.clone();
        let resource = resource.clone();
        tokio::spawn(async move {
            let gate = ReadyGate(AtomicUsize::new(0));
            flush(&fixture, &paused, &registry, &resource, &token, &gate).await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("claim committed");
    sqlx::query(&format!(
        "REVOKE SELECT ON {}.mam_messages FROM {}",
        db.target, db.runtime
    ))
    .execute(&db.admin)
    .await
    .expect("revoke MAM lookup");
    proceed.notify_one();
    let outcome = running.await.expect("flush task");
    assert_eq!(
        (
            outcome.claimed,
            outcome.pushed,
            outcome.unresolved,
            outcome.deferred_transient
        ),
        (2, 0, 0, 2)
    );
    assert!(rx.try_recv().is_err());
    let retained = fixture
        .pending()
        .list(&stamp.by.to_bare())
        .await
        .expect("list");
    assert_eq!(retained.len(), 2);
    assert!(retained.iter().all(|row| row.flushed_in_session.is_none()));
    sqlx::query(&format!(
        "GRANT SELECT ON {}.mam_messages TO {}",
        db.target, db.runtime
    ))
    .execute(&db.admin)
    .await
    .expect("restore MAM lookup");
    clear(&fixture, stamp).await;

    for (id, archived) in [
        ("resolver-timeout-a", stamp),
        ("resolver-timeout-b", &other_stamp),
    ] {
        fixture
            .pending()
            .insert(row(id, archived))
            .await
            .expect("pending row");
    }
    let registry = Arc::new(ConnectionRegistry::new());
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let token = registry.register(resource.clone(), tx);
    registry.update_presence(&resource, true, 0);
    let entered = Arc::new(Notify::new());
    let stalled = StalledRollbackResolver {
        inner: Arc::clone(&fixture),
        entered: Arc::clone(&entered),
        proceed: Arc::new(Notify::new()),
    };
    let running = {
        let fixture = Arc::clone(&fixture);
        let registry = Arc::clone(&registry);
        let resource = resource.clone();
        tokio::spawn(async move {
            let gate = ReadyGate(AtomicUsize::new(0));
            flush(&fixture, &stalled, &registry, &resource, &token, &gate).await
        })
    };
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .expect("lookup reached stalled rollback after claim");
    let outcome = tokio::time::timeout(Duration::from_secs(6), running)
        .await
        .expect("deadline and responsive cleanup")
        .expect("flush task");
    assert_eq!(
        (
            outcome.claimed,
            outcome.pushed,
            outcome.unresolved,
            outcome.deferred_transient
        ),
        (2, 0, 0, 2)
    );
    assert!(rx.try_recv().is_err());
    let retained = fixture
        .pending()
        .list(&stamp.by.to_bare())
        .await
        .expect("list");
    assert_eq!(retained.len(), 2);
    assert!(retained.iter().all(|row| row.flushed_in_session.is_none()));
    clear(&fixture, stamp).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(3), fixture.resolve(stamp))
            .await
            .expect("deadline cleanup returned permit")
            .expect("lookup recovered")
            .is_some()
    );

    let ingress = pool.ingress();
    let ingress_hold = ingress.begin().await.expect("occupy ingress");
    assert!(fixture
        .resolve(stamp)
        .await
        .expect("main lane available")
        .is_some());
    drop(ingress_hold);
    let main_hold = pool.global().begin().await.expect("occupy main");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), fixture.resolve(stamp))
            .await
            .is_err()
    );
    let reserved = ingress.begin().await.expect("ingress lane available");
    drop(reserved);
    drop(main_hold);
    assert!(
        tokio::time::timeout(Duration::from_secs(3), fixture.resolve(stamp))
            .await
            .expect("permit returned")
            .expect("lookup recovered")
            .is_some()
    );

    let mut blocker = db.admin.begin().await.expect("lock holder");
    sqlx::query(&format!(
        "LOCK TABLE {}.mam_messages IN ACCESS EXCLUSIVE MODE",
        db.target
    ))
    .execute(&mut *blocker)
    .await
    .expect("block lookup");
    let running = {
        let fixture = Arc::clone(&fixture);
        let stamp = stamp.clone();
        tokio::spawn(async move { fixture.resolve(&stamp).await })
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname=$1 AND usename=$2 AND wait_event_type='Lock' AND query LIKE '%mam_messages%')",
            ).bind(&db.database_name).bind(&db.runtime).fetch_one(&db.admin).await.expect("observe lookup");
            if waiting { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("lookup is waiting after checkout");
    running.abort();
    assert!(running.await.expect_err("cancelled lookup").is_cancelled());
    blocker.rollback().await.expect("release MAM lock");
    assert!(
        tokio::time::timeout(Duration::from_secs(3), fixture.resolve(stamp))
            .await
            .expect("cancelled permit returned")
            .expect("lookup recovered")
            .is_some()
    );
}
