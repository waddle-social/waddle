use super::*;
use crate::pending_delivery::PendingPayload;
use chrono::Utc;
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::message::Message;

fn bare(s: &str) -> BareJid {
    s.parse().expect("valid bare jid")
}

fn direct_target(wire_id: &str, author: &str, archive: &str) -> crate::tombstone::TombstoneTarget {
    crate::tombstone::TombstoneTarget::Direct {
        wire_id: wire_id.to_string(),
        author: bare(author),
        archive: bare(archive),
    }
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

/// Archive ids of the claimed rows, in the order returned — a stable
/// FIFO witness for the batch-claim tests (the row `id` is a random
/// UUID, so we assert on the deterministic `<stanza-id/>` archive id).
fn archive_ids(rows: &[PendingRow]) -> Vec<String> {
    rows.iter()
        .map(|row| match &row.payload {
            PendingPayload::Archived(stanza_id) => stanza_id.id.as_str().to_string(),
            PendingPayload::Transient(_) => panic!("expected Archived row"),
        })
        .collect()
}

#[tokio::test]
async fn claim_batch_claims_only_the_fifo_prefix() {
    // Issue #1220: the flush must drain a large offline backlog in
    // bounded batches, never one unbounded claim. A batch claim of
    // `limit` returns only the oldest `limit` unclaimed rows.
    let store = InMemoryPendingDeliveryStorage::unlimited();
    let recipient = bare("alice@example.com");
    for n in 0..5 {
        store
            .insert(archived_row("alice@example.com", &format!("id-{n}")))
            .await
            .expect("insert ok");
    }
    let session = SmSessionId::new("s1");

    let batch = store
        .claim_batch_for_session(&recipient, &session, 2)
        .await
        .expect("batch claim ok");

    assert_eq!(
        archive_ids(&batch),
        vec!["id-0", "id-1"],
        "a batch of 2 claims only the FIFO prefix, leaving the rest unclaimed"
    );
    // The remaining three rows are still unclaimed and eligible.
    let unclaimed = store
        .list_unclaimed_after(&recipient, None, 100)
        .await
        .expect("list ok");
    assert_eq!(archive_ids(&unclaimed), vec!["id-2", "id-3", "id-4"]);
}

#[tokio::test]
async fn claim_batch_continues_from_where_the_previous_batch_left_off() {
    // Successive batch claims for the same session walk the queue
    // forward: the flush loop relies on this to drain the whole
    // backlog across multiple bounded batches (issue #1220).
    let store = InMemoryPendingDeliveryStorage::unlimited();
    let recipient = bare("alice@example.com");
    for n in 0..5 {
        store
            .insert(archived_row("alice@example.com", &format!("id-{n}")))
            .await
            .expect("insert ok");
    }
    let session = SmSessionId::new("s1");

    let batch1 = store
        .claim_batch_for_session(&recipient, &session, 2)
        .await
        .expect("claim ok");
    let batch2 = store
        .claim_batch_for_session(&recipient, &session, 2)
        .await
        .expect("claim ok");
    let batch3 = store
        .claim_batch_for_session(&recipient, &session, 2)
        .await
        .expect("claim ok");
    let batch4 = store
        .claim_batch_for_session(&recipient, &session, 2)
        .await
        .expect("claim ok");

    assert_eq!(archive_ids(&batch1), vec!["id-0", "id-1"]);
    assert_eq!(archive_ids(&batch2), vec!["id-2", "id-3"]);
    assert_eq!(archive_ids(&batch3), vec!["id-4"], "final short batch");
    assert!(batch4.is_empty(), "drained — nothing left to claim");
}

#[tokio::test]
async fn claim_batch_cross_session_race_sees_disjoint_prefixes() {
    // Q7c per-recipient lock: two recovering resources batch-claiming
    // the same backlog must see disjoint FIFO slices, never the same
    // row twice.
    let store = InMemoryPendingDeliveryStorage::unlimited();
    let recipient = bare("alice@example.com");
    for n in 0..4 {
        store
            .insert(archived_row("alice@example.com", &format!("id-{n}")))
            .await
            .expect("insert ok");
    }
    let s1 = SmSessionId::new("s1");
    let s2 = SmSessionId::new("s2");

    let b1 = store
        .claim_batch_for_session(&recipient, &s1, 2)
        .await
        .expect("claim ok");
    let b2 = store
        .claim_batch_for_session(&recipient, &s2, 2)
        .await
        .expect("claim ok");

    assert_eq!(archive_ids(&b1), vec!["id-0", "id-1"]);
    assert_eq!(
        archive_ids(&b2),
        vec!["id-2", "id-3"],
        "the second session claims the next unclaimed prefix, disjoint from the first"
    );
}

#[tokio::test]
async fn claim_batch_limit_zero_claims_nothing() {
    let store = InMemoryPendingDeliveryStorage::unlimited();
    let recipient = bare("alice@example.com");
    store
        .insert(archived_row("alice@example.com", "id-0"))
        .await
        .expect("insert ok");
    let session = SmSessionId::new("s1");

    let claimed = store
        .claim_batch_for_session(&recipient, &session, 0)
        .await
        .expect("claim ok");
    assert!(claimed.is_empty());
    // The row was not touched — still claimable.
    assert_eq!(store.count(&recipient).await.unwrap(), 1);
    let still_unclaimed = store
        .list_unclaimed_after(&recipient, None, 10)
        .await
        .unwrap();
    assert_eq!(still_unclaimed.len(), 1);
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
async fn mark_notification_outboxed_requires_existing_row() {
    let store = InMemoryPendingDeliveryStorage::unlimited();
    let missing = PendingRowId::fresh();

    assert_eq!(store.mark_notification_outboxed(&missing).await.unwrap(), 0);

    let row = archived_row("alice@example.com", "archive-1");
    let row_id = row.id.clone();
    store.insert(row).await.unwrap();
    assert_eq!(store.mark_notification_outboxed(&row_id).await.unwrap(), 1);
    assert_eq!(store.mark_notification_outboxed(&row_id).await.unwrap(), 0);
    assert!(store
        .list_unoutboxed_archived(16)
        .await
        .expect("unoutboxed")
        .is_empty());
}

#[tokio::test]
async fn delete_row_clears_notification_outboxed_marker() {
    let store = InMemoryPendingDeliveryStorage::unlimited();
    let row = archived_row("alice@example.com", "archive-1");
    let row_id = row.id.clone();
    store.insert(row).await.unwrap();
    assert_eq!(store.mark_notification_outboxed(&row_id).await.unwrap(), 1);
    assert_eq!(store.delete_row(&row_id).await.unwrap(), 1);
    assert_eq!(store.mark_notification_outboxed(&row_id).await.unwrap(), 0);
}

#[tokio::test]
async fn empty_recipient_count_is_zero() {
    let store = InMemoryPendingDeliveryStorage::unlimited();
    assert_eq!(store.count(&bare("nobody@example.com")).await.unwrap(), 0);
}

#[tokio::test]
async fn delete_acked_in_window_only_removes_acked_session_rows() {
    // Locked Q7b SM-ack-keyed deletion: an SM `<a h>` ack with
    // h=N must remove rows where flushed_in_session = current
    // AND outbound_sequence in the acked window (last_acked, N] —
    // and must leave alone:
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
    let removed = store
        .delete_acked_in_window(&session_a, 0, 2)
        .await
        .unwrap();
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
async fn delete_acked_in_window_ignores_other_sessions() {
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
    let removed = store
        .delete_acked_in_window(&session_b, 0, 10)
        .await
        .unwrap();
    assert_eq!(removed, 0);
    assert_eq!(
        store.count(&bare("alice@example.com")).await.unwrap(),
        1,
        "session_a row preserved"
    );
}

#[tokio::test]
async fn delete_acked_in_window_handles_mod_2_32_wrap() {
    // Review F4: the XEP-0198 ack window is mod-2^32, so a valid
    // wrap-spanning ack (h small post-wrap) must delete the pre-wrap
    // rows near u32::MAX too. A numeric `seq <= h` delete would only
    // remove the numerically-small rows and strand the pre-wrap rows
    // claimed, to be later released by the claim-expiry janitor as
    // duplicates.
    let store = InMemoryPendingDeliveryStorage::unlimited();
    let recipient = bare("alice@example.com");
    for n in 0..4 {
        store
            .insert(archived_row("alice@example.com", &format!("id-{n}")))
            .await
            .unwrap();
    }
    let session = SmSessionId::new("s-wrap");
    let claimed = store.claim_for_session(&recipient, &session).await.unwrap();
    assert_eq!(claimed.len(), 4);

    store
        .record_pushed_at(&claimed[0].id, u32::MAX - 1)
        .await
        .unwrap();
    store
        .record_pushed_at(&claimed[1].id, u32::MAX)
        .await
        .unwrap();
    store.record_pushed_at(&claimed[2].id, 1).await.unwrap();
    store.record_pushed_at(&claimed[3].id, 2).await.unwrap();

    // Ack window (u32::MAX - 2, 1]: covers MAX-1, MAX, and 1 — NOT 2.
    let removed = store
        .delete_acked_in_window(&session, u32::MAX - 2, 1)
        .await
        .unwrap();
    assert_eq!(removed, 3, "wrap-spanning window deletes across the wrap");
    let remaining = store.list(&recipient).await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(
        remaining[0].outbound_sequence,
        Some(2),
        "the row past the ack window survives"
    );
}

#[tokio::test]
async fn delete_acked_in_window_non_wrapping_window() {
    let store = InMemoryPendingDeliveryStorage::unlimited();
    let recipient = bare("alice@example.com");
    for n in 0..3 {
        store
            .insert(archived_row("alice@example.com", &format!("id-{n}")))
            .await
            .unwrap();
    }
    let session = SmSessionId::new("s-plain");
    let claimed = store.claim_for_session(&recipient, &session).await.unwrap();
    store.record_pushed_at(&claimed[0].id, 5).await.unwrap();
    store.record_pushed_at(&claimed[1].id, 6).await.unwrap();
    store.record_pushed_at(&claimed[2].id, 7).await.unwrap();

    // Window (5, 6]: exclusive lower bound keeps 5, deletes 6, keeps 7.
    let removed = store.delete_acked_in_window(&session, 5, 6).await.unwrap();
    assert_eq!(removed, 1);
    let mut remaining: Vec<_> = store
        .list(&recipient)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.outbound_sequence)
        .collect();
    remaining.sort();
    assert_eq!(remaining, vec![Some(5), Some(7)]);

    // Empty window (7, 7] deletes nothing.
    let removed = store.delete_acked_in_window(&session, 7, 7).await.unwrap();
    assert_eq!(removed, 0, "an empty (from == to) window is a no-op");
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

fn transient_dm_row(recipient: &str, from: &str, wire_id: &str, body: &str) -> PendingRow {
    let mut m = Message::new(Some(recipient.parse::<jid::Jid>().expect("jid")));
    m.from = Some(from.parse::<jid::Jid>().expect("jid"));
    m.id = Some(xmpp_parsers::message::Id(wire_id.to_string()));
    m.type_ = xmpp_parsers::message::MessageType::Chat;
    m.bodies
        .insert(xmpp_parsers::message::Lang::new(), body.to_string());
    PendingRow {
        id: PendingRowId::fresh(),
        recipient: bare(recipient),
        original_receipt_at: Utc::now(),
        payload: PendingPayload::Transient(Box::new(m)),
        flushed_in_session: None,
        outbound_sequence: None,
    }
}

#[tokio::test]
async fn scrub_for_tombstone_removes_matching_transient_row_only() {
    // F2: promotion (#1097/#1098) parks unacked stanzas in pending
    // delivery; a XEP-0424/0425 retraction must scrub the inline
    // (Transient) copy or the retracted content delivers verbatim at
    // the recipient's next login.
    let store = InMemoryPendingDeliveryStorage::unlimited();
    store
        .insert(transient_dm_row(
            "alice@example.com",
            "bob@elsewhere/x",
            "retract-me",
            "secret",
        ))
        .await
        .unwrap();
    store
        .insert(transient_dm_row(
            "alice@example.com",
            "bob@elsewhere/x",
            "keep-me",
            "safe",
        ))
        .await
        .unwrap();
    // Same wire id, different conversation: scope guard must keep it.
    store
        .insert(transient_dm_row(
            "carol@example.com",
            "bob@elsewhere/x",
            "retract-me",
            "unrelated",
        ))
        .await
        .unwrap();

    let removed = store
        .scrub_for_tombstone(&direct_target(
            "retract-me",
            "bob@elsewhere",
            "alice@example.com",
        ))
        .await
        .unwrap();
    assert_eq!(removed, 1, "exactly the in-scope matching row is removed");

    let alice_rows = store.list(&bare("alice@example.com")).await.unwrap();
    assert_eq!(alice_rows.len(), 1);
    match &alice_rows[0].payload {
        PendingPayload::Transient(m) => {
            assert_eq!(m.id.as_ref().map(|id| id.0.as_str()), Some("keep-me"));
        }
        _ => panic!("expected Transient"),
    }
    assert_eq!(
        store.list(&bare("carol@example.com")).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn scrub_for_tombstone_removes_matching_archived_pointer_row() {
    // Archived rows are MAM pointers keyed by (stanza-id, archive-by).
    // A retraction tombstones the MAM row itself; the pending pointer
    // must go too so the next-login flush doesn't push a stub for a
    // message the recipient never saw.
    let store = InMemoryPendingDeliveryStorage::unlimited();
    store
        .insert(archived_row("alice@example.com", "archive-1"))
        .await
        .unwrap();
    store
        .insert(archived_row("alice@example.com", "archive-2"))
        .await
        .unwrap();
    // Same archive id, different archive owner: out of scope.
    store
        .insert(archived_row("carol@example.com", "archive-1"))
        .await
        .unwrap();

    let removed = store
        .scrub_for_tombstone(&direct_target(
            "archive-1",
            "bob@elsewhere",
            "alice@example.com",
        ))
        .await
        .unwrap();
    assert_eq!(removed, 1);

    let alice_rows = store.list(&bare("alice@example.com")).await.unwrap();
    assert_eq!(alice_rows.len(), 1);
    match &alice_rows[0].payload {
        PendingPayload::Archived(r) => assert_eq!(r.id.as_str(), "archive-2"),
        _ => panic!("expected Archived"),
    }
    assert_eq!(
        store.list(&bare("carol@example.com")).await.unwrap().len(),
        1
    );
}
