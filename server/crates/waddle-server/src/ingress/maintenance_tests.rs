use super::{
    run_maintenance_pass, run_maintenance_pass_with_cursor, MaintenanceBudget, MaintenanceCursor,
    MaintenanceOutcome,
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
                    run_maintenance_pass_with_cursor(&database, &uow, budget, &cursor).await;
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
                run_maintenance_pass(&database, &uow, MaintenanceBudget::DEFAULT).await
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
        run_maintenance_pass(&fixture.db, &fixture.uow, MaintenanceBudget::DEFAULT).await,
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
        run_maintenance_pass(&fixture.db, &fixture.uow, budget).await,
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
        run_maintenance_pass(&fixture.db, &fixture.uow, immediate_budget()).await,
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
        run_maintenance_pass(&fixture.db, &fixture.uow, immediate_budget()).await,
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
        run_maintenance_pass(&fixture.db, &fixture.uow, MaintenanceBudget::DEFAULT).await,
        MaintenanceOutcome::Partial
    );
    assert_eq!(
        terminal_count(&fixture).await,
        1,
        "a failed head row must not starve the rest of its page"
    );
    assert_eq!(
        run_maintenance_pass(&fixture.db, &fixture.uow, MaintenanceBudget::DEFAULT).await,
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
        run_maintenance_pass_with_cursor(&fixture.db, &fixture.uow, budget, &cursor).await,
        MaintenanceOutcome::TimedOut
    );
    assert_eq!(terminal_count(&fixture).await, 0);
    assert_eq!(
        run_maintenance_pass_with_cursor(&fixture.db, &fixture.uow, budget, &cursor).await,
        MaintenanceOutcome::TimedOut
    );
    assert_eq!(terminal_count(&fixture).await, 0);
    assert_eq!(
        run_maintenance_pass_with_cursor(&fixture.db, &fixture.uow, budget, &cursor).await,
        MaintenanceOutcome::Partial
    );
    assert_eq!(
        terminal_count(&fixture).await,
        1,
        "a continuation resumes beyond the contended prefix"
    );

    blocker.commit().await.expect("release contended prefix");
    assert_eq!(
        run_maintenance_pass_with_cursor(&fixture.db, &fixture.uow, budget, &cursor).await,
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
    let maintenance = run_maintenance_pass(&fixture.db, &fixture.uow, immediate_budget());
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
    let page = crate::ingress_substrate::unreceipted_nonterminal_keys(
        &mut tx,
        None,
        chrono::Utc::now() + chrono::Duration::seconds(1),
        &[route_kind],
        16,
    )
    .await
    .expect("unreceipted page");
    let empty = crate::ingress_substrate::unreceipted_nonterminal_keys(
        &mut tx,
        None,
        chrono::Utc::now() + chrono::Duration::seconds(1),
        &[],
        16,
    )
    .await
    .expect("empty kinds");
    tx.commit().await.expect("scan commit");
    let keys: Vec<_> = page.into_iter().map(|(_, key)| key).collect();
    assert_eq!(keys, vec![pending_key]);
    assert!(!keys.contains(&complete) && !keys.contains(&carbons_key));
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
