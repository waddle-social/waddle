//! XEP-0160: Best Practices for Handling Offline Messages — dedicated suite.
//!
//! Tracks issue #209 (durable offline DM delivery and reconnect semantics).
//!
//! The locked design: MAM is the canonical archive, `pending_delivery` is a
//! thin durable pointer/payload table flushed on first non-negative-priority
//! presence of a fresh (non-SM-resumed) session.
//!
//! Tests still marked `#[ignore]` cover behaviors that ship in slice (d) /
//! follow-up PRs (SM persistence, restart durability, SM-expiry promotion,
//! Q7c re-flush). Remove the attribute as each capability lands.
//!
//! Citations refer to `xeps/xep-0160.xml` unless otherwise noted.

use chrono::{TimeZone, Utc};
use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use waddle_xmpp::disco::{server_features, Feature};
use waddle_xmpp::pending_delivery::flush::{
    build_replay_stanza, MaterializedPayload, ReplayReason,
};
use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow, PendingRowId, SmSessionId};
use waddle_xmpp::protocol::dm_routing::{
    classify_dm_intake, ArchiveDecision, CarbonsDecision, InboxDecision, LiveDecision,
    OnlineResources, PendingDecision,
};
use waddle_xmpp::protocol::session_state::Blocklist;
use waddle_xmpp::xep::xep0334::{add_hint, Hint};
use waddle_xmpp::xep::NS_DELAY;
use waddle_xmpp_core::xep0359::{build_stanza_id_element, NS_SID};
use xmpp_parsers::message::{Message, MessageType};

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

fn bare(s: &str) -> BareJid {
    s.parse().expect("bare jid")
}
fn full(s: &str) -> FullJid {
    s.parse().expect("full jid")
}
fn dm(from: &str, to: &str, kind: MessageType, body: Option<&str>) -> Message {
    let mut m = Message::new(Some(to.parse::<Jid>().expect("jid")));
    m.from = Some(from.parse::<Jid>().expect("jid"));
    m.type_ = kind;
    if let Some(b) = body {
        m.bodies
            .insert(xmpp_parsers::message::Lang::new(), b.to_string());
    }
    m
}
fn online_with_priority(priority: i8) -> OnlineResources {
    OnlineResources::from_pairs([(full("alice@example.com/web"), priority)])
}

// -----------------------------------------------------------------------------
// §5 Service Discovery — feature advertisement
// -----------------------------------------------------------------------------

/// XEP-0160 §5: server SHOULD advertise `msgoffline`.
#[test]
fn xep0160_server_advertises_msgoffline_feature() {
    assert!(server_features().contains(&Feature::offline_messages()));
}

// -----------------------------------------------------------------------------
// §3 Process Flow — intake (storage trigger) — covered by classifier tests
// -----------------------------------------------------------------------------

/// §3 step 2/4: store offline when recipient has zero resources with
/// non-negative presence priority at intake time.
#[test]
fn xep0160_stores_offline_when_no_non_negative_resource_online() {
    let msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        Some("hi"),
    );
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::Mam);
    assert_eq!(routing.pending, PendingDecision::Archived);
    assert_eq!(routing.live, LiveDecision::None);
}

/// §3 step 2 (note): negative-priority resources do not count as available.
#[test]
fn xep0160_treats_negative_priority_resources_as_unavailable_for_storage() {
    let msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        Some("hi"),
    );
    let routing = classify_dm_intake(&msg, &online_with_priority(-1), &Blocklist::empty());
    assert_eq!(routing.pending, PendingDecision::Archived);
    assert_eq!(routing.live, LiveDecision::None);
}

// XEP-0160 §3 step 3 quota → `<service-unavailable/>` bounce is
// covered by the storage-trait contract (`InsertOutcome::QuotaExceeded`
// when over `QuotaPolicy::CountCap`) plus the dedicated server-side
// integration test
// `sm_promotion::tests::bounces_service_unavailable_when_quota_exceeded`
// in `server/crates/waddle-server/src/sm_promotion.rs` (which exercises
// the quota → bounce path through the actual flush/promotion pipeline).
// See also the storage-layer tests
// `pending_delivery::storage::tests::quota_exceeded_returns_outcome` and
// `db_storage_quota_returns_quota_exceeded_outcome`.
#[tokio::test]
async fn xep0160_queue_full_returns_service_unavailable_to_sender() {
    use std::sync::Arc;
    use waddle_xmpp::pending_delivery::storage::{
        InMemoryPendingDeliveryStorage, PendingDeliveryStorage,
    };
    use waddle_xmpp::pending_delivery::{InsertOutcome, QuotaPolicy};
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::new(QuotaPolicy::CountCap {
            max_rows: 2,
        }));
    for n in 0..2 {
        let outcome = storage
            .insert(transient_row("alice@example.com", &format!("hi-{n}")))
            .await
            .expect("insert ok");
        assert_eq!(outcome, InsertOutcome::Inserted);
    }
    // Third insert exceeds the per-recipient cap; the storage layer
    // surfaces QuotaExceeded so the routing layer (interpret.rs ::
    // OfflineDeliveryHandler) can bounce <service-unavailable/> per
    // XEP-0160 §3 step 3 + RFC 6120 §8.3.
    let outcome = storage
        .insert(transient_row("alice@example.com", "overflow"))
        .await
        .expect("insert ok");
    assert_eq!(outcome, InsertOutcome::QuotaExceeded);
}

// -----------------------------------------------------------------------------
// §4 Handling of Message Types — eligibility matrix
// -----------------------------------------------------------------------------

#[test]
fn xep0160_chat_messages_are_stored_offline() {
    let msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        Some("hi"),
    );
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::Mam);
    assert_eq!(routing.pending, PendingDecision::Archived);
}

#[test]
fn xep0160_normal_messages_are_stored_offline() {
    let msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Normal,
        Some("hi"),
    );
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::Mam);
    assert_eq!(routing.pending, PendingDecision::Archived);
}

#[test]
fn xep0160_chat_states_only_message_is_not_stored_offline() {
    let mut msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        None,
    );
    msg.payloads
        .push(Element::builder("composing", "http://jabber.org/protocol/chatstates").build());
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::None);
    assert_eq!(routing.pending, PendingDecision::None);
    assert_eq!(routing.inbox, InboxDecision::None);
}

#[test]
fn xep0160_groupchat_messages_are_not_stored_offline() {
    let msg = dm(
        "room@conf.example.com/bob",
        "alice@example.com",
        MessageType::Groupchat,
        Some("hi"),
    );
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::None);
    assert_eq!(routing.pending, PendingDecision::None);
}

#[test]
fn xep0160_headline_messages_are_not_stored_offline() {
    let msg = dm(
        "system@example.com",
        "alice@example.com",
        MessageType::Headline,
        Some("notif"),
    );
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::None);
    assert_eq!(routing.pending, PendingDecision::None);
}

/// §4 + RFC 6121 §8.5.2.1.4: error to fully-offline → silently dropped.
#[test]
fn xep0160_error_messages_are_silently_dropped_when_recipient_offline() {
    let mut msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Error,
        None,
    );
    add_hint(&mut msg, Hint::Store);
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::None);
    assert_eq!(routing.pending, PendingDecision::None);
    assert_eq!(routing.live, LiveDecision::None);
    assert_eq!(routing.inbox, InboxDecision::None);
}

// -----------------------------------------------------------------------------
// §3 Process Flow — replayed wire shape (covered by flush builder tests)
// -----------------------------------------------------------------------------

fn fixed_receipt() -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(2026, 5, 1, 12, 30, 0).unwrap()
}

fn transient_row(recipient: &str, body: &str) -> PendingRow {
    let mut m = dm("bob@elsewhere/x", recipient, MessageType::Chat, Some(body));
    m.id = Some(xmpp_parsers::message::Id("origin-id-1".to_string()));
    PendingRow {
        id: PendingRowId::fresh(),
        recipient: bare(recipient),
        original_receipt_at: fixed_receipt(),
        payload: PendingPayload::Transient(Box::new(m)),
        flushed_in_session: None,
        outbound_sequence: None,
    }
}

#[test]
fn xep0160_flushed_message_carries_delay_with_original_receipt_time() {
    let row = transient_row("alice@example.com", "hi");
    let payload = MaterializedPayload::from_transient(&row).expect("transient");
    let replayed = build_replay_stanza(
        payload,
        "example.com",
        row.original_receipt_at,
        ReplayReason::OfflineStorage,
    );
    let delay = replayed
        .payloads
        .iter()
        .find(|p| p.name() == "delay" && p.ns() == NS_DELAY)
        .expect("delay element appended");
    // XEP-0203 §4.1: from = server domain.
    assert_eq!(delay.attr("from"), Some("example.com"));
    // XEP-0082 §3.2 BNF: UTC stamps use the literal `Z` form; receipt
    // time, NOT flush time.
    assert_eq!(delay.attr("stamp"), Some("2026-05-01T12:30:00Z"));
}

#[test]
fn xep0160_flushed_message_preserves_original_to_attribute() {
    // §3 example preserves the sender's original `to` (locked Q5a).
    let row = transient_row("alice@example.com", "hi");
    let payload = MaterializedPayload::from_transient(&row).expect("transient");
    let replayed = build_replay_stanza(
        payload,
        "example.com",
        row.original_receipt_at,
        ReplayReason::OfflineStorage,
    );
    let to = replayed.to.expect("to preserved");
    assert_eq!(to.to_string(), "alice@example.com");
}

#[test]
fn xep0160_archived_flush_includes_xep0359_stanza_id() {
    // Locked Q5c: Archived flushes carry the server-stamped XEP-0359
    // <stanza-id by='recipient'/> matching the MAM ID. Build a
    // synthetic resolved-archived payload with the stamp.
    let recipient = bare("alice@example.com");
    let mut m = dm(
        "bob@elsewhere/x",
        recipient.as_str(),
        MessageType::Chat,
        Some("hi"),
    );
    m.payloads.push(build_stanza_id_element(
        "mam-id-1",
        &Jid::from(recipient.clone()),
    ));
    let payload = MaterializedPayload::Archived(Box::new(m));
    let replayed = build_replay_stanza(
        payload,
        "example.com",
        fixed_receipt(),
        ReplayReason::OfflineStorage,
    );
    let stanza_id = replayed
        .payloads
        .iter()
        .find(|p| p.name() == "stanza-id" && p.ns() == NS_SID)
        .expect("stanza-id preserved on archived flush");
    assert_eq!(stanza_id.attr("id"), Some("mam-id-1"));
    assert_eq!(stanza_id.attr("by"), Some("alice@example.com"));
}

#[test]
fn xep0160_transient_flush_omits_xep0359_stanza_id() {
    // Locked Q5c: Transient flushes do NOT carry a server-stamped
    // stanza-id (no MAM row exists for them).
    let row = transient_row("alice@example.com", "hi");
    let payload = MaterializedPayload::from_transient(&row).expect("transient");
    let replayed = build_replay_stanza(
        payload,
        "example.com",
        row.original_receipt_at,
        ReplayReason::OfflineStorage,
    );
    let has_stanza_id = replayed
        .payloads
        .iter()
        .any(|p| p.name() == "stanza-id" && p.ns() == NS_SID);
    assert!(!has_stanza_id, "Transient flushes must omit <stanza-id/>");
}

#[test]
fn xep0160_flushed_message_preserves_sender_extensions() {
    // Locked Q5e: server preserves all sender-set extension elements.
    let mut row = transient_row("alice@example.com", "secret");
    if let PendingPayload::Transient(msg) = &mut row.payload {
        msg.payloads
            .push(Element::builder("custom", "urn:test:custom").build());
    }
    let payload = MaterializedPayload::from_transient(&row).expect("transient");
    let replayed = build_replay_stanza(
        payload,
        "example.com",
        row.original_receipt_at,
        ReplayReason::OfflineStorage,
    );
    assert!(replayed
        .payloads
        .iter()
        .any(|p| p.ns() == "urn:test:custom" && p.name() == "custom"));
}

// -----------------------------------------------------------------------------
// §3 Process Flow — flush trigger (slice (d) integration scope)
// -----------------------------------------------------------------------------

/// Locked Q7a + Q7d: the offline-flush trigger fires AT MOST ONCE
/// per fresh session, gated by a connection-entry CAS
/// (`ConnectionEntry::claim_offline_flush`). Repeat presence
/// updates (priority transitions including `-1 → +1`, status text
/// changes, …) MUST observe `false` and skip the flush.
///
/// This pins the CAS contract used by the presence handler in
/// `server/crates/waddle-server/src/server/routes/websocket/handlers/presence.rs::maybe_flush_pending_delivery`,
/// which gates the actual flush on `priority >= 0` AND on this
/// CAS returning `true`. The full presence-transition simulation
/// (priority change events going through the live registry +
/// presence handler) lives in waddle-server; this trait-level
/// test verifies the building block — the second
/// `claim_offline_flush()` call ALWAYS returns false, regardless
/// of how many transitions happened between calls. Any future
/// code that re-arms the CAS would need to update this test.
/// (Copilot review on PR #362: previously two redundant tests
/// covered this same property.)
#[test]
fn xep0160_claim_offline_flush_cas_fires_once_per_connection() {
    use waddle_xmpp::registry::ConnectionEntry;
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    let entry = ConnectionEntry::new(tx);
    // First non-negative-priority presence wins; the presence
    // handler triggers a flush only when this CAS returns true.
    assert!(entry.claim_offline_flush(), "first call wins");
    // Subsequent presence updates (priority transitions, status
    // text changes, …) MUST observe `false` and skip the flush.
    assert!(
        !entry.claim_offline_flush(),
        "second call (any cause: priority transition, status update) must NOT re-flush"
    );
    assert!(
        !entry.claim_offline_flush(),
        "third+ calls remain idempotent"
    );
}

// XEP-0160 SM-resumed session does NOT re-flush pending_delivery: covered
// by the `ConnectionEntry::claim_offline_flush` CAS contract above plus
// the presence-handler wiring in
// `server/crates/waddle-server/src/server/routes/websocket/handlers/presence.rs`,
// which only calls `maybe_flush_pending_delivery` on first non-negative
// presence of a connection. A resumed session re-uses the same
// `ConnectionEntry` so its `offline_flushed` AtomicBool stays `true`
// (was claimed on first presence of the original session).

/// Locked Q7c: when two resources race their first presence, the
/// per-user lock built into `claim_for_session` ensures only the
/// first caller sees the unclaimed pool — the second sees empty.
#[tokio::test]
async fn xep0160_concurrent_resources_first_presence_wins_via_lock() {
    use std::sync::Arc;
    use waddle_xmpp::pending_delivery::storage::{
        InMemoryPendingDeliveryStorage, PendingDeliveryStorage,
    };
    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let recipient = bare("alice@example.com");
    for n in 0..3 {
        storage
            .insert(transient_row("alice@example.com", &format!("msg-{n}")))
            .await
            .unwrap();
    }
    let session_a = SmSessionId::new("alice@example.com/laptop");
    let session_b = SmSessionId::new("alice@example.com/web");
    let claimed_a = storage
        .claim_for_session(&recipient, &session_a)
        .await
        .unwrap();
    let claimed_b = storage
        .claim_for_session(&recipient, &session_b)
        .await
        .unwrap();
    assert_eq!(claimed_a.len(), 3, "first claimer wins all unclaimed rows");
    assert_eq!(
        claimed_b.len(),
        0,
        "second claimer sees empty pool (per-user lock)"
    );
}

#[tokio::test]
async fn xep0160_pending_row_survives_pre_ack_session_death_for_reflush() {
    // Locked Q7b + Q7c (issue #209 PR #347): when a recovering
    // session claims a pending_delivery row, has its flush stanza
    // pushed (`record_pushed_at` stamps the outbound counter), and
    // then the session dies BEFORE the SM `<a h>` ack arrives, the
    // SM-expiry janitor / shutdown drain calls `release_claim` which
    // restores the row to the unclaimed pool. A subsequent resource
    // can then re-claim and re-flush the same row — its content is
    // preserved exactly because deletion is gated on SM-ack via
    // `delete_acked_in_window`, not on push.
    use std::sync::Arc;
    use waddle_xmpp::pending_delivery::storage::{
        InMemoryPendingDeliveryStorage, PendingDeliveryStorage,
    };

    let storage: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let recipient = bare("alice@example.com");
    storage
        .insert(transient_row("alice@example.com", "missed during detach"))
        .await
        .unwrap();

    // Session-A claims and pushes (recipient main loop stamped seq=4).
    let session_a = SmSessionId::new("alice@example.com/laptop");
    let claimed_a = storage
        .claim_for_session(&recipient, &session_a)
        .await
        .unwrap();
    assert_eq!(claimed_a.len(), 1);
    let row_id = claimed_a[0].id.clone();
    storage.record_pushed_at(&row_id, 4).await.unwrap();

    // Session-A dies pre-ack. Janitor / shutdown drain releases.
    let released = storage.release_claim(&session_a).await.unwrap();
    assert_eq!(released, 1);

    // The row's still in storage with flushed_in_session = NULL.
    let rows = storage.list(&recipient).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].flushed_in_session.is_none(),
        "release_claim cleared the dead session's tag"
    );

    // Session-B (a new resource) recovers and re-claims.
    let session_b = SmSessionId::new("alice@example.com/web");
    let claimed_b = storage
        .claim_for_session(&recipient, &session_b)
        .await
        .unwrap();
    assert_eq!(claimed_b.len(), 1);
    assert_eq!(
        claimed_b[0].id, row_id,
        "same row, preserved across pre-ack session death"
    );

    // Session-B's flush stanza assigned outbound seq=2 (different
    // counter on a different SM stream). On its SM ack, the row is
    // finally deleted.
    storage.record_pushed_at(&row_id, 2).await.unwrap();
    let removed = storage
        .delete_acked_in_window(&session_b, 0, 2)
        .await
        .unwrap();
    assert_eq!(removed, 1);
    assert_eq!(storage.count(&recipient).await.unwrap(), 0);
}

// -----------------------------------------------------------------------------
// XEP-0334 hint interactions (locked Q3 / Q4) — covered by classifier tests
// -----------------------------------------------------------------------------

#[test]
fn xep0160_no_store_hint_skips_pending_delivery() {
    let mut msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        Some("ephemeral"),
    );
    add_hint(&mut msg, Hint::NoStore);
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::None);
    assert_eq!(routing.pending, PendingDecision::None);
    assert_eq!(routing.inbox, InboxDecision::None);
}

#[test]
fn xep0160_no_permanent_store_uses_transient_pending_payload() {
    let mut msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        Some("off the record"),
    );
    add_hint(&mut msg, Hint::NoPermanentStore);
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::None);
    assert_eq!(routing.pending, PendingDecision::Transient);
    // Locked Q10b: Transient leaves no inbox trace.
    assert_eq!(routing.inbox, InboxDecision::None);
}

#[test]
fn xep0160_store_hint_forces_offline_storage_except_for_error_type() {
    // Headline + <store/> override.
    let mut msg = dm(
        "system@example.com",
        "alice@example.com",
        MessageType::Headline,
        None,
    );
    add_hint(&mut msg, Hint::Store);
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::Mam);

    // Error + <store/> — XEP-0334 §6 ¶3 says hints in error stanzas are ignored.
    let mut err = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Error,
        None,
    );
    add_hint(&mut err, Hint::Store);
    let routing = classify_dm_intake(&err, &OnlineResources::empty(), &Blocklist::empty());
    assert_eq!(routing.archive, ArchiveDecision::None);
    assert_eq!(routing.pending, PendingDecision::None);
}

#[test]
fn xep0160_hints_in_error_stanzas_are_ignored() {
    // XEP-0334 §6 ¶3: any hint inside type='error' is ignored; verify
    // <no-copy/>, <no-store/>, <store/> all have no effect on routing.
    for hint in [
        Hint::NoCopy,
        Hint::NoStore,
        Hint::Store,
        Hint::NoPermanentStore,
    ] {
        let mut err = dm(
            "bob@elsewhere/x",
            "alice@example.com/web",
            MessageType::Error,
            None,
        );
        add_hint(&mut err, hint);
        let routing = classify_dm_intake(&err, &online_with_priority(1), &Blocklist::empty());
        // Error stanzas with <no-copy/> still suppress carbons (because
        // error type itself is not carbons-eligible — the hint matrix
        // is irrelevant here, the type guard already disallows it).
        assert_eq!(
            routing.carbons,
            CarbonsDecision::Suppressed,
            "hint={hint:?}"
        );
        // Storage-related hints are uniformly ignored on error.
        assert_eq!(routing.archive, ArchiveDecision::None, "hint={hint:?}");
        assert_eq!(routing.pending, PendingDecision::None, "hint={hint:?}");
    }
}

// -----------------------------------------------------------------------------
// XEP-0198 SM-expiry promotion (locked Q2 / Q6) — slice (d) follow-up
// -----------------------------------------------------------------------------

// XEP-0160 SM-expiry promotion path (locked Q6 = B priority chain:
// alt-resource → offline-storage → service-unavailable bounce) is
// covered by the dedicated `sm_promotion::tests` module in
// `server/crates/waddle-server/src/sm_promotion.rs`:
//
//   - `promotes_to_alt_resource_when_one_is_online` — Q6 step 1
//   - `promotes_to_pending_delivery_when_no_alt_resource` — Q6 step 2
//   - `bounces_service_unavailable_when_quota_exceeded` — Q6 step 3
//   - `promoted_pending_row_carries_per_stanza_original_receipt_at`
//     — Q6c receipt-time preservation (issue #209 PR #361)
//   - `xep0160_promoted_stanzas_carry_original_receipt_time_in_delay`
//     in `pending_delivery::tests` — full e2e flow from row insert
//     through flush replay through SM-promote
//
// The promotion path lives in waddle-server (it depends on the
// ConnectionRegistry and PendingDeliveryStorage); the dedicated
// XEP-0160 suite here covers the classifier (intake) half and
// pointer-comments the promotion half to its test home.

// XEP-0160 Q6b "promotion filter delegates to classify_dm_intake" is
// pinned by the structural fact that `sm_promotion::promote_one`
// calls `classify_dm_intake(message, online, blocklist)` directly —
// see `server/crates/waddle-server/src/sm_promotion.rs:155` (line
// number stable since PR #346). Any Q6 promotion test in
// `sm_promotion::tests::*` is therefore also a classifier-reuse
// test by construction. Trying to re-test it here would just be
// asserting against a lint-suppressed re-export of the
// classifier, which adds no value.

// XEP-0160 promoted stanzas carrying their original receipt time on
// the XEP-0203 `<delay/>` is now covered by the storage-trait
// contract on `DetachedUnackedStanza.original_receipt_at` (issue
// #209 PR #361) and the round-trip persistence test
// `persistence::tests::persisted_unacked_round_trips_original_receipt_at`
// in `server/crates/waddle-xmpp/src/stream_management/persistence.rs`.
// The end-to-end SM-promotion → flush wire shape lives in the server
// crate (it requires the Q6 promotion path which is in waddle-server),
// and is exercised via the existing sm_promotion unit tests +
// `flushed_message_carries_delay_with_original_receipt_time` above
// (which tests the same `build_replay_stanza` wire shape consumed by
// the promotion path).

// -----------------------------------------------------------------------------
// Server restart durability (locked Q8 = B) — slice (d) follow-up
// -----------------------------------------------------------------------------

// XEP-0160 Q8 = B `pending_delivery` rows survive a process restart
// is covered by the dedicated server-side integration test
// `pending_delivery::tests::xep0160_pending_delivery_survives_server_restart`
// in `server/crates/waddle-server/src/pending_delivery.rs`. That test
// uses a `tempfile`-backed SQLite path: insert → drop the storage
// handle → reopen the SAME path → assert the row is still there.
// This is the actual "process restart" semantic.
//
// In-memory backends (the default in waddle-xmpp's test fixtures)
// are per-handle and therefore can't model restart durability —
// only the file-backed Database backend in waddle-server can. The
// trait contract is identical, but the durability assertion only
// holds for backends with on-disk storage.

// XEP-0160 SM session resumability across server restart is covered
// by the dedicated SM-persistence tests in
// `waddle_xmpp::stream_management::persistence::tests`:
//   - `upsert_get_round_trip` — session round-trip
//   - `persisted_unacked_round_trips_original_receipt_at` —
//     unacked queue + original_receipt_at round-trip (issue #209 PR #361)
//   - `delete_session_clears_unacked_too` — referential integrity
// And by `InMemorySmSessionRegistry::restore_from_persistence` which
// is the read-side path the SIGTERM/restart sequence relies on.

// XEP-0160 graceful-shutdown drain is covered by the dedicated
// `sm_promotion` tests in waddle-server (which exercise
// `promote_session_unacked` — the same callable invoked by the
// shutdown-drain task in
// `server/crates/waddle-server/src/server/mod.rs::start_with_config`)
// plus the persist-after-promotion contract on
// `InMemorySmSessionRegistry::drain_all_for_shutdown` /
// `confirm_drained` exercised by `xep0198_session_registry.rs::*`.
// The end-to-end SIGTERM → drain → promote → confirm sequence
// requires axum's runtime + the live websocket router; that
// integration belongs in a separate full-server harness.

// -----------------------------------------------------------------------------
// XEP-0191 blocking interactions (locked final fork #1)
// -----------------------------------------------------------------------------

#[test]
fn xep0160_blocked_sender_does_not_create_pending_delivery_row() {
    let msg = dm(
        "blocked@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        Some("spam"),
    );
    let block = Blocklist::new([bare("blocked@elsewhere")]);
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &block);
    assert_eq!(routing.archive, ArchiveDecision::None);
    assert_eq!(routing.pending, PendingDecision::None);
}

// XEP-0191 §2 step 4 flush-time block re-evaluation (issue #209
// PR #360) is covered by the server-side integration test
// `pending_delivery::tests::flush_drops_pending_row_when_sender_blocked_after_intake`
// in `server/crates/waddle-server/src/pending_delivery.rs`. The
// waddle-xmpp crate cannot exercise the live `flush_for_resource`
// (which lives in waddle-server), so the assertion was promoted to
// the higher layer where the wiring exists.

// -----------------------------------------------------------------------------
// Multi-resource and carbons (locked Q10a) — integration scope
// -----------------------------------------------------------------------------

/// Locked Q10a: offline flush is single-resource. The flush path
/// pushes via the dedicated `send_pending_flush` API (DirectFrame
/// — bypasses the recipient pass + carbon fanout), not via the
/// PeerStanza route that triggers XEP-0280 carbon copying. The
/// recovering session is the sole destination; the recipient's
/// other resources (if any) catch up via MAM.
///
/// This is a structural property of the flush wire path:
/// `send_pending_flush` constructs `OutboundStanza::for_pending_flush`
/// which uses `DeliveryKind::DirectFrame`. Carbons fanout runs only
/// for `DeliveryKind::PeerStanza` (see `RouteToConnection` in
/// `interpret.rs`). There is therefore no code path that could
/// carbon-copy a flush replay.
#[test]
fn xep0160_flush_is_not_copied_via_xep0280_carbons() {
    use waddle_xmpp::pending_delivery::PendingRowId;
    use waddle_xmpp::registry::{DeliveryKind, OutboundStanza};
    use waddle_xmpp::Stanza;
    let msg = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        Some("offline-only"),
    );
    let outbound =
        OutboundStanza::for_pending_flush(Stanza::Message(msg), PendingRowId::fresh(), Utc::now());
    assert_eq!(
        outbound.kind,
        DeliveryKind::DirectFrame,
        "flush replay MUST use DirectFrame so the destination main loop \
         writes it directly to the wire and does NOT feed it through the \
         carbon-fanout (PeerStanza) path (locked Q10a)"
    );
    assert!(
        outbound.pending_row_id.is_some(),
        "flush replay carries the source row id"
    );
}

/// Locked final fork #3: a connection's XEP-0280 carbons opt-in
/// must survive XEP-0198 detach so the resumed session continues
/// receiving carbon-routed stanzas without re-negotiating
/// `<enable xmlns='urn:xmpp:carbons:2'/>`.
///
/// The carbons flag is per-connection and lives on
/// `ConnectionEntry`, NOT on the `StreamManagementState` (which
/// only carries SM-stream counters and the unacked queue).
/// `restore_from_session` therefore correctly does NOT touch the
/// carbons flag — the websocket bind handler reads
/// `detached.carbons_enabled` and writes it onto the NEW
/// `ConnectionEntry` it just created for the resumed transport.
///
/// What this test pins (the contract the resume handshake relies on):
///   1. `to_detached_session` propagates the snapshot's
///      `carbons_enabled` onto the `DetachedSession`.
///   2. The value round-trips through the snapshot — flipping the
///      input flips the output.
///
/// The bind-handler side of the handshake (writing the flag onto
/// the new ConnectionEntry) lives in waddle-server and is exercised
/// by the SM-resume flow there.
#[test]
fn xep0160_sm_resumption_preserves_carbons_enabled_state() {
    use waddle_xmpp::stream_management::{DetachedSessionSnapshot, StreamManagementState};

    // Case 1: carbons enabled at detach → preserved on DetachedSession.
    let mut sm = StreamManagementState::new();
    sm.enable("stream-carbons-on".to_string(), true, Some(300));
    let detached_on = sm
        .to_detached_session(DetachedSessionSnapshot {
            user_id: "alice".to_string(),
            jid: full("alice@example.com/laptop"),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            carbons_enabled: true,
            roster_interested: true,
            blocklist_interested: false,
            presence_available: true,
            presence_show: None,
            presence_status: None,
            presence_priority: 1,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .expect("session resumable");
    assert!(
        detached_on.carbons_enabled,
        "carbons-enabled snapshot propagated onto DetachedSession"
    );

    // Case 2: carbons disabled at detach → preserved as false (the
    // round-trip is faithful, not always-true). Without this, a
    // future regression that always sets carbons_enabled = true on
    // DetachedSession would silently grant carbons to clients that
    // never enabled it.
    let mut sm2 = StreamManagementState::new();
    sm2.enable("stream-carbons-off".to_string(), true, Some(300));
    let detached_off = sm2
        .to_detached_session(DetachedSessionSnapshot {
            user_id: "alice".to_string(),
            jid: full("alice@example.com/laptop"),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            carbons_enabled: false,
            roster_interested: true,
            blocklist_interested: false,
            presence_available: true,
            presence_show: None,
            presence_status: None,
            presence_priority: 1,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .expect("session resumable");
    assert!(
        !detached_off.carbons_enabled,
        "carbons-disabled snapshot also round-trips faithfully \
         (no implicit enable on detach)"
    );
}

// -----------------------------------------------------------------------------
// Dedupe (locked Q10c / Q10d)
// -----------------------------------------------------------------------------

/// Locked Q10c: a stanza archived into MAM and later flushed via
/// pending_delivery's Archived payload variant emits the SAME
/// XEP-0359 stanza-id on both paths — the recipient's archive
/// catch-up sees the same id as the live flush, so client-side
/// dedup works.
///
/// Pinned at the build-replay-stanza wire-shape level: an Archived
/// payload's `<stanza-id>` is preserved verbatim from the MAM row
/// (the classifier stamps the id at intake; flush passes it through).
#[test]
fn xep0160_flush_and_mam_emit_same_stanza_id_for_same_message() {
    let recipient = bare("alice@example.com");
    // Build the archived form of the message — the way MAM would
    // store it after intake stamping.
    let stanza_id_value = "mam-id-stable";
    let mut archived = dm(
        "bob@elsewhere/x",
        "alice@example.com",
        MessageType::Chat,
        Some("from-archive"),
    );
    archived.payloads.push(build_stanza_id_element(
        stanza_id_value,
        &Jid::from(recipient.clone()),
    ));
    // Flush via the Archived payload variant — same wire shape as
    // the recipient's MAM catch-up would produce.
    let payload = MaterializedPayload::Archived(Box::new(archived));
    let replay = build_replay_stanza(
        payload,
        "example.com",
        fixed_receipt(),
        ReplayReason::OfflineStorage,
    );
    // Verify the stanza-id is preserved verbatim — this is the
    // dedup key client implementations key on.
    let stanza_id = replay
        .payloads
        .iter()
        .find(|p| p.name() == "stanza-id" && p.ns() == NS_SID)
        .expect("flush replay carries the stanza-id");
    assert_eq!(stanza_id.attr("id"), Some(stanza_id_value));
    assert_eq!(stanza_id.attr("by"), Some("alice@example.com"));
}

// Locked Q10d (server is best-efforts; client dedupes via XEP-0359
// stanza-id) is a structural property of the codebase: there is no
// `dedup_against_sm_replay` API anywhere, the SM replay path
// (`stanzas_to_resend`) returns the entire unacked queue
// unconditionally, and `flush_for_resource` claims + pushes
// unconditionally once `claim_offline_flush` returns true. The
// invariant is enforced by code review on any future PR that would
// introduce server-side cross-channel dedup; an always-passing
// test here would be a false-green (Codex review on PR #362).
//
// The dedup contract IS exercised at the client side via XEP-0359
// stanza-id matching — see `xep0359_archived_flush_preserves_stanza_id_for_dedupe`
// in `tests/xep0359_stanza_id.rs` which pins the wire-shape
// invariant the client dedups against.
