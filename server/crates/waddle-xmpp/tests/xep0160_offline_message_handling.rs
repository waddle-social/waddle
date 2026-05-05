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
use waddle_xmpp::pending_delivery::flush::{build_replay_stanza, MaterializedPayload};
use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow, PendingRowId};
use waddle_xmpp::protocol::dm_routing::{
    classify_dm_intake, ArchiveDecision, CarbonsDecision, InboxDecision, LiveDecision,
    OnlineResources, PendingDecision,
};
use waddle_xmpp::protocol::event::{StanzaIdRef, StanzaIdValue};
use waddle_xmpp::protocol::session_state::Blocklist;
use waddle_xmpp::xep::xep0334::{add_hint, Hint};
use waddle_xmpp::xep::NS_DELAY;
use waddle_xmpp_core::xep0359::{build_stanza_id_element, NS_SID};
use xmpp_parsers::message::{Body, Message, MessageType};

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
        m.bodies.insert(String::new(), Body(b.to_string()));
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

#[test]
#[ignore = "TODO #209 slice (b) phase 2: integration test requires full intake → quota path"]
fn xep0160_queue_full_returns_service_unavailable_to_sender() {
    todo!("integration: feed N+1 stanzas through OfflineDeliveryHandler with cap=N, assert bounce");
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
    m.id = Some("origin-id-1".to_string());
    PendingRow {
        id: PendingRowId::fresh(),
        recipient: bare(recipient),
        original_receipt_at: fixed_receipt(),
        payload: PendingPayload::Transient(Box::new(m)),
        flushed_in_session: None,
    }
}

#[test]
fn xep0160_flushed_message_carries_delay_with_original_receipt_time() {
    let row = transient_row("alice@example.com", "hi");
    let payload = MaterializedPayload::from_transient(&row).expect("transient");
    let replayed = build_replay_stanza(payload, "example.com", row.original_receipt_at);
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
    let replayed = build_replay_stanza(payload, "example.com", row.original_receipt_at);
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
    let replayed = build_replay_stanza(payload, "example.com", fixed_receipt());
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
    let replayed = build_replay_stanza(payload, "example.com", row.original_receipt_at);
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
    let replayed = build_replay_stanza(payload, "example.com", row.original_receipt_at);
    assert!(replayed
        .payloads
        .iter()
        .any(|p| p.ns() == "urn:test:custom" && p.name() == "custom"));
}

// -----------------------------------------------------------------------------
// §3 Process Flow — flush trigger (slice (d) integration scope)
// -----------------------------------------------------------------------------

#[test]
#[ignore = "TODO #209 slice (d): integration test against ConnectionRegistry+presence handler"]
fn xep0160_flushes_on_first_non_negative_presence_of_fresh_session() {
    todo!("integration: connect Alice, send presence priority=1, assert pending row pushed");
}

#[test]
#[ignore = "TODO #209 slice (d): integration test"]
fn xep0160_negative_to_non_negative_priority_transition_triggers_flush() {
    todo!("integration: connect with priority=-1 then send priority=1; flush must fire on the +1");
}

#[test]
#[ignore = "TODO #209 slice (d) phase 2-3: SM session resumption durability"]
fn xep0160_sm_resumed_session_does_not_reflush_pending_delivery() {
    todo!("integration: detach + resume SM session; pending_delivery must not double-flush");
}

#[test]
#[ignore = "TODO #209 slice (d) phase 2: per-user-bare-JID lock under concurrency"]
fn xep0160_concurrent_resources_first_presence_wins_via_lock() {
    todo!("integration: race two resources' presence; second sees empty claim pool");
}

#[test]
#[ignore = "TODO #209 slice (d) phase 2: SM-ack-keyed deletion (locked Q7b)"]
fn xep0160_pending_row_survives_pre_ack_session_death_for_reflush() {
    todo!("integration: claim → session dies before SM-ack → next resource re-claims");
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

#[test]
#[ignore = "TODO #209 slice (d) phase 4: SM-expiry promotion path"]
fn xep0160_sm_expired_unacked_promoted_to_alt_resource_when_available() {
    todo!("integration: SM session expires; alt resource exists; unacked re-routes there");
}

#[test]
#[ignore = "TODO #209 slice (d) phase 4: SM-expiry promotion path"]
fn xep0160_sm_expired_unacked_promoted_to_pending_delivery_when_no_alt_resource() {
    todo!(
        "integration: SM session expires; user has no other resources; unacked → pending_delivery"
    );
}

#[test]
#[ignore = "TODO #209 slice (d) phase 4: SM-expiry promotion path"]
fn xep0160_sm_expired_unacked_returns_service_unavailable_when_storage_refuses() {
    todo!("integration: quota full + SM expiry → bounce sender");
}

#[test]
#[ignore = "TODO #209 slice (d) phase 4: classifier reuse on promotion"]
fn xep0160_sm_expiry_promotion_reuses_intake_classifier() {
    todo!("verify Q6 promotion path delegates to classify_dm_intake (single source of truth)");
}

#[test]
#[ignore = "TODO #209 slice (d) phase 4: <delay/> stamping"]
fn xep0160_promoted_stanzas_carry_original_receipt_time_in_delay() {
    todo!("integration: promoted stanza's <delay/> stamp = original SM-receipt time, not expiry");
}

// -----------------------------------------------------------------------------
// Server restart durability (locked Q8 = B) — slice (d) follow-up
// -----------------------------------------------------------------------------

#[test]
#[ignore = "TODO #209 slice (d) phase 2: requires DatabaseSmPersistence"]
fn xep0160_pending_delivery_survives_server_restart() {
    todo!("integration: insert + simulate restart + assert rows still present");
}

#[test]
#[ignore = "TODO #209 slice (d) phase 2-3: SM session persistence + restoration"]
fn xep0160_sm_session_resumable_after_server_restart() {
    todo!("integration: detach SM, restart server, resume session, replay unacked");
}

#[test]
#[ignore = "TODO #209 slice (d) phase 4: graceful-shutdown drain"]
fn xep0160_graceful_shutdown_drains_unacked_into_pending_delivery() {
    todo!("integration: SIGTERM → drain → unacked promoted via Q6 path");
}

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

#[test]
#[ignore = "TODO #209 follow-up PR: flush-time block re-evaluation"]
fn xep0160_flush_drops_pending_row_when_sender_blocked_after_intake() {
    todo!("flush-time block check: integration test against pending_delivery + flush_for_resource");
}

// -----------------------------------------------------------------------------
// Multi-resource and carbons (locked Q10a) — integration scope
// -----------------------------------------------------------------------------

#[test]
#[ignore = "TODO #209 slice (d) phase 2: integration with carbons handler"]
fn xep0160_flush_is_not_copied_via_xep0280_carbons() {
    todo!("integration: 3 resources, flush only delivers single-resource");
}

#[test]
#[ignore = "TODO #209 slice (d) phase 3: SM session persistence carries carbons_enabled"]
fn xep0160_sm_resumption_preserves_carbons_enabled_state() {
    todo!("integration: enable carbons → detach → resume → carbons still enabled");
}

// -----------------------------------------------------------------------------
// Dedupe (locked Q10c / Q10d)
// -----------------------------------------------------------------------------

#[test]
#[ignore = "TODO #209 slice (d) phase 2: integration"]
fn xep0160_flush_and_mam_emit_same_stanza_id_for_same_message() {
    todo!("integration: archive a stanza, flush it, MAM-query for it → same stanza-id");
}

#[test]
#[ignore = "TODO #209 slice (d) phase 4: explicit no-server-side-dedupe assertion"]
fn xep0160_server_does_not_filter_duplicates_across_channels() {
    todo!("verify server is best-efforts per XEP-0198 §5 line 367; client dedupes");
}
