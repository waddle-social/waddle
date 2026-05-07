//! XEP-0191: Blocking Command — offline-DM interactions.
//!
//! Locked design from issue #209 grilling (final fork #1):
//!
//!   *Blocklist evaluated at intake AND at flush*.
//!
//! - **Intake** (`classify_dm_intake`): if the recipient's blocklist
//!   contains the sender, the classifier emits `pending = None` and
//!   `archive = None` so no MAM row and no `pending_delivery` row
//!   are created (XEP-0191 §2 "blocked entities are silently
//!   discarded").
//!
//! - **Flush re-evaluation** (PR #360, XEP-0191 §2 step 4): if the
//!   recipient blocked the sender AFTER intake but BEFORE flush, the
//!   row is dropped (deleted) instead of replayed. Block is final
//!   until lifted, so `delete_row` not `release_row`. Server-side
//!   integration test for this lives in
//!   `server/crates/waddle-server/src/pending_delivery.rs::tests::flush_drops_pending_row_when_sender_blocked_after_intake`.
//!
//! Citations refer to `xeps/xep-0191.xml` unless otherwise noted.

use jid::{BareJid, Jid};
use waddle_xmpp::protocol::dm_routing::{
    classify_dm_intake, ArchiveDecision, CarbonsDecision, InboxDecision, LiveDecision,
    OnlineResources, PendingDecision,
};
use waddle_xmpp::protocol::session_state::Blocklist;
use waddle_xmpp::xep::xep0191::{BlockingStorage, InMemoryBlockingStorage};
use xmpp_parsers::message::{Body, Message, MessageType};

fn bare(s: &str) -> BareJid {
    s.parse().expect("bare jid")
}

fn dm(from: &str, to: &str, body: &str) -> Message {
    let mut m = Message::new(Some(to.parse::<Jid>().expect("jid")));
    m.from = Some(from.parse::<Jid>().expect("jid"));
    m.type_ = MessageType::Chat;
    m.bodies.insert(String::new(), Body(body.to_string()));
    m
}

/// XEP-0191 §2 + locked Q9-related: a blocked sender's stanza is
/// silently discarded at intake — no MAM row, no `pending_delivery`
/// row, no inbox bump, no carbons fanout.
#[test]
fn xep0191_intake_drops_blocked_sender_completely() {
    let msg = dm("blocked@elsewhere/x", "alice@example.com", "spam");
    let block = Blocklist::new([bare("blocked@elsewhere")]);
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &block);
    assert_eq!(routing.archive, ArchiveDecision::None);
    assert_eq!(routing.pending, PendingDecision::None);
    assert_eq!(routing.live, LiveDecision::None);
    assert_eq!(routing.carbons, CarbonsDecision::Suppressed);
    assert_eq!(routing.inbox, InboxDecision::None);
}

/// Block matches on the sender's BARE JID (XEP-0191 §2): a
/// per-resource block is implicit when the bare JID matches.
#[test]
fn xep0191_block_matches_on_bare_jid_regardless_of_sender_resource() {
    let msg_laptop = dm("blocked@elsewhere/laptop", "alice@example.com", "spam");
    let msg_phone = dm("blocked@elsewhere/phone", "alice@example.com", "spam");
    let block = Blocklist::new([bare("blocked@elsewhere")]);
    for msg in [msg_laptop, msg_phone] {
        let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &block);
        assert_eq!(routing.pending, PendingDecision::None);
        assert_eq!(routing.archive, ArchiveDecision::None);
    }
}

/// Block does NOT affect a different sender (sanity: the blocklist
/// is per-recipient and matches by bare JID identity).
#[test]
fn xep0191_unblocked_sender_unaffected_by_other_block_entry() {
    let msg = dm("alice-friend@elsewhere/x", "alice@example.com", "hello");
    let block = Blocklist::new([bare("blocked@elsewhere")]);
    let routing = classify_dm_intake(&msg, &OnlineResources::empty(), &block);
    // Alice has no available resources → recipient-fully-offline.
    // Without a block entry against this sender, the message MUST
    // route to MAM + pending_delivery normally.
    assert_eq!(routing.pending, PendingDecision::Archived);
    assert_eq!(routing.archive, ArchiveDecision::Mam);
}

/// XEP-0191 §2 step 4 (PR #360 storage-trait contract): the
/// `BlockingStorage` trait `list_blocked_jids` is the read used at
/// flush time. Verify the in-memory implementation behaves the way
/// the flush re-eval depends on (set/clear semantics).
#[tokio::test]
async fn xep0191_blocking_storage_round_trips_per_user_lists() {
    let storage = InMemoryBlockingStorage::new();
    let alice = bare("alice@example.com");

    // Initially no blocks.
    let initial = storage.list_blocked_jids(&alice).await.expect("read empty");
    assert!(initial.is_empty());

    // Add two entries.
    storage.set_blocklist(
        alice.clone(),
        vec![bare("spam@elsewhere"), bare("phisher@elsewhere")],
    );
    let listed = storage
        .list_blocked_jids(&alice)
        .await
        .expect("read after set");
    assert_eq!(listed.len(), 2);
    assert!(listed.contains(&bare("spam@elsewhere")));
    assert!(listed.contains(&bare("phisher@elsewhere")));

    // Replace with empty list — XEP-0191 §3.2 "unblock all".
    storage.set_blocklist(alice.clone(), vec![]);
    let after_clear = storage
        .list_blocked_jids(&alice)
        .await
        .expect("read after clear");
    assert!(after_clear.is_empty());
}

// XEP-0191 §2 step 4 flush-time block re-evaluation (PR #360) —
// the live `flush_for_resource` path that reads
// `BlockingStorage::list_blocked_jids` per batch and drops rows
// whose sender is now blocked — is exercised by the dedicated
// server-side integration test in
// `server/crates/waddle-server/src/pending_delivery.rs::tests`:
//
//   - `flush_drops_pending_row_when_sender_blocked_after_intake`
//     — the happy-path drop semantics
//   - `flush_aborts_on_blocking_storage_failure_fail_closed` —
//     fail-closed policy on storage error (matches the intake
//     pass policy in
//     `interpret.rs::offline_recipient_pass_blocklist_storage_error_skips_recipient_persistence`)
//   - `flush_blocked_row_releases_claim_when_delete_fails` — the
//     wedge-recovery path that prevents a quota leak when
//     `delete_row` fails
//
// The flush function lives in waddle-server (it depends on
// ConnectionRegistry + PendingDeliveryStorage), so the integration
// test belongs there. This dedicated XEP-0191 suite covers the
// trait + classifier surface; the flush wiring sits one layer up.
