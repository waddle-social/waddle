use super::{
    requeue_failed_accounting, run_maintenance_pass, run_maintenance_pass_with_cursor,
    MaintenanceBudget, MaintenanceCursor, MaintenanceOutcome, RECOVERY_ACCOUNTING_QUEUE_BOUND,
};
use crate::{
    db::DatabaseDriver,
    ingress::{
        commit::commit_submission,
        effects::{
            delivery::{ExternalDeliveryEffect, PeerDeliveryKind},
            Effect,
        },
        execute::{execute_effects, test_hooks},
        gc::{run_retention_gc_coordinator, RetentionGcCoordinator},
        test_support::IngressFixture,
        Deps, ExternalEffect, ExternalOutcome, ImmediateSink, PlannedEffect,
    },
    ingress_uow::{CanonicalMessageRepository, EffectReceiptRepository},
};
use std::{sync::Arc, time::Duration};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use waddle_xmpp::{
    ingress::{EffectMessageIdentity, IngressEffectIntent, MessageKey},
    registry::ConnectionRegistry,
    Stanza,
};

fn immediate_budget() -> MaintenanceBudget {
    MaintenanceBudget {
        grace: chrono::Duration::zero(),
        recovery_stall_sample_interval: Duration::ZERO,
        ..MaintenanceBudget::DEFAULT
    }
}

/// Drive real Phase B and Phase C, pause after the receipt commit, then fail
/// only the final terminalization. No raw receipt rows are fabricated.
async fn interrupted_delivery(fixture: &IngressFixture, origin: &str) -> MessageKey {
    let mut submission = fixture.submission(Some(origin), "maintenance delivery");
    let resource: jid::FullJid = "juliet@example.com/phone".parse().expect("resource");
    let identity = EffectMessageIdentity::capture_ordinal(0);
    submission
        .plan
        .intents
        .push(IngressEffectIntent::RouteDirect {
            recipient: resource.to_bare(),
            fanout: vec![resource.clone()],
            route_identity: identity.clone(),
        });
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::External(
            ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
                route_identity: Some(identity),
                jid: resource.clone(),
                stanza: Box::new(Stanza::Message(submission.plan.sanitized_message.clone())),
                kind: PeerDeliveryKind::RegistryFrame,
                call_setup: None,
            }),
        )));
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit delivery");
    let key = decision.message_key.expect("canonical key");
    let registry = ConnectionRegistry::new();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
    registry.register(resource, sender);
    let deps = Deps::new(&registry, "example.com");
    let gate = test_hooks::pause_before_terminalization(key);
    let execution = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    );
    let inspect = async {
        tokio::time::timeout(Duration::from_secs(5), gate.wait_until_reached())
            .await
            .expect("receipt barrier");
        let mut tx = fixture.uow.begin().await.expect("receipt inspection");
        assert!(EffectReceiptRepository::receipts_complete(&mut tx, key)
            .await
            .expect("durable receipts"));
        tx.commit().await.expect("inspection commit");
        assert_eq!(
            fixture
                .count(&format!(
                    "ingress_messages WHERE terminal_at IS NULL AND message_key = '{}'",
                    key.to_storage()
                ))
                .await,
            1,
            "the barrier exposes the receipt-complete non-terminal window"
        );
        assert!(
            receiver.try_recv().is_ok(),
            "the actual delivery preceded settlement"
        );
        test_hooks::force_terminalization_timeout_once(key);
        gate.release();
    };
    let (report, ()) = tokio::join!(execution, inspect);
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    assert!(report.receipt_failures.is_empty());
    assert!(report.terminalization_failure.is_some());
    key
}

async fn terminal_count(fixture: &IngressFixture) -> i64 {
    fixture
        .count("ingress_messages WHERE terminal_at IS NOT NULL")
        .await
}

async fn backdate_created(fixture: &IngressFixture, key: MessageKey, seconds: i64) {
    let sql = match fixture.db.driver() {
        DatabaseDriver::Postgres => "UPDATE ingress_messages SET created_at = ?::timestamptz WHERE message_key = ?::uuid",
        DatabaseDriver::Sqlite => "UPDATE ingress_messages SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', ?) WHERE message_key = ?",
    };
    fixture
        .execute(
            sql,
            crate::db_params![
                (chrono::Utc::now() - chrono::Duration::seconds(seconds)).to_rfc3339(),
                key.to_storage().to_string()
            ],
        )
        .await;
}

fn spawn_coordinator(
    fixture: &IngressFixture,
    budget: MaintenanceBudget,
) -> (
    CancellationToken,
    tokio::task::JoinHandle<()>,
    tokio::sync::mpsc::UnboundedReceiver<MaintenanceOutcome>,
) {
    let database = fixture.db.clone();
    let uow = fixture.uow.clone();
    let cursor = MaintenanceCursor::default();
    let (outcomes, receiver) = tokio::sync::mpsc::unbounded_channel();
    let coordinator = RetentionGcCoordinator {
        trigger: Arc::new(Notify::new()),
        run: Arc::new(move || {
            let database = database.clone();
            let uow = uow.clone();
            let cursor = cursor.clone();
            let outcomes = outcomes.clone();
            Box::pin(async move {
                let outcome =
                    run_maintenance_pass_with_cursor(&database, &uow, budget, &cursor, None).await;
                let _ = outcomes.send(outcome);
                outcome
            })
        }),
        partial_retry_delay: Duration::from_millis(10),
        periodic_interval: Duration::from_millis(20),
    };
    let cancellation = CancellationToken::new();
    let task = tokio::spawn(run_retention_gc_coordinator(
        coordinator,
        cancellation.clone(),
        CancellationToken::new(),
    ));
    (cancellation, task, receiver)
}

async fn next_pass(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<MaintenanceOutcome>,
) -> MaintenanceOutcome {
    tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .expect("maintenance pass deadline")
        .expect("coordinator alive")
}

async fn stop_coordinator(cancellation: CancellationToken, task: tokio::task::JoinHandle<()>) {
    cancellation.cancel();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("prompt coordinator shutdown")
        .expect("coordinator task");
}

async fn cancellation_during_real_pass(fixture: IngressFixture) {
    let held = fixture.db.begin().await.expect("occupy the only pool slot");
    let database = fixture.db.clone();
    let uow = fixture.uow.clone();
    let started = Arc::new(Notify::new());
    let pass_started = started.clone();
    let coordinator = RetentionGcCoordinator {
        trigger: Arc::new(Notify::new()),
        run: Arc::new(move || {
            let database = database.clone();
            let uow = uow.clone();
            let pass_started = pass_started.clone();
            Box::pin(async move {
                pass_started.notify_one();
                run_maintenance_pass(&database, &uow, MaintenanceBudget::DEFAULT, None).await
            })
        }),
        partial_retry_delay: Duration::from_millis(10),
        periodic_interval: Duration::from_secs(30),
    };
    let force_stop = CancellationToken::new();
    let task = tokio::spawn(run_retention_gc_coordinator(
        coordinator,
        CancellationToken::new(),
        force_stop.clone(),
    ));
    started.notified().await;
    tokio::task::yield_now().await;
    force_stop.cancel();
    tokio::time::timeout(Duration::from_millis(100), task)
        .await
        .expect("forced cancellation drops a blocked real pass")
        .expect("coordinator exits");
    drop(held);
    fixture.close().await;
}

async fn periodic_grace_repair(fixture: IngressFixture) {
    let key = interrupted_delivery(&fixture, "maintenance-grace").await;
    assert_eq!(terminal_count(&fixture).await, 0);
    assert_eq!(
        run_maintenance_pass(&fixture.db, &fixture.uow, MaintenanceBudget::DEFAULT, None).await,
        MaintenanceOutcome::Complete
    );
    assert_eq!(
        terminal_count(&fixture).await,
        0,
        "default 60-second grace protects fresh messages"
    );
    // Simulate a restart near the end of the grace interval. No Notify or
    // new submission follows coordinator startup: periodic ticks must repair it.
    backdate_created(&fixture, key, 30).await;
    let (cancellation, task, mut outcomes) =
        spawn_coordinator(&fixture, MaintenanceBudget::DEFAULT);
    assert_eq!(next_pass(&mut outcomes).await, MaintenanceOutcome::Complete);
    assert_eq!(
        terminal_count(&fixture).await,
        0,
        "startup remains inside grace"
    );
    backdate_created(&fixture, key, 61).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while terminal_count(&fixture).await == 0 {
            next_pass(&mut outcomes).await;
        }
    })
    .await
    .expect("periodic tick repairs without traffic");
    stop_coordinator(cancellation, task).await;
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    fixture.close().await;
}

async fn backlog_and_retention(fixture: IngressFixture) {
    for origin in [
        "maintenance-backlog-a",
        "maintenance-backlog-b",
        "maintenance-backlog-c",
    ] {
        interrupted_delivery(&fixture, origin).await;
    }
    let mut missing = fixture.submission(Some("maintenance-missing"), "missing receipt");
    missing.plan.intents.push(IngressEffectIntent::RouteDirect {
        recipient: "juliet@example.com".parse().expect("recipient"),
        fanout: vec!["juliet@example.com/phone".parse().expect("resource")],
        route_identity: EffectMessageIdentity::capture_ordinal(0),
    });
    commit_submission(&fixture.uow, &missing, 5)
        .await
        .expect("pending message");
    let budget = MaintenanceBudget {
        page_size: 1,
        max_pages: 1,
        ..immediate_budget()
    };
    assert_eq!(
        run_maintenance_pass(&fixture.db, &fixture.uow, budget, None).await,
        MaintenanceOutcome::Partial
    );
    assert_eq!(terminal_count(&fixture).await, 1, "one bounded page only");
    let (cancellation, task, mut outcomes) = spawn_coordinator(&fixture, budget);
    assert_eq!(next_pass(&mut outcomes).await, MaintenanceOutcome::Partial);
    while next_pass(&mut outcomes).await != MaintenanceOutcome::Complete {}
    stop_coordinator(cancellation, task).await;
    assert_eq!(terminal_count(&fixture).await, 3);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NULL")
            .await,
        1,
        "missing receipts remain unresolved"
    );
    let sql = match fixture.db.driver() {
        DatabaseDriver::Postgres => {
            "UPDATE ingress_messages SET terminal_at = ?::timestamptz WHERE terminal_at IS NOT NULL"
        }
        DatabaseDriver::Sqlite => {
            "UPDATE ingress_messages SET terminal_at = ? WHERE terminal_at IS NOT NULL"
        }
    };
    fixture
        .execute(
            sql,
            crate::db_params![(chrono::Utc::now() - chrono::Duration::days(9)).to_rfc3339()],
        )
        .await;
    assert_eq!(
        run_maintenance_pass(&fixture.db, &fixture.uow, immediate_budget(), None).await,
        MaintenanceOutcome::Complete
    );
    assert_eq!(
        fixture.count("ingress_messages").await,
        1,
        "retention removes repaired messages after expiry"
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    fixture.close().await;
}

async fn epoch_one_repair(fixture: IngressFixture) {
    let sql = match fixture.db.driver() {
        DatabaseDriver::Postgres => "UPDATE ingress_protocol_epoch SET epoch = epoch + 1, activated_at = ?::timestamptz, lineage_uuid = ?::uuid WHERE id = 1",
        DatabaseDriver::Sqlite => "UPDATE ingress_protocol_epoch SET epoch = epoch + 1, activated_at = ?, lineage_uuid = ? WHERE id = 1",
    };
    fixture
        .execute(
            sql,
            crate::db_params![
                chrono::Utc::now().to_rfc3339(),
                uuid::Uuid::new_v4().to_string()
            ],
        )
        .await;
    interrupted_delivery(&fixture, "maintenance-epoch-one").await;
    assert_eq!(
        run_maintenance_pass(&fixture.db, &fixture.uow, immediate_budget(), None).await,
        MaintenanceOutcome::Complete
    );
    assert_eq!(terminal_count(&fixture).await, 1);
    fixture.close().await;
}

async fn failed_head_does_not_starve_next_row(fixture: IngressFixture) {
    let head = interrupted_delivery(&fixture, "maintenance-failed-head").await;
    let next = interrupted_delivery(&fixture, "maintenance-after-head").await;
    backdate_created(&fixture, head, 120).await;
    backdate_created(&fixture, next, 90).await;
    test_hooks::force_terminalization_timeout_once(head);
    assert_eq!(
        run_maintenance_pass(&fixture.db, &fixture.uow, MaintenanceBudget::DEFAULT, None).await,
        MaintenanceOutcome::Partial
    );
    assert_eq!(
        terminal_count(&fixture).await,
        1,
        "a failed head row must not starve the rest of its page"
    );
    assert_eq!(
        run_maintenance_pass(&fixture.db, &fixture.uow, MaintenanceBudget::DEFAULT, None).await,
        MaintenanceOutcome::Complete
    );
    assert_eq!(terminal_count(&fixture).await, 2);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_maintenance_periodic_grace_repair() {
    periodic_grace_repair(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_maintenance_periodic_grace_repair() {
    if let Some(fixture) = IngressFixture::postgres("maintenance_grace").await {
        periodic_grace_repair(fixture).await;
    }
}
#[tokio::test]
async fn sqlite_maintenance_backlog_missing_receipt_and_retention() {
    backlog_and_retention(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_maintenance_backlog_missing_receipt_and_retention() {
    if let Some(fixture) = IngressFixture::postgres("maintenance_backlog").await {
        backlog_and_retention(fixture).await;
    }
}
#[tokio::test]
async fn sqlite_maintenance_epoch_one_execution() {
    epoch_one_repair(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_maintenance_epoch_one_execution() {
    if let Some(fixture) = IngressFixture::postgres("maintenance_epoch_one").await {
        epoch_one_repair(fixture).await;
    }
}

#[tokio::test]
async fn sqlite_maintenance_failed_head_does_not_starve_next_row() {
    failed_head_does_not_starve_next_row(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_maintenance_failed_head_does_not_starve_next_row() {
    if let Some(fixture) = IngressFixture::postgres("maintenance_failed_head").await {
        failed_head_does_not_starve_next_row(fixture).await;
    }
}

#[tokio::test]
async fn postgres_maintenance_contended_head_does_not_starve_next_row() {
    let Some(fixture) = IngressFixture::postgres("maintenance_contention").await else {
        return;
    };
    let head = interrupted_delivery(&fixture, "maintenance-contended").await;
    let next = interrupted_delivery(&fixture, "maintenance-uncontended").await;
    backdate_created(&fixture, head, 120).await;
    backdate_created(&fixture, next, 90).await;
    let mut blocker = fixture.uow.begin().await.expect("contending transaction");
    assert!(CanonicalMessageRepository::lock(&mut blocker, head)
        .await
        .expect("head lock"));
    let (cancellation, task, mut outcomes) =
        spawn_coordinator(&fixture, MaintenanceBudget::DEFAULT);
    assert_eq!(next_pass(&mut outcomes).await, MaintenanceOutcome::Partial);
    assert_eq!(
        terminal_count(&fixture).await,
        1,
        "uncontended row terminalizes in the same pass"
    );
    blocker.commit().await.expect("release contended head");
    assert_eq!(next_pass(&mut outcomes).await, MaintenanceOutcome::Complete);
    stop_coordinator(cancellation, task).await;
    assert_eq!(terminal_count(&fixture).await, 2);
    fixture.close().await;
}

#[tokio::test]
async fn postgres_maintenance_contended_prefix_preserves_continuation_cursor() {
    let Some(fixture) = IngressFixture::postgres("maintenance_contended_prefix").await else {
        return;
    };
    let first = interrupted_delivery(&fixture, "maintenance-contended-first").await;
    let second = interrupted_delivery(&fixture, "maintenance-contended-second").await;
    let later = interrupted_delivery(&fixture, "maintenance-later-free").await;
    backdate_created(&fixture, first, 180).await;
    backdate_created(&fixture, second, 150).await;
    backdate_created(&fixture, later, 120).await;

    let mut blocker = fixture.uow.begin().await.expect("contending transaction");
    for key in [first, second] {
        assert!(CanonicalMessageRepository::lock(&mut blocker, key)
            .await
            .expect("locked prefix"));
    }
    let budget = MaintenanceBudget {
        terminalization: Duration::from_millis(50),
        ..MaintenanceBudget::DEFAULT
    };
    let cursor = MaintenanceCursor::default();
    assert_eq!(
        run_maintenance_pass_with_cursor(&fixture.db, &fixture.uow, budget, &cursor, None).await,
        MaintenanceOutcome::TimedOut
    );
    assert_eq!(terminal_count(&fixture).await, 0);
    assert_eq!(
        run_maintenance_pass_with_cursor(&fixture.db, &fixture.uow, budget, &cursor, None).await,
        MaintenanceOutcome::TimedOut
    );
    assert_eq!(terminal_count(&fixture).await, 0);
    assert_eq!(
        run_maintenance_pass_with_cursor(&fixture.db, &fixture.uow, budget, &cursor, None).await,
        MaintenanceOutcome::Partial
    );
    assert_eq!(
        terminal_count(&fixture).await,
        1,
        "a continuation resumes beyond the contended prefix"
    );

    blocker.commit().await.expect("release contended prefix");
    assert_eq!(
        run_maintenance_pass_with_cursor(&fixture.db, &fixture.uow, budget, &cursor, None).await,
        MaintenanceOutcome::Complete
    );
    assert_eq!(terminal_count(&fixture).await, 3);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_maintenance_cancellation_during_real_pass_is_prompt() {
    cancellation_during_real_pass(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_maintenance_cancellation_during_real_pass_is_prompt() {
    let Some(fixture) = IngressFixture::postgres_with_pool("maintenance_cancel", 1).await else {
        return;
    };
    cancellation_during_real_pass(fixture).await;
}

#[tokio::test]
async fn postgres_pool_one_remains_admissible_during_maintenance() {
    let Some(fixture) = IngressFixture::postgres_with_pool("maintenance_pool_one", 1).await else {
        return;
    };
    for origin in ["maintenance-pool-a", "maintenance-pool-b"] {
        let key = interrupted_delivery(&fixture, origin).await;
        backdate_created(&fixture, key, 120).await;
    }
    let pending = stalled_route(&fixture).await;
    let cursor = MaintenanceCursor::default();
    let environment: Arc<dyn super::RecoveryEnvironment> =
        Arc::new(EmptyRecoveryEnvironment(ConnectionRegistry::new()));
    let maintenance = run_maintenance_pass_with_cursor(
        &fixture.db,
        &fixture.uow,
        immediate_budget(),
        &cursor,
        Some(environment),
    );
    let foreground = async {
        let transaction = tokio::time::timeout(Duration::from_secs(2), fixture.uow.begin())
            .await
            .expect("foreground admission waits only for one bounded operation")
            .expect("foreground transaction");
        transaction.commit().await.expect("foreground commit");
    };
    let (outcome, ()) = tokio::join!(maintenance, foreground);
    assert_eq!(outcome, MaintenanceOutcome::Complete);
    assert_eq!(terminal_count(&fixture).await, 2);
    cursor.wait_for_recovery_accounting().await;
    assert_eq!(super::super::recovery_executor::attempt_count(pending), 1);
    fixture.close().await;
}

async fn unreceipted_page_selects_only_recoverable_pending_rows(fixture: IngressFixture) {
    let complete = interrupted_delivery(&fixture, "recovery-page-complete").await;
    let mut pending = fixture.submission(Some("recovery-page-pending"), "pending");
    pending.plan.intents.push(IngressEffectIntent::RouteDirect {
        recipient: "juliet@example.com".parse().expect("recipient"),
        fanout: vec!["juliet@example.com/phone".parse().expect("resource")],
        route_identity: EffectMessageIdentity::capture_ordinal(0),
    });
    let pending_key = commit_submission(&fixture.uow, &pending, 5)
        .await
        .expect("pending commit")
        .message_key
        .expect("key");
    let mut carbons = fixture.submission(Some("recovery-page-carbons"), "carbons only");
    carbons.plan.intents.push(IngressEffectIntent::Carbons {
        carbon_recipients: vec!["romeo@example.com/laptop".parse().expect("carbon")],
        excluded_source: "romeo@example.com/phone".parse().expect("source"),
        kind: waddle_xmpp::protocol::CarbonKind::Sent,
    });
    let carbons_key = commit_submission(&fixture.uow, &carbons, 5)
        .await
        .expect("carbons commit")
        .message_key
        .expect("key");
    let route_kind = crate::ingress_substrate::EffectReceiptKind::from_storage(
        waddle_xmpp::ingress::IngressEffectKind::RouteDirect.storage_tag(),
    );
    let mut tx = fixture.db.begin().await.expect("scan transaction");
    let page = crate::ingress_substrate::unreceipted_nonterminal_candidates(
        &mut tx,
        None,
        chrono::Utc::now() + chrono::Duration::seconds(1),
        &[route_kind],
        16,
    )
    .await
    .expect("unreceipted page");
    let empty = crate::ingress_substrate::unreceipted_nonterminal_candidates(
        &mut tx,
        None,
        chrono::Utc::now() + chrono::Duration::seconds(1),
        &[],
        16,
    )
    .await
    .expect("empty kinds");
    tx.commit().await.expect("scan commit");
    // Every pending row is paged so the cursor can advance past unsupported
    // ones; only rows with an unreceipted recoverable kind are flagged.
    let flags: Vec<_> = page
        .iter()
        .map(|candidate| (candidate.key, candidate.recoverable))
        .collect();
    assert_eq!(flags, vec![(pending_key, true), (carbons_key, false)]);
    assert!(!page.iter().any(|candidate| candidate.key == complete));
    assert!(empty.is_empty());
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_unreceipted_page() {
    unreceipted_page_selects_only_recoverable_pending_rows(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_unreceipted_page() {
    if let Some(fixture) = IngressFixture::postgres("recovery_page").await {
        unreceipted_page_selects_only_recoverable_pending_rows(fixture).await;
    }
}

use waddle_xmpp::stream_management::{
    DetachedSession, InMemorySmSessionRegistry, SmSessionRegistry,
};
async fn store_detached(sm: &InMemorySmSessionRegistry, resource: &jid::FullJid) {
    sm.store_session(DetachedSession {
        stream_id: resource.to_string(),
        user_id: resource.to_bare().to_string(),
        jid: resource.clone(),
        occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        inbound_count: 0,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: Vec::new(),
        max_resume_time: Some(300),
        detached_at: std::time::Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    })
    .await
    .expect("store detached session");
}

async fn append_count(sm: &InMemorySmSessionRegistry, resource: &jid::FullJid) -> usize {
    sm.peek_session(&resource.to_string())
        .await
        .expect("peek session")
        .expect("retained session")
        .unacked_stanzas
        .len()
}

struct StateEnvironment(Arc<crate::server::routes::websocket::WebSocketState>);

impl super::RecoveryEnvironment for StateEnvironment {
    fn recovery_deps(&self) -> Deps<'_> {
        super::RecoveryEnvironment::recovery_deps(self.0.as_ref())
    }
}

async fn recovery_phase_completes_a_lost_detached_route(fixture: IngressFixture) {
    let persistence = Arc::new(
        crate::sm_persistence::DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("SM persistence"),
    );
    let sm = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence));
    let resource: jid::FullJid = "juliet@example.com/phone".parse().expect("resource");
    store_detached(&sm, &resource).await;
    let pool = crate::db::DatabasePool::new(
        crate::db::DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
        crate::db::PoolConfig,
    )
    .await
    .expect("shared pool");
    let state = crate::server::routes::websocket::tests::create_test_websocket_state_with_db_pool_and_ingress(
        Arc::new(pool), Arc::new(fixture.authority().await),
    ).await;
    let mut state = Arc::try_unwrap(state).unwrap_or_else(|_| panic!("fresh state"));
    state.deps.protocol.sm_session_registry = sm.clone();
    let environment: Arc<dyn super::RecoveryEnvironment> =
        Arc::new(StateEnvironment(Arc::new(state)));
    let mut submission =
        fixture.submission(Some("maintenance-lost-route"), "lost detached delivery");
    let identity = EffectMessageIdentity::capture_ordinal(0);
    submission.plan.intents = vec![IngressEffectIntent::RouteDirect {
        recipient: resource.to_bare(),
        fanout: vec![resource.clone()],
        route_identity: identity.clone(),
    }];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(
        ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
            route_identity: Some(identity),
            call_setup: None,
            bare: resource.to_bare(),
            resources: vec![resource.clone()],
            stanza: Box::new(Stanza::Message(submission.plan.sanitized_message.clone())),
        }),
    ))];
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("phase B");
    let key = decision.message_key.expect("key");
    backdate_created(&fixture, key, 120).await;
    assert_eq!(
        run_maintenance_pass(&fixture.db, &fixture.uow, MaintenanceBudget::DEFAULT, None).await,
        MaintenanceOutcome::Complete
    );
    assert_eq!(append_count(&sm, &resource).await, 0);
    assert_eq!(terminal_count(&fixture).await, 0);
    assert_eq!(super::super::recovery_executor::attempt_count(key), 0);
    let gate = test_hooks::pause_after_recovery_freeze(key);
    let pass = run_maintenance_pass(
        &fixture.db,
        &fixture.uow,
        MaintenanceBudget::DEFAULT,
        Some(environment.clone()),
    );
    let release = async {
        tokio::time::timeout(Duration::from_secs(5), gate.wait_until_reached())
            .await
            .expect("recovery freeze barrier");
        // A new canonical lock is available while recovery is paused: freeze committed.
        let mut tx = fixture.uow.begin().await.expect("post-freeze transaction");
        assert!(CanonicalMessageRepository::lock(&mut tx, key)
            .await
            .expect("post-freeze lock"));
        tx.commit().await.expect("post-freeze commit");
        gate.release();
    };
    let (outcome, ()) = tokio::join!(pass, release);
    assert_eq!(outcome, MaintenanceOutcome::Complete);
    assert_eq!(append_count(&sm, &resource).await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(terminal_count(&fixture).await, 1);
    assert_eq!(super::super::recovery_executor::attempt_count(key), 1);
    assert_eq!(
        run_maintenance_pass(
            &fixture.db,
            &fixture.uow,
            MaintenanceBudget::DEFAULT,
            Some(environment)
        )
        .await,
        MaintenanceOutcome::Complete
    );
    assert_eq!(append_count(&sm, &resource).await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_recovery_phase_completes_a_lost_detached_route() {
    recovery_phase_completes_a_lost_detached_route(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_recovery_phase_completes_a_lost_detached_route() {
    if let Some(fixture) = IngressFixture::postgres("maintenance_lost_route").await {
        recovery_phase_completes_a_lost_detached_route(fixture).await;
    }
}

/// A failed or timed-out accounting read must not lose the row's credit: the
/// item returns to the queue ahead of newer rows, and the queue stays bounded.
#[test]
fn failed_accounting_reads_are_requeued_ahead_and_bounded() {
    let queue = std::sync::Mutex::new(Vec::new());
    let newer = accounting_attempt(MessageKey::new(), 3);
    let failed = accounting_attempt(MessageKey::new(), 1);
    queue.lock().expect("queue").push(newer.clone());
    requeue_failed_accounting(&queue, Vec::new());
    assert_eq!(*queue.lock().expect("queue"), vec![newer.clone()]);
    requeue_failed_accounting(&queue, vec![failed.clone()]);
    assert_eq!(*queue.lock().expect("queue"), vec![failed, newer.clone()]);

    let flood: Vec<_> = (0..RECOVERY_ACCOUNTING_QUEUE_BOUND + 5)
        .map(|_| accounting_attempt(MessageKey::new(), 0))
        .collect();
    // Two items already queued plus the flood exceed the bound by this many.
    let excess = flood.len() + 2 - RECOVERY_ACCOUNTING_QUEUE_BOUND;
    let survivor = flood[excess].clone();
    requeue_failed_accounting(&queue, flood.clone());
    let bounded = queue.lock().expect("queue").clone();
    assert_eq!(bounded.len(), RECOVERY_ACCOUNTING_QUEUE_BOUND);
    assert_eq!(bounded[0], survivor, "the oldest excess items are dropped");
    assert_eq!(
        bounded[bounded.len() - 1],
        newer,
        "existing rows stay behind the retries"
    );
}

fn accounting_attempt(key: MessageKey, generation: u64) -> super::RecoveryAttempt {
    super::RecoveryAttempt {
        key,
        observed: crate::ingress_substrate::RecoveryEvidence {
            intents: 1,
            receipts: 0,
            progress: 0,
        },
        classification: super::AttemptClassification::Evaluable,
        pending: vec![waddle_xmpp::ingress::IngressEffectKind::RouteDirect],
        generation,
    }
}

struct EmptyRecoveryEnvironment(ConnectionRegistry);
impl super::RecoveryEnvironment for EmptyRecoveryEnvironment {
    fn recovery_deps(&self) -> Deps<'_> {
        Deps::new(&self.0, "example.com")
    }
}

async fn stalled_route(fixture: &IngressFixture) -> MessageKey {
    let mut submission = fixture.submission(Some("maintenance-stall"), "pending delivery");
    let resources = ["juliet@example.com/phone", "juliet@example.com/laptop"]
        .into_iter()
        .map(|resource| resource.parse().expect("resource"))
        .collect();
    submission.plan.intents = vec![IngressEffectIntent::RouteDirect {
        recipient: "juliet@example.com".parse().expect("recipient"),
        fanout: resources,
        route_identity: EffectMessageIdentity::capture_ordinal(0),
    }];
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit route");
    decision.message_key.expect("key")
}

/// Maintenance also runs at startup and after every committed decision, so a
/// burst of passes must not burn the streak: parking a row that is merely
/// waiting for its recipient would suppress a recoverable delivery for a whole
/// cooldown. Only one attempt per sample interval counts (#1782).
#[tokio::test]
async fn sqlite_rapid_passes_within_the_sample_interval_never_park_a_row() {
    let fixture = IngressFixture::sqlite().await;
    let key = stalled_route(&fixture).await;
    let environment = EmptyRecoveryEnvironment(ConnectionRegistry::new());
    let cursor = MaintenanceCursor::default();
    // The production sample interval, with everything else immediate.
    let budget = MaintenanceBudget {
        grace: chrono::Duration::zero(),
        ..MaintenanceBudget::DEFAULT
    };
    for _ in 0..(budget.recovery_stall_attempts * 4) {
        assert_eq!(
            super::recover_candidates(&fixture.db, &fixture.uow, budget, &cursor, &environment)
                .await,
            MaintenanceOutcome::Complete
        );
        cursor.wait_for_recovery_accounting().await;
    }
    assert_eq!(
        super::super::recovery_executor::attempt_count(key),
        u64::from(budget.recovery_stall_attempts * 4),
        "every rapid pass still attempts the row; none of them parks it"
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_stalled_row_parks_exactly_at_threshold_and_cache_hit_queues_nothing() {
    let fixture = IngressFixture::sqlite().await;
    let key = stalled_route(&fixture).await;
    let environment = EmptyRecoveryEnvironment(ConnectionRegistry::new());
    let cursor = MaintenanceCursor::default();
    let budget = immediate_budget();
    for attempt in 1..=budget.recovery_stall_attempts {
        assert_eq!(
            super::recover_candidates(&fixture.db, &fixture.uow, budget, &cursor, &environment)
                .await,
            MaintenanceOutcome::Complete
        );
        assert_eq!(
            super::super::recovery_executor::attempt_count(key),
            u64::from(attempt)
        );
        super::spawn_recovery_accounting(&fixture.db, &cursor, budget);
        cursor.wait_for_recovery_accounting().await;
    }
    assert_eq!(
        super::recover_candidates(&fixture.db, &fixture.uow, budget, &cursor, &environment).await,
        MaintenanceOutcome::Complete
    );
    assert_eq!(
        super::super::recovery_executor::attempt_count(key),
        u64::from(budget.recovery_stall_attempts)
    );
    assert!(cursor.recovery_accounting.lock().expect("queue").is_empty());
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(terminal_count(&fixture).await, 0);
    tokio::time::pause();
    tokio::time::advance(budget.recovery_stall_cooldown).await;
    tokio::time::resume();
    super::recover_candidates(&fixture.db, &fixture.uow, budget, &cursor, &environment).await;
    assert_eq!(
        super::super::recovery_executor::attempt_count(key),
        u64::from(budget.recovery_stall_attempts) + 1
    );
    assert_eq!(cursor.recovery_accounting.lock().expect("queue").len(), 1);
    super::spawn_recovery_accounting(&fixture.db, &cursor, budget);
    cursor.wait_for_recovery_accounting().await;
    fixture.close().await;
}

#[tokio::test]
async fn stale_requeued_accounting_cannot_resurrect_a_reset_streak() {
    let key = MessageKey::new();
    let mut stalled = super::StalledRows::default();
    let mut suppressed = super::UnsupportedRows::default();
    let budget = immediate_budget();
    for generation in 1..=2 {
        let attempt = accounting_attempt(key, generation);
        stalled.account(&attempt, attempt.observed, budget, &mut suppressed);
    }
    let stale = accounting_attempt(key, 3);
    let mut newer = accounting_attempt(key, 4);
    newer.classification = super::AttemptClassification::Inconclusive;
    stalled.account(&newer, newer.observed, budget, &mut suppressed);
    let queue = std::sync::Mutex::new(Vec::new());
    requeue_failed_accounting(&queue, vec![stale]);
    for attempt in queue.into_inner().expect("queue") {
        stalled.account(&attempt, attempt.observed, budget, &mut suppressed);
    }
    for generation in 5..=6 {
        let attempt = accounting_attempt(key, generation);
        stalled.account(&attempt, attempt.observed, budget, &mut suppressed);
        assert_eq!(suppressed.get(key, attempt.observed), None);
    }
    let attempt = accounting_attempt(key, 7);
    stalled.account(&attempt, attempt.observed, budget, &mut suppressed);
    assert!(matches!(
        suppressed.get(key, attempt.observed),
        Some(super::Suppression::StalledUntil(_))
    ));
}

#[tokio::test]
async fn accounting_storage_failure_resets_streak_and_retry_only_credits_progress() {
    let fixture = IngressFixture::sqlite().await;
    let key = stalled_route(&fixture).await;
    let cursor = MaintenanceCursor::default();
    let budget = immediate_budget();
    for generation in 1..=2 {
        cursor
            .recovery_accounting
            .lock()
            .expect("queue")
            .push(accounting_attempt(key, generation));
        super::spawn_recovery_accounting(&fixture.db, &cursor, budget);
        cursor.wait_for_recovery_accounting().await;
    }
    fixture
        .execute(
            "ALTER TABLE ingress_delivery_receipts RENAME TO suspended_progress",
            vec![],
        )
        .await;
    cursor
        .recovery_accounting
        .lock()
        .expect("queue")
        .push(accounting_attempt(key, 3));
    super::spawn_recovery_accounting(&fixture.db, &cursor, budget);
    cursor.wait_for_recovery_accounting().await;
    assert_eq!(
        cursor.recovery_accounting.lock().expect("queue")[0].classification,
        super::AttemptClassification::Inconclusive
    );
    fixture
        .execute(
            "ALTER TABLE suspended_progress RENAME TO ingress_delivery_receipts",
            vec![],
        )
        .await;
    // Read recovery cannot convert the failed observation into a no-progress attempt.
    super::spawn_recovery_accounting(&fixture.db, &cursor, budget);
    cursor.wait_for_recovery_accounting().await;
    for generation in 4..=5 {
        let attempt = accounting_attempt(key, generation);
        cursor
            .recovery_accounting
            .lock()
            .expect("queue")
            .push(attempt.clone());
        super::spawn_recovery_accounting(&fixture.db, &cursor, budget);
        cursor.wait_for_recovery_accounting().await;
        assert_eq!(
            cursor
                .recovery_unsupported
                .lock()
                .expect("suppression")
                .get(key, attempt.observed),
            None
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_replica_progress_rearms_parked_row_and_counts_a_new_episode() {
    use waddle_xmpp::ingress::IngressEffectKind;
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let fixture = IngressFixture::sqlite().await;
    let key = stalled_route(&fixture).await;
    let environment = EmptyRecoveryEnvironment(ConnectionRegistry::new());
    let cursor = MaintenanceCursor::default();
    let budget = immediate_budget();
    let labels = [("kind", "route_direct"), ("reason", "no_durable_progress")];
    let before = metrics
        .counter_sum("ingress.maintenance.unrecoverable_obligations", &labels)
        .unwrap_or(0);
    for _ in 0..budget.recovery_stall_attempts {
        super::recover_candidates(&fixture.db, &fixture.uow, budget, &cursor, &environment).await;
        super::spawn_recovery_accounting(&fixture.db, &cursor, budget);
        cursor.wait_for_recovery_accounting().await;
    }
    assert_eq!(
        metrics.counter_sum("ingress.maintenance.unrecoverable_obligations", &labels),
        Some(before + 1)
    );
    for _ in 0..3 {
        super::recover_candidates(&fixture.db, &fixture.uow, budget, &cursor, &environment).await;
        assert!(cursor.recovery_accounting.lock().expect("queue").is_empty());
    }
    // An unchanged retry after expiry is the same episode, not another classification.
    tokio::time::pause();
    tokio::time::advance(budget.recovery_stall_cooldown).await;
    tokio::time::resume();
    super::recover_candidates(&fixture.db, &fixture.uow, budget, &cursor, &environment).await;
    super::spawn_recovery_accounting(&fixture.db, &cursor, budget);
    cursor.wait_for_recovery_accounting().await;
    assert_eq!(
        metrics.counter_sum("ingress.maintenance.unrecoverable_obligations", &labels),
        Some(before + 1)
    );
    let old = super::evidence_now(&fixture.db, key)
        .await
        .expect("old evidence");
    // A second replica records one recipient, without the aggregate receipt.
    let mut tx = fixture.uow.begin().await.expect("replica transaction");
    let intents = crate::ingress_uow::EffectIntentRepository::load(&mut tx, key)
        .await
        .expect("intents");
    let receipt = super::super::durable::receipt_key(&intents[0]).expect("receipt key");
    crate::ingress_uow::DeliveryProgressRepository::record(
        &mut tx,
        key,
        &receipt,
        &["juliet@example.com/phone".parse().expect("resource")],
    )
    .await
    .expect("replica progress");
    tx.commit().await.expect("replica commit");
    let fresh = super::evidence_now(&fixture.db, key)
        .await
        .expect("fresh evidence");
    assert_eq!(fresh.progress, old.progress + 1);
    assert_eq!(fresh.receipts, old.receipts);
    let mut tx = fixture.db.begin().await.expect("scan");
    let page = crate::ingress_substrate::unreceipted_nonterminal_candidates(
        &mut tx,
        None,
        chrono::Utc::now() + chrono::Duration::seconds(1),
        &[crate::ingress_substrate::EffectReceiptKind::from_storage(
            IngressEffectKind::RouteDirect.storage_tag(),
        )],
        10,
    )
    .await
    .expect("scan evidence");
    tx.commit().await.expect("scan commit");
    assert_eq!(
        page.iter()
            .find(|row| row.key == key)
            .expect("row")
            .evidence,
        fresh
    );
    let attempts = super::super::recovery_executor::attempt_count(key);
    for index in 1..=budget.recovery_stall_attempts {
        super::recover_candidates(&fixture.db, &fixture.uow, budget, &cursor, &environment).await;
        super::spawn_recovery_accounting(&fixture.db, &cursor, budget);
        cursor.wait_for_recovery_accounting().await;
        assert_eq!(
            super::super::recovery_executor::attempt_count(key),
            attempts + u64::from(index)
        );
        assert_eq!(
            metrics.counter_sum("ingress.maintenance.unrecoverable_obligations", &labels),
            Some(before + 1 + u64::from(index == budget.recovery_stall_attempts))
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn accounting_timeout_resets_streak_and_is_requeued_as_inconclusive() {
    let fixture = IngressFixture::sqlite().await;
    let key = stalled_route(&fixture).await;
    let mut config =
        crate::db::DatabaseConfig::new(DatabaseDriver::Sqlite, fixture.db.database_url());
    config.pool_size = 1;
    let accounting_db = crate::db::Database::from_config("accounting-timeout", &config)
        .await
        .expect("one-connection accounting pool");
    let cursor = MaintenanceCursor::default();
    let budget = immediate_budget();
    for generation in 1..=2 {
        cursor
            .recovery_accounting
            .lock()
            .expect("queue")
            .push(accounting_attempt(key, generation));
        super::spawn_recovery_accounting(&accounting_db, &cursor, budget);
        cursor.wait_for_recovery_accounting().await;
    }
    // Occupy the only pool slot so the bounded accounting read must time out.
    let held = accounting_db.begin().await.expect("occupy connection");
    cursor
        .recovery_accounting
        .lock()
        .expect("queue")
        .push(accounting_attempt(key, 3));
    tokio::time::pause();
    super::spawn_recovery_accounting(&accounting_db, &cursor, budget);
    cursor.wait_for_recovery_accounting().await;
    tokio::time::resume();
    held.commit().await.expect("release connection");
    assert_eq!(
        cursor.recovery_accounting.lock().expect("queue")[0].classification,
        super::AttemptClassification::Inconclusive
    );
    super::spawn_recovery_accounting(&accounting_db, &cursor, budget);
    cursor.wait_for_recovery_accounting().await;
    for generation in 4..=5 {
        let attempt = accounting_attempt(key, generation);
        cursor
            .recovery_accounting
            .lock()
            .expect("queue")
            .push(attempt.clone());
        super::spawn_recovery_accounting(&accounting_db, &cursor, budget);
        cursor.wait_for_recovery_accounting().await;
        assert_eq!(
            cursor
                .recovery_unsupported
                .lock()
                .expect("suppression")
                .get(key, attempt.observed),
            None
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn newer_inconclusive_attempt_clears_an_older_workers_parking_decision() {
    let key = MessageKey::new();
    let budget = immediate_budget();
    let mut stalled = super::StalledRows::default();
    let mut suppressed = super::UnsupportedRows::default();
    for generation in 1..=3 {
        let attempt = accounting_attempt(key, generation);
        stalled.account(&attempt, attempt.observed, budget, &mut suppressed);
    }
    let mut newer = accounting_attempt(key, 4);
    assert!(matches!(
        suppressed.get(key, newer.observed),
        Some(super::Suppression::StalledUntil(_))
    ));
    // This attempt began before the older worker installed the suppression.
    newer.classification = super::AttemptClassification::Inconclusive;
    stalled.account(&newer, newer.observed, budget, &mut suppressed);
    assert_eq!(suppressed.get(key, newer.observed), None);
    for generation in 5..=6 {
        let attempt = accounting_attempt(key, generation);
        stalled.account(&attempt, attempt.observed, budget, &mut suppressed);
        assert_eq!(suppressed.get(key, attempt.observed), None);
    }
    // Unsupported suppression retains its existing semantics on uncertainty.
    suppressed.insert(key, newer.observed, super::Suppression::Unsupported);
    newer.generation = 7;
    stalled.account(&newer, newer.observed, budget, &mut suppressed);
    assert_eq!(
        suppressed.get(key, newer.observed),
        Some(super::Suppression::Unsupported)
    );
}
