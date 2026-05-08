use super::*;
use crate::pending_delivery::PendingPayload;
use chrono::Utc;
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::message::Message;

fn bare(s: &str) -> BareJid {
    s.parse().expect("valid bare jid")
}

fn archived_row(recipient: &str, id: &str) -> PendingRow {
    let archive_jid: jid::Jid = bare(recipient).into();
    PendingRow {
        id: PendingRowId::fresh(),
        recipient: bare(recipient),
        original_receipt_at: Utc::now(),
        payload: PendingPayload::Archived(StanzaId::new(id, archive_jid)),
        flushed_in_session: None,
        outbound_sequence: None,
    }
}

fn transient_row(recipient: &str) -> PendingRow {
    PendingRow {
        id: PendingRowId::fresh(),
        recipient: bare(recipient),
        original_receipt_at: Utc::now(),
        payload: PendingPayload::Transient(Box::new(Message::new(None::<jid::Jid>))),
        flushed_in_session: None,
        outbound_sequence: None,
    }
}

#[tokio::test]
async fn insert_and_list_preserves_fifo_order() {
    let store = InMemoryPendingDeliveryStorage::unlimited();
    for n in 0..5 {
        let outcome = store
            .insert(archived_row("alice@example.com", &format!("id-{n}")))
            .await
            .expect("insert ok");
        assert_eq!(outcome, InsertOutcome::Inserted);
    }
    let rows = store
        .list(&bare("alice@example.com"))
        .await
        .expect("list ok");
    assert_eq!(rows.len(), 5);
    for (n, row) in rows.iter().enumerate() {
        match &row.payload {
            PendingPayload::Archived(r) => assert_eq!(r.id.as_str(), format!("id-{n}")),
            _ => panic!("expected Archived"),
        }
    }
}

#[tokio::test]
async fn quota_exceeded_returns_outcome() {
    let store = InMemoryPendingDeliveryStorage::new(QuotaPolicy::CountCap { max_rows: 2 });
    let recipient = "alice@example.com";
    for n in 0..2 {
        let outcome = store
            .insert(archived_row(recipient, &format!("id-{n}")))
            .await
            .expect("insert ok");
        assert_eq!(outcome, InsertOutcome::Inserted);
    }
    let outcome = store
        .insert(archived_row(recipient, "overflow"))
        .await
        .expect("insert ok");
    assert_eq!(outcome, InsertOutcome::QuotaExceeded);
    // Existing rows preserved (XEP-0160 §3 step 3 — refuse new, keep old).
    assert_eq!(store.count(&bare(recipient)).await.unwrap(), 2);
}

#[tokio::test]
async fn claim_marks_rows_for_session_first_caller_wins() {
    let store = InMemoryPendingDeliveryStorage::unlimited();
    for n in 0..3 {
        store
            .insert(archived_row("alice@example.com", &format!("id-{n}")))
            .await
            .expect("insert ok");
    }
    let session1 = SmSessionId::new("session-1");
    let session2 = SmSessionId::new("session-2");
    let claimed1 = store
        .claim_for_session(&bare("alice@example.com"), &session1)
        .await
        .expect("claim ok");
    let claimed2 = store
        .claim_for_session(&bare("alice@example.com"), &session2)
        .await
        .expect("claim ok");
    assert_eq!(claimed1.len(), 3);
    assert_eq!(claimed2.len(), 0); // first caller drained the unclaimed pool
}

#[tokio::test]
async fn delete_claimed_removes_only_session_rows() {
    let store = InMemoryPendingDeliveryStorage::unlimited();
    let recipient = bare("alice@example.com");
    store
        .insert(archived_row("alice@example.com", "a"))
        .await
        .unwrap();
    store
        .insert(archived_row("alice@example.com", "b"))
        .await
        .unwrap();
    store
        .insert(archived_row("alice@example.com", "c"))
        .await
        .unwrap();

    let session = SmSessionId::new("s1");
    let _ = store.claim_for_session(&recipient, &session).await.unwrap();

    let removed = store.delete_claimed(&session).await.unwrap();
    assert_eq!(removed, 3);
    assert_eq!(store.count(&recipient).await.unwrap(), 0);
}

#[tokio::test]
async fn release_claim_makes_rows_eligible_for_reflush() {
    let store = InMemoryPendingDeliveryStorage::unlimited();
    let recipient = bare("alice@example.com");
    store
        .insert(archived_row("alice@example.com", "a"))
        .await
        .unwrap();
    store
        .insert(archived_row("alice@example.com", "b"))
        .await
        .unwrap();

    let session1 = SmSessionId::new("s1");
    let claimed = store
        .claim_for_session(&recipient, &session1)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 2);

    // Session dies pre-ack — release.
    let released = store.release_claim(&session1).await.unwrap();
    assert_eq!(released, 2);

    // A new session can now claim.
    let session2 = SmSessionId::new("s2");
    let reclaimed = store
        .claim_for_session(&recipient, &session2)
        .await
        .unwrap();
    assert_eq!(reclaimed.len(), 2);
}

#[tokio::test]
async fn transient_payload_round_trips() {
    let store = InMemoryPendingDeliveryStorage::unlimited();
    store
        .insert(transient_row("alice@example.com"))
        .await
        .unwrap();
    let rows = store.list(&bare("alice@example.com")).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].payload.is_transient());
}

#[tokio::test]
async fn empty_recipient_count_is_zero() {
    let store = InMemoryPendingDeliveryStorage::unlimited();
    assert_eq!(store.count(&bare("nobody@example.com")).await.unwrap(), 0);
}

#[tokio::test]
async fn delete_acked_through_only_removes_acked_session_rows() {
    // Locked Q7b SM-ack-keyed deletion: an SM `<a h>` ack with
    // h=N must remove rows where flushed_in_session = current
    // AND outbound_sequence <= N — and must leave alone:
    // - rows with outbound_sequence = NULL (claimed but not yet
    //   pushed),
    // - rows with outbound_sequence > N (pushed but not yet
    //   ack'd),
    // - rows claimed by a different session.
    let store = InMemoryPendingDeliveryStorage::unlimited();
    let recipient = bare("alice@example.com");
    for n in 0..4 {
        store
            .insert(archived_row("alice@example.com", &format!("id-{n}")))
            .await
            .unwrap();
    }
    let session_a = SmSessionId::new("s-a");
    let claimed = store
        .claim_for_session(&recipient, &session_a)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 4);

    // Three rows pushed and assigned outbound_sequences 1, 2, 3;
    // fourth row was claimed but the recipient's main loop never
    // got around to pushing it (e.g. socket died) — sequence
    // stays NULL.
    store.record_pushed_at(&claimed[0].id, 1).await.unwrap();
    store.record_pushed_at(&claimed[1].id, 2).await.unwrap();
    store.record_pushed_at(&claimed[2].id, 3).await.unwrap();
    // claimed[3] left without record_pushed_at.

    // SM ack with h=2 covers the first two only.
    let removed = store.delete_acked_through(&session_a, 2).await.unwrap();
    assert_eq!(removed, 2);
    let remaining = store.list(&recipient).await.unwrap();
    assert_eq!(remaining.len(), 2);
    // Surviving rows: the one with outbound_sequence=3 and the
    // unsequenced one.
    let mut seen_seq3 = false;
    let mut seen_unseq = false;
    for row in &remaining {
        match row.outbound_sequence {
            Some(3) => seen_seq3 = true,
            None => seen_unseq = true,
            other => panic!("unexpected outbound_sequence: {other:?}"),
        }
    }
    assert!(seen_seq3, "outbound_sequence=3 row survives ack(h=2)");
    assert!(
        seen_unseq,
        "unsequenced (claimed but unpushed) row survives ack"
    );
}

#[tokio::test]
async fn delete_acked_through_ignores_other_sessions() {
    // Two sessions for the same recipient (e.g. parallel
    // resources): an ack from session A must not affect rows
    // claimed by session B.
    let store = InMemoryPendingDeliveryStorage::unlimited();
    store
        .insert(archived_row("alice@example.com", "x"))
        .await
        .unwrap();
    let session_a = SmSessionId::new("s-a");
    let claimed_a = store
        .claim_for_session(&bare("alice@example.com"), &session_a)
        .await
        .unwrap();
    store.record_pushed_at(&claimed_a[0].id, 5).await.unwrap();

    // A different session's ack with h=10 must not touch
    // session_a's row.
    let session_b = SmSessionId::new("s-b");
    let removed = store.delete_acked_through(&session_b, 10).await.unwrap();
    assert_eq!(removed, 0);
    assert_eq!(
        store.count(&bare("alice@example.com")).await.unwrap(),
        1,
        "session_a row preserved"
    );
}

#[tokio::test]
async fn record_pushed_at_is_idempotent_per_row() {
    // Locked Q7b: outbound_sequence updates are only valid when
    // they progress forward, but the storage layer is permissive —
    // it just sets the value. The invariant "first write wins" is
    // maintained at the call site (the recipient main loop calls
    // record_outbound exactly once per stanza). Here we verify the
    // storage layer preserves the latest write.
    let store = InMemoryPendingDeliveryStorage::unlimited();
    store
        .insert(archived_row("alice@example.com", "a"))
        .await
        .unwrap();
    let session = SmSessionId::new("s");
    let claimed = store
        .claim_for_session(&bare("alice@example.com"), &session)
        .await
        .unwrap();
    let id = &claimed[0].id;
    store.record_pushed_at(id, 7).await.unwrap();
    let rows = store.list(&bare("alice@example.com")).await.unwrap();
    assert_eq!(rows[0].outbound_sequence, Some(7));
    // Latest write wins (no monotonicity check at storage layer).
    store.record_pushed_at(id, 12).await.unwrap();
    let rows = store.list(&bare("alice@example.com")).await.unwrap();
    assert_eq!(rows[0].outbound_sequence, Some(12));
}
