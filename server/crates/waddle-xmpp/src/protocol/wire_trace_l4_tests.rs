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
use xmpp_parsers::message::{Message, MessageType};

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

fn chat_with_body(from: &FullJid, to: &BareJid, body: &str) -> Message {
    let mut m = Message::new(Some(Jid::from(to.clone())));
    m.from = Some(Jid::from(from.clone()));
    m.type_ = MessageType::Chat;
    m.id = Some(xmpp_parsers::message::Id("wire-id".to_string()));
    m.bodies
        .insert(xmpp_parsers::message::Lang::new(), body.to_string());
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
        .filter(|p| p.name() == "stanza-id" && p.ns() == waddle_xmpp_core::xep0359::NS_SID)
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
    // - RouteToConnection { jid: bob, ... } (final route)
    //
    // SendCarbons is NOT emitted in this fixture because the default
    // session state has carbons disabled — the
    // [`super::handlers::carbons_message::CarbonsMessageHandler`]
    // gate suppresses fan-out per XEP-0280 §4. The L1
    // CarbonsMessageHandler suite asserts the emission shape when
    // carbons are enabled; this L4 test focuses on archive/inbox/route.
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
        .push(waddle_xmpp_core::xep0359::build_stanza_id_element(
            "alice-canon-1",
            &"alice@example.com".parse::<Jid>().expect("jid"),
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

    // 3. Final wire write — XEP-0359 §5 + XEP-0280 §4 require the
    //    bytes Bob sees to carry BOTH stanza-ids: alice's (sender
    //    pass, preserved as cross-archive) and bob's (recipient pass,
    //    freshly stamped under bob's bare).
    let send_stanza_msgs: Vec<&Message> = events
        .iter()
        .filter_map(|e| match e {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Message(m) => Some(m),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        send_stanza_msgs.len(),
        1,
        "recipient pass terminates with exactly one SendStanza to bob's wire"
    );
    let wire_stamps = stanza_id_bys(send_stanza_msgs[0]);
    assert!(
        wire_stamps.contains(&"alice@example.com".to_string()),
        "wire bytes preserve alice's cross-archive stamp; got {wire_stamps:?}"
    );
    assert!(
        wire_stamps.contains(&"bob@example.com".to_string()),
        "wire bytes carry bob's recipient-side stamp; got {wire_stamps:?}"
    );
    assert_eq!(
        wire_stamps.len(),
        2,
        "wire bytes carry exactly two <stanza-id/> siblings; got {wire_stamps:?}"
    );
}

#[test]
fn xep_0359_self_loop_peer_stanza_terminates_at_wire_not_re_routes() {
    // Regression for the routing-loop bug Codex flagged: a peer
    // stanza arriving from `from=alice/web, to=alice/web` on
    // alice/web's connection is `Locality::Both` per
    // `MessageContext::derive`. RouteHandler treats `Both` chat as
    // needing routing — without the peer-pass locality override in
    // `on_peer_stanza`, this would re-emit `RouteToConnection` and
    // loop. The override forces `Locality::Recipient` so the pass
    // terminates with `SendStanza`.
    let alice_web = full("alice@example.com/web");
    let alice = bare("alice@example.com");

    let mut wire_msg = chat_with_body(&alice_web, &alice, "self");
    // Force `to=alice/web` (full self-loop) so locality::derive
    // returns Both rather than Recipient. The override under test
    // is what disambiguates.
    wire_msg.to = Some(Jid::from(alice_web.clone()));
    wire_msg
        .payloads
        .push(waddle_xmpp_core::xep0359::build_stanza_id_element(
            "alice-loop-1",
            &"alice@example.com".parse::<Jid>().expect("jid"),
        ));

    let id_gen = Arc::new(FixedIdGenerator("alice-loop-2".to_string()));
    let mut sm =
        ready_machine_with_id_gen("example.com", alice_web.clone(), build_dispatcher(), id_gen);

    let events = sm.handle(InboundEvent::StanzaFromPeer(Box::new(Stanza::Message(
        wire_msg,
    ))));

    let route_count = events
        .iter()
        .filter(|e| matches!(e, OutboundEvent::RouteToConnection { .. }))
        .count();
    let send_count = events
        .iter()
        .filter(|e| matches!(e, OutboundEvent::SendStanza(_)))
        .count();
    // Diagnostic: variant-name list only — never `{events:#?}`.
    // `OutboundEvent` carries variants whose Debug impl would serialize
    // user-content fields (`Stanza::Message`, OAuth tokens, SCRAM
    // credentials), and the static analyzer flags blanket Debug
    // formatting as a cleartext-logging concern even on a path that
    // can't produce those variants.
    let variant_summary: Vec<&'static str> = events.iter().map(outbound_variant_name).collect();
    assert_eq!(
        route_count, 0,
        "peer-pass self-loop must NOT re-emit RouteToConnection \
         (would create routing loop); got variants: {variant_summary:?}"
    );
    assert_eq!(
        send_count, 1,
        "peer-pass self-loop terminates with exactly one SendStanza; \
         got variants: {variant_summary:?}"
    );
}

/// Map an [`OutboundEvent`] variant to its name without serializing
/// the typed payload. Used in panic messages so test diagnostics never
/// blanket-Debug-format events whose variants may carry user content
/// or credentials (per the cleartext-logging static check).
fn outbound_variant_name(event: &OutboundEvent) -> &'static str {
    match event {
        OutboundEvent::SendStanza(_) => "SendStanza",
        OutboundEvent::CloseTransport => "CloseTransport",
        OutboundEvent::Log { .. } => "Log",
        OutboundEvent::RouteToConnection { .. } => "RouteToConnection",
        OutboundEvent::DispatchToRoom { .. } => "DispatchToRoom",
        OutboundEvent::RegisterConnection(_) => "RegisterConnection",
        OutboundEvent::UnregisterConnection(_) => "UnregisterConnection",
        OutboundEvent::ArchiveGroupchat { .. } => "ArchiveGroupchat",
        OutboundEvent::ArchiveDirect { .. } => "ArchiveDirect",
        OutboundEvent::ApplyGroupchatRetractionTombstone { .. } => {
            "ApplyGroupchatRetractionTombstone"
        }
        OutboundEvent::ApplyPinChange { .. } => "ApplyPinChange",
        OutboundEvent::PersistRoomSubject { .. } => "PersistRoomSubject",
        OutboundEvent::ProjectInbox { .. } => "ProjectInbox",
        OutboundEvent::ProjectGroupchatInbox { .. } => "ProjectGroupchatInbox",
        OutboundEvent::SendCarbons { .. } => "SendCarbons",
        OutboundEvent::RequestEnrichment { .. } => "RequestEnrichment",
        OutboundEvent::AskSfu { .. } => "AskSfu",
        OutboundEvent::QueryMam { .. } => "QueryMam",
        OutboundEvent::LoadScramCredentials { .. } => "LoadScramCredentials",
        OutboundEvent::ValidateOAuthBearer { .. } => "ValidateOAuthBearer",
        OutboundEvent::LookupArchivedMessage { .. } => "LookupArchivedMessage",
        OutboundEvent::SendKeepaliveProbe => "SendKeepaliveProbe",
        OutboundEvent::SetTimer { .. } => "SetTimer",
        OutboundEvent::CancelTimer(_) => "CancelTimer",
        OutboundEvent::QueueOfflineDelivery { .. } => "QueueOfflineDelivery",
        OutboundEvent::MarkInboxReadFromDisplayed { .. } => "MarkInboxReadFromDisplayed",
    }
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
        .push(waddle_xmpp_core::xep0359::build_stanza_id_element(
            "alice-A",
            &"alice@example.com".parse::<Jid>().expect("jid"),
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
