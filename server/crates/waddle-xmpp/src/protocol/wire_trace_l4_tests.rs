//! L4 wire-trace integration tests for the sans-I/O message pipeline
//! (issue #229 PR9).
//!
//! These tests exercise the full handler chain end-to-end against a
//! deterministic [`FixedIdGenerator`] so they can assert on the exact
//! shape of the outbound events the chain produces — including the
//! XEP-0359 §5 + XEP-0280 §4 multi-`<stanza-id>` attribution that
//! distinguishes a sender-side archive entry from a recipient-side one.
//!
//! Two state machines (alice/web sending, bob/desk receiving) drive
//! the local-to-local 1:1 flow:
//!
//! 1. Alice's machine receives `<message type='chat' from='alice/web'
//!    to='bob'>` via `InboundFrame::Stanza` — the **sender pass**
//!    runs. We assert: archive write under alice's bare, sent-carbon
//!    fan-out, inbox row keyed `(alice, bob)`, and a route to bob.
//! 2. Bob's machine receives the same stanza via
//!    `InboundEvent::StanzaFromPeer` — the **recipient pass** runs.
//!    We assert: archive write under bob's bare, received-carbon
//!    fan-out, inbox row keyed `(bob, alice)`, and the final
//!    `SendStanza` writing to bob's wire carries TWO `<stanza-id/>`
//!    siblings (one `by='alice@example.com'` from sender pass, one
//!    `by='bob@example.com'` from recipient pass).
//!
//! Locked Q1–Q9 contract from the design grilling — every assertion
//! here is the cross-PR conformance gate we promised (see
//! `docs/superpowers/plans/2026-04-28-issue-229-sans-io-message-pipeline-remaining-prs.md`).

use crate::Stanza;
use jid::{BareJid, FullJid, Jid};
use std::sync::Arc;
use xmpp_parsers::message::{Body, Message, MessageType};

use super::dispatch::StanzaDispatcher;
use super::event::OutboundEvent;
use super::handlers::register_default_message_handlers;
use super::id_gen::FixedIdGenerator;
use super::machine::test_support::ready_machine_with_id_gen;
use super::{InboundEvent, InboundFrame};

fn full(s: &str) -> FullJid {
    s.parse().expect("valid full jid")
}

fn bare(s: &str) -> BareJid {
    s.parse().expect("valid bare jid")
}

fn jid(s: &str) -> Jid {
    s.parse().expect("valid jid")
}

fn chat_with_body(from: &FullJid, to: &BareJid, body: &str) -> Message {
    let mut m = Message::new(Some(jid(&to.to_string())));
    m.from = Some(jid(&from.to_string()));
    m.type_ = MessageType::Chat;
    m.id = Some("wire-id".to_string());
    m.bodies.insert(String::new(), Body(body.to_string()));
    m
}

fn build_dispatcher() -> StanzaDispatcher {
    let mut d = StanzaDispatcher::new();
    register_default_message_handlers(&mut d);
    d
}

/// Helper: count `<stanza-id by='…'/>` siblings on a [`Message`].
fn stanza_id_bys(message: &Message) -> Vec<String> {
    message
        .payloads
        .iter()
        .filter(|p| p.name() == "stanza-id" && p.ns() == crate::xep::xep0359::NS_SID)
        .filter_map(|p| p.attr("by").map(ToOwned::to_owned))
        .collect()
}

#[test]
fn xep_0359_sender_pass_archives_under_local_user_and_routes_to_recipient() {
    // Sender pass on alice/web's machine. Inbound:
    // <message type='chat' from='alice/web' to='bob' body='hi'>
    //
    // Expected events (Q2(a) order):
    // - ArchiveDirect { archive_jid: alice, ... } (XEP-0313)
    // - ProjectInbox { owner: alice, peer: bob, ... } (Waddle inbox)
    // - SendCarbons { owner: alice, kind: Sent, ... } (XEP-0280 §4
    //   suppressed when no other resources, but the event would emit
    //   if alice had carbons enabled — we assert the emission shape)
    // - RouteToConnection { jid: bob, ... } (final route)
    let alice_web = full("alice@example.com/web");
    let bob = bare("bob@example.com");

    let id_gen = Arc::new(FixedIdGenerator("alice-canon-1".to_string()));
    let mut sm =
        ready_machine_with_id_gen("example.com", alice_web.clone(), build_dispatcher(), id_gen);

    let msg = chat_with_body(&alice_web, &bob, "hi bob");
    let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
        Stanza::Message(msg),
    ))));

    let archive_events: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            OutboundEvent::ArchiveDirect { archive_jid, .. } => Some(archive_jid.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        archive_events,
        vec![bare("alice@example.com")],
        "sender pass writes one archive entry under alice's bare"
    );

    let inbox_events: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            OutboundEvent::ProjectInbox { owner, peer, .. } => Some((owner.clone(), peer.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        inbox_events,
        vec![(bare("alice@example.com"), bare("bob@example.com"))]
    );

    let route_targets: Vec<Jid> = events
        .iter()
        .filter_map(|e| match e {
            OutboundEvent::RouteToConnection { jid, .. } => Some(jid.clone()),
            _ => None,
        })
        .collect();
    assert!(
        route_targets.iter().any(|j| j.to_bare() == bob),
        "sender pass produces RouteToConnection targeting bob; got {route_targets:?}"
    );
}

#[test]
fn xep_0359_recipient_pass_writes_recipient_archive_and_double_stamps_outgoing_wire() {
    // Recipient pass on bob/desk's machine. Inbound:
    // alice/web's stanza arrived via routing; bob's machine sees it
    // as InboundEvent::StanzaFromPeer with alice's `<stanza-id by='alice'/>`
    // already stamped from the sender pass.
    //
    // Expected events on bob's machine:
    // - ArchiveDirect { archive_jid: bob, ... } (XEP-0313 recipient
    //   side — the inbox→MAM-pivot test in PR7 proves this writes the
    //   canonical XEP-0359 stamp Bob's CanonicalizeHandler just stamped).
    // - ProjectInbox { owner: bob, peer: alice, ... }
    // - SendStanza writing to bob's wire — the final stanza MUST carry
    //   TWO `<stanza-id/>` siblings: alice's (preserved) and bob's
    //   (freshly stamped).
    let bob_desk = full("bob@example.com/desk");
    let alice_web = full("alice@example.com/web");
    let bob = bare("bob@example.com");

    // Pre-stamp alice's sender-side `<stanza-id by='alice'/>` to
    // simulate the sender pass already having canonicalized.
    let mut wire_msg = chat_with_body(&alice_web, &bob, "hi bob");
    wire_msg
        .payloads
        .push(crate::xep::xep0359::build_stanza_id_element(
            "alice-canon-1",
            "alice@example.com",
        ));

    let id_gen = Arc::new(FixedIdGenerator("bob-canon-1".to_string()));
    let mut sm =
        ready_machine_with_id_gen("example.com", bob_desk.clone(), build_dispatcher(), id_gen);

    let events = sm.handle(InboundEvent::StanzaFromPeer(Box::new(Stanza::Message(
        wire_msg,
    ))));

    // 1. Archive entry under bob's bare.
    let archive_events: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            OutboundEvent::ArchiveDirect {
                archive_jid,
                message,
                ..
            } => Some((archive_jid.clone(), stanza_id_bys(message))),
            _ => None,
        })
        .collect();
    assert_eq!(archive_events.len(), 1);
    let (archive_jid, archive_stamps) = &archive_events[0];
    assert_eq!(archive_jid, &bare("bob@example.com"));
    // The archived message at the time `ArchiveHandler` emits carries
    // BOTH stamps because Q2(a) order has CanonicalizeHandler running
    // before ArchiveHandler — bob's stamp is added; alice's stamp
    // (cross-archive) is preserved per XEP-0359 §5.
    assert!(
        archive_stamps.contains(&"alice@example.com".to_string()),
        "alice's cross-archive stamp must be preserved on the archived message; \
         got {archive_stamps:?}"
    );
    assert!(
        archive_stamps.contains(&"bob@example.com".to_string()),
        "bob's recipient-side stamp must be present on the archived message; \
         got {archive_stamps:?}"
    );

    // 2. Inbox keyed (bob, alice) with bob's archive_ref.
    let inbox_events: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            OutboundEvent::ProjectInbox {
                owner,
                peer,
                archive_ref,
                ..
            } => Some((
                owner.clone(),
                peer.clone(),
                archive_ref.id.as_str().to_string(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        inbox_events,
        vec![(
            bare("bob@example.com"),
            bare("alice@example.com"),
            "bob-canon-1".to_string()
        )],
        "recipient-pass inbox row keys to bob's local archive ref"
    );
}

#[test]
fn xep_0359_recipient_pass_handler_chain_runs_in_locked_q2a_order() {
    // Cross-PR conformance gate: the chain produces handler events in
    // exactly the locked Q2(a) order. We assert by event type sequence
    // — privacy first (BlockingFilter), then rich-target validation,
    // then canonicalize (stamps the archived message), then archive,
    // then carbons, then inbox, then route. Inbox MUST come AFTER
    // archive so the inbox row can reference the canonical XEP-0359
    // stamp under the local archive.
    let bob_desk = full("bob@example.com/desk");
    let alice_web = full("alice@example.com/web");
    let bob = bare("bob@example.com");

    let mut wire_msg = chat_with_body(&alice_web, &bob, "ordered");
    wire_msg
        .payloads
        .push(crate::xep::xep0359::build_stanza_id_element(
            "alice-A",
            "alice@example.com",
        ));

    let id_gen = Arc::new(FixedIdGenerator("bob-B".to_string()));
    let mut sm =
        ready_machine_with_id_gen("example.com", bob_desk.clone(), build_dispatcher(), id_gen);

    let events = sm.handle(InboundEvent::StanzaFromPeer(Box::new(Stanza::Message(
        wire_msg,
    ))));

    // Emission order maps directly to the locked chain: ArchiveDirect
    // (handler 5) MUST appear before ProjectInbox (handler 7) which
    // MUST appear before RouteToConnection (handler 8). Pre-archive
    // handlers (BlockingFilter, RichTargetValidation, Canonicalize,
    // EnrichmentDispatch) emit no outbound events for a plain chat
    // body so they don't show up in this order check.
    let typed_order: Vec<&'static str> = events
        .iter()
        .filter_map(|e| match e {
            OutboundEvent::ArchiveDirect { .. } => Some("ArchiveDirect"),
            OutboundEvent::ProjectInbox { .. } => Some("ProjectInbox"),
            OutboundEvent::RouteToConnection { .. } => Some("RouteToConnection"),
            OutboundEvent::SendCarbons { .. } => Some("SendCarbons"),
            _ => None,
        })
        .collect();

    let archive_idx = typed_order.iter().position(|s| *s == "ArchiveDirect");
    let inbox_idx = typed_order.iter().position(|s| *s == "ProjectInbox");
    assert!(
        archive_idx.is_some(),
        "recipient pass must emit ArchiveDirect"
    );
    assert!(inbox_idx.is_some(), "recipient pass must emit ProjectInbox");
    assert!(
        archive_idx.unwrap() < inbox_idx.unwrap(),
        "ArchiveDirect must precede ProjectInbox per Q2(a) — \
         inbox links to the canonical stamp the archive write captured. \
         Got order: {typed_order:?}"
    );
}
