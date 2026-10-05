//! The observer-retention phase rides passes on its own interval (#1901).

use super::{run_maintenance_pass_with_cursor, MaintenanceBudget, MaintenanceCursor};
use crate::{ingress::gc::RetentionGcBudget, ingress::test_support::IngressFixture};
use std::time::Duration;
use uuid::Uuid;

/// Completed work settled at the epoch, with no canonical row: collectable.
async fn insert_expired_work(fixture: &IngressFixture, count: usize) {
    let mut tx = fixture.db.begin_immediate().await.expect("expired work");
    for _ in 0..count {
        tx.execute(
            "INSERT INTO extension_room_observation_work (id, source_key, message_key, plugin_id, generation, identity, room_jid, revision, source_json, body, status, attempt, due_at_ms, terminal_category, settled_at_ms) VALUES (?, ?, ?, 'observer', 1, 'identity', 'room@conference.example.org', 0, '{}', '', 'completed', 1, 0, 'completed', 0)",
            crate::db_params![Uuid::now_v7().to_string(), Uuid::now_v7().to_string(), Uuid::now_v7().to_string()],
        )
        .await
        .expect("expired work row");
    }
    tx.commit().await.expect("expired work commit");
}

fn observer_runs(
    metrics: &waddle_xmpp::telemetry::test_support::MetricsTestGuard,
    outcome: &str,
) -> u64 {
    metrics
        .counter_sum(
            "ingress.maintenance.runs",
            &[("phase", "observer_retention"), ("outcome", outcome)],
        )
        .unwrap_or(0)
}

async fn observer_retention_waits_out_its_interval(fixture: IngressFixture) {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let cursor = MaintenanceCursor::default();
    assert!(
        cursor.observer_retention_due(),
        "a fresh cursor collects at startup"
    );
    let budget = MaintenanceBudget::DEFAULT;
    run_maintenance_pass_with_cursor(&fixture.db, &fixture.uow, budget, &cursor, None).await;
    assert_eq!(observer_runs(&metrics, "complete"), 1);
    assert!(!cursor.observer_retention_due());

    // A commit-triggered pass inside the interval leaves expired history alone.
    insert_expired_work(&fixture, 1).await;
    run_maintenance_pass_with_cursor(&fixture.db, &fixture.uow, budget, &cursor, None).await;
    assert_eq!(
        observer_runs(&metrics, "complete"),
        1,
        "the phase was skipped"
    );
    assert_eq!(fixture.count("extension_room_observation_work").await, 1);

    cursor.expire_observer_retention_interval();
    run_maintenance_pass_with_cursor(&fixture.db, &fixture.uow, budget, &cursor, None).await;
    assert_eq!(observer_runs(&metrics, "complete"), 2);
    assert_eq!(fixture.count("extension_room_observation_work").await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_observer_retention_waits_out_its_interval() {
    observer_retention_waits_out_its_interval(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_observer_retention_waits_out_its_interval() {
    if let Some(fixture) = IngressFixture::postgres("observer_retention_interval").await {
        observer_retention_waits_out_its_interval(fixture).await;
    }
}

async fn partial_observer_retention_retries_on_the_next_pass(fixture: IngressFixture) {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let cursor = MaintenanceCursor::default();
    insert_expired_work(&fixture, 300).await;
    // A zero cooperative budget stops the phase after its first full batch.
    let budget = MaintenanceBudget {
        retention: RetentionGcBudget {
            cooperative: Duration::ZERO,
            ..RetentionGcBudget::DEFAULT
        },
        ..MaintenanceBudget::DEFAULT
    };
    run_maintenance_pass_with_cursor(&fixture.db, &fixture.uow, budget, &cursor, None).await;
    assert_eq!(observer_runs(&metrics, "partial"), 1);
    assert_eq!(fixture.count("extension_room_observation_work").await, 44);
    assert!(cursor.observer_retention_due(), "a partial run stays due");

    run_maintenance_pass_with_cursor(&fixture.db, &fixture.uow, budget, &cursor, None).await;
    assert_eq!(observer_runs(&metrics, "complete"), 1);
    assert_eq!(fixture.count("extension_room_observation_work").await, 0);
    assert!(!cursor.observer_retention_due());

    run_maintenance_pass_with_cursor(&fixture.db, &fixture.uow, budget, &cursor, None).await;
    assert_eq!(
        (
            observer_runs(&metrics, "partial"),
            observer_runs(&metrics, "complete")
        ),
        (1, 1),
        "a complete run waits out the interval"
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_partial_observer_retention_retries_on_the_next_pass() {
    partial_observer_retention_retries_on_the_next_pass(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_partial_observer_retention_retries_on_the_next_pass() {
    if let Some(fixture) = IngressFixture::postgres("observer_retention_partial").await {
        partial_observer_retention_retries_on_the_next_pass(fixture).await;
    }
}
