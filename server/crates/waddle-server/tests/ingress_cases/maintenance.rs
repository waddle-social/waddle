use super::*;
use chrono::{DateTime, Duration, Utc};
use waddle_server::{
    db::DatabaseDriver,
    ingress::{execute::execute_effects, Deps, ImmediateSink},
    ingress_substrate::receipt_complete_nonterminal_keys,
};
use waddle_xmpp::{
    ingress::{EffectMessageIdentity, MessageKey},
    registry::ConnectionRegistry,
};

async fn receipt_complete_message(fixture: &IngressFixture, origin: &str) -> MessageKey {
    let submission = archive_plan(fixture, Some(origin), "maintenance scan", origin);
    let decision = commit_submission(&fixture.uow, &submission, 5)
        .await
        .expect("commit archived message");
    let registry = ConnectionRegistry::new();
    let deps = Deps::new(&registry, "example.com");
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        std::time::Duration::ZERO,
    )
    .await;
    assert!(report.terminalization_failure.is_some());
    decision.message_key.expect("canonical message")
}

async fn set_created_at(fixture: &IngressFixture, key: MessageKey, at: DateTime<Utc>) {
    let sql = match fixture.db.driver() {
        DatabaseDriver::Postgres => {
            "UPDATE ingress_messages SET created_at = ?::timestamptz WHERE message_key = ?::uuid"
        }
        DatabaseDriver::Sqlite => {
            "UPDATE ingress_messages SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', ?) WHERE message_key = ?"
        }
    };
    fixture
        .execute(
            sql,
            waddle_server::db_params![at.to_rfc3339(), key.to_storage().to_string()],
        )
        .await;
}

async fn bounded_keyset_scan(fixture: IngressFixture) {
    let cutoff = DateTime::from_timestamp(1_800_000_000, 0).expect("cutoff");
    let old = cutoff - Duration::seconds(1);
    let mut expected = Vec::new();
    for origin in [
        "maintenance-first",
        "maintenance-second",
        "maintenance-third",
    ] {
        let key = receipt_complete_message(&fixture, origin).await;
        set_created_at(&fixture, key, old).await;
        expected.push((old, key));
    }
    expected.sort_by_key(|(_, key)| key.to_storage());
    let young = receipt_complete_message(&fixture, "maintenance-young").await;
    set_created_at(&fixture, young, cutoff).await;
    let terminal = receipt_complete_message(&fixture, "maintenance-terminal").await;
    set_created_at(&fixture, terminal, old).await;
    assert!(
        waddle_server::ingress::execute::terminalize_if_complete(&fixture.uow, terminal)
            .await
            .expect("terminalize excluded row")
    );

    let mut missing = fixture.submission(Some("maintenance-missing"), "unsettled route");
    missing.plan.intents.push(IngressEffectIntent::RouteDirect {
        recipient: "juliet@example.com".parse().expect("recipient"),
        fanout: vec!["juliet@example.com/phone".parse().expect("resource")],
        route_identity: EffectMessageIdentity::capture_ordinal(0),
    });
    let missing = commit_submission(&fixture.uow, &missing, 5)
        .await
        .expect("commit unreceipted intent")
        .message_key
        .expect("missing receipt key");
    set_created_at(&fixture, missing, old - Duration::seconds(1)).await;

    // Epoch-one readers use the same query and retain the keyset semantics.
    let activation = match fixture.db.driver() {
        DatabaseDriver::Postgres => "UPDATE ingress_protocol_epoch SET epoch = epoch + 1, activated_at = ?::timestamptz, lineage_uuid = ?::uuid WHERE id = 1",
        DatabaseDriver::Sqlite => "UPDATE ingress_protocol_epoch SET epoch = epoch + 1, activated_at = ?, lineage_uuid = ? WHERE id = 1",
    };
    fixture
        .execute(
            activation,
            waddle_server::db_params![Utc::now().to_rfc3339(), uuid::Uuid::new_v4().to_string()],
        )
        .await;
    let mut tx = fixture
        .db
        .begin()
        .await
        .expect("read-only scan transaction");
    assert!(receipt_complete_nonterminal_keys(&mut tx, None, cutoff, 0)
        .await
        .expect("zero-sized page")
        .is_empty());
    let first = receipt_complete_nonterminal_keys(&mut tx, None, cutoff, 2)
        .await
        .expect("first bounded page");
    assert_eq!(first, expected[..2]);
    let second = receipt_complete_nonterminal_keys(&mut tx, first.last().copied(), cutoff, 2)
        .await
        .expect("second bounded page");
    assert_eq!(second, expected[2..]);
    assert!(
        receipt_complete_nonterminal_keys(&mut tx, second.last().copied(), cutoff, 2)
            .await
            .expect("exhausted scan")
            .is_empty()
    );
    tx.commit().await.expect("finish scan");
    assert_eq!(fixture.count("ingress_messages").await, 6);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NULL")
            .await,
        5
    );
    fixture.close().await;
}

#[tokio::test]
async fn ingress_maintenance_bounded_keyset_scan_sqlite() {
    bounded_keyset_scan(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn ingress_maintenance_bounded_keyset_scan_postgres() {
    if let Some(fixture) = IngressFixture::postgres("maintenance_keyset").await {
        bounded_keyset_scan(fixture).await;
    }
}
