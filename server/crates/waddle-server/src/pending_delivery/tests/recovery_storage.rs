//! Durable rediscovery distinguishes an interrupted claim from offered custody.

use super::*;
use waddle_xmpp::pending_delivery::storage::PendingClaim;

async fn restart_preserves_recovery_and_offer_fences(database_url: &str) {
    let storage = DatabasePendingDeliveryStorage::open(Some(database_url), QuotaPolicy::Unlimited)
        .await
        .expect("open pending recovery storage");
    // Ordered claiming resolves this schema even for transient rows. These
    // tests exercise pending custody rather than archive materialization.
    storage.database().guard().await.expect("schema connection").execute(
        "CREATE TABLE mam_messages (id TEXT PRIMARY KEY, room_jid TEXT NOT NULL, stanza_id TEXT, archive_seq BIGINT NOT NULL)",
        (),
    ).await.expect("archive ordering schema");
    let recipient = bare("alice@example.com");
    let unoffered = transient_row("alice@example.com", "interrupted before offer");
    let offered = transient_row("alice@example.com", "already offered to writer");
    let unoffered_id = unoffered.id.clone();
    let offered_id = offered.id.clone();
    storage
        .insert(unoffered)
        .await
        .expect("insert interrupted row");
    storage.insert(offered).await.expect("insert offered row");
    let session = SmSessionId::new("still-live-after-restart");
    let token = PendingClaimToken::fresh();
    let claimed = storage
        .claim_archive_ordered_batch_for_session(&recipient, &session, &token, 2)
        .await
        .expect("claim both rows in one ordered batch");
    assert_eq!(claimed.len(), 2);
    let interrupted_claim = PendingClaim {
        row_id: unoffered_id.clone(),
        session: session.clone(),
        token,
    };
    let offered_claim = PendingClaim {
        row_id: offered_id.clone(),
        session: session.clone(),
        token,
    };
    assert!(storage
        .mark_claim_offered(&offered_claim)
        .await
        .expect("reserve offer"));
    assert!(storage
        .mark_claim_offered(&offered_claim)
        .await
        .expect("idempotent exact offer reservation"));
    storage
        .database()
        .guard()
        .await
        .expect("age connection")
        .execute(
            "UPDATE pending_delivery SET claimed_at_ms = ? WHERE recipient_jid = ?",
            crate::db_params![100_i64, recipient.to_string()],
        )
        .await
        .expect("age durable claims without sleeping");
    assert!(storage
        .list_unoffered_claims(&recipient, None, 99, 8)
        .await
        .expect("fresh claims excluded")
        .is_empty());
    drop(storage);

    // Drop every storage handle and reopen the same SQLite file / PG schema.
    // No in-memory retry work survives this boundary.
    let restarted =
        DatabasePendingDeliveryStorage::open(Some(database_url), QuotaPolicy::Unlimited)
            .await
            .expect("reopen pending recovery storage");
    let rows = restarted
        .list(&recipient)
        .await
        .expect("durable claims after restart");
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .all(|row| row.flushed_in_session.as_ref() == Some(&session)
            && row.outbound_sequence.is_none()));
    assert!(restarted
        .list_orphaned_claims(std::slice::from_ref(&session), 101)
        .await
        .expect("ordinary live-session claim sweep")
        .is_empty());
    assert_eq!(
        restarted
            .list_unoffered_claims(&recipient, None, 100, 8)
            .await
            .expect("recover live-session interrupted claim"),
        vec![interrupted_claim.clone()]
    );
    assert!(restarted
        .list_unoffered_claims(&bare("other@example.com"), None, 100, 8)
        .await
        .expect("recipient-local recovery")
        .is_empty());
    assert!(restarted
        .list_unoffered_claims(&recipient, Some(&unoffered_id), 100, 8)
        .await
        .expect("exclusive recovery cursor")
        .is_empty());
    assert!(restarted
        .mark_claim_offered(&offered_claim)
        .await
        .expect("offer reservation survives restart idempotently"));
    assert_eq!(
        restarted
            .release_unpushed_row_if_session(&offered_id, &session, &token)
            .await
            .expect("generic recovery cannot release an offered row"),
        0
    );

    // The exact interrupted claim can be recovered while the stream is live.
    assert_eq!(
        restarted
            .release_unpushed_row_if_session(&unoffered_id, &session, &token)
            .await
            .expect("release interrupted claim"),
        1
    );
    assert_eq!(
        restarted
            .record_pushed_at(&offered_id, 1)
            .await
            .expect("writer completes offered row"),
        1
    );
    let replacement_token = PendingClaimToken::fresh();
    let replacement = restarted
        .claim_archive_ordered_batch_for_session(&recipient, &session, &replacement_token, 1)
        .await
        .expect("reclaim on the same resumed SM stream");
    assert_eq!(replacement.len(), 1);
    assert_eq!(replacement[0].id, unoffered_id);

    // A stale process retains the same row/session pair, but its old token
    // must neither reserve nor release the replacement's durable claim.
    assert!(!restarted
        .mark_claim_offered(&interrupted_claim)
        .await
        .expect("reject stale offer reservation"));
    assert_eq!(
        restarted
            .release_unqueued_offer(&interrupted_claim)
            .await
            .expect("reject stale unqueued-offer release"),
        0
    );
    assert_eq!(
        restarted
            .release_unpushed_row_if_session(&unoffered_id, &session, &token)
            .await
            .expect("reject stale interrupted-claim release"),
        0
    );
    let replacement_claim = PendingClaim {
        row_id: unoffered_id.clone(),
        session: session.clone(),
        token: replacement_token,
    };
    assert_eq!(
        restarted
            .list_unoffered_claims(&recipient, None, i64::MAX, 8)
            .await
            .expect("replacement still owns the unoffered claim"),
        vec![replacement_claim.clone()]
    );
    assert!(restarted
        .mark_claim_offered(&replacement_claim)
        .await
        .expect("reserve replacement offer"));
    assert!(restarted
        .mark_claim_offered(&replacement_claim)
        .await
        .expect("repeat replacement reservation"));
    assert!(restarted
        .list_unoffered_claims(&recipient, None, i64::MAX, 8)
        .await
        .expect("offered unsequenced replacement is excluded")
        .is_empty());
    let preserved = restarted
        .list(&recipient)
        .await
        .expect("preserved replacement custody");
    let pending = preserved
        .iter()
        .find(|row| row.id == unoffered_id)
        .expect("replacement row");
    assert_eq!(pending.flushed_in_session.as_ref(), Some(&session));
    assert!(pending.outbound_sequence.is_none());
}

#[tokio::test]
async fn sqlite_unoffered_recovery_survives_restart_and_fences_offered_claims() {
    let directory = tempfile::tempdir().expect("recovery database directory");
    let database_url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("recovery.db").display()
    );
    restart_preserves_recovery_and_offer_fences(&database_url).await;
}

#[tokio::test]
async fn postgres_unoffered_recovery_survives_restart_and_fences_offered_claims() {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (pending recovery restart)");
        return;
    };
    let (schema, scoped_url) = create_postgres_test_schema(&database_url, "pending_recovery").await;
    restart_preserves_recovery_and_offer_fences(&scoped_url).await;
    drop_postgres_test_schema(&database_url, &schema).await;
}
