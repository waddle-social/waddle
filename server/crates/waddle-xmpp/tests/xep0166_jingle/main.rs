//! XEP-0166: Jingle (core session signaling) — dedicated test suite.
//!
//! Spec: <https://xmpp.org/extensions/xep-0166.html> (v1.1.2, Final).
//!
//! Pins the wire shapes Waddle accepts and emits for the four
//! Jingle actions that drive a LiveKit-backed call:
//!
//! * `session-initiate` — XEP-0166 §6.2
//! * `session-accept`   — XEP-0166 §6.4
//! * `session-terminate` — XEP-0166 §6.7
//! * `transport-info`   — XEP-0166 §6.8 (used by mid-session
//!   negotiation; routed unchanged with a sanitised `from` by the
//!   server)
//!
//! Construction goes through typed Rust values
//! ([`xmpp_parsers::jingle::Jingle`], [`Action`], [`Content`],
//! [`SessionId`]) and [`minidom::Element::builder`] — no `format!`,
//! no string concatenation (CLAUDE.md XML hard rule).

use std::{
    sync::{Arc, Barrier},
    thread,
};

use chrono::Duration;
use jid::FullJid;
use minidom::Element;
use waddle_sfu::{
    ApiKey, ApiSecret, Identity, LiveKitSfu, SfuConfig, SfuService, TurnHost, TurnSharedSecret,
    WebsocketUrl,
};
use waddle_xmpp::protocol::{
    event::OutboundEvent, handlers::jingle::JingleHandler, traits::IqHandler, StanzaContext,
};
use waddle_xmpp::xep::xep0166::{
    reason_element, session_terminate, Action, Content, ContentId, Creator, Jingle, Reason,
    SessionId, Transport, NS_JINGLE,
};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::DefinedCondition;

use waddle_xmpp::xep::xep_waddle_livekit_transport::NS_WADDLE_LIVEKIT_TRANSPORT;

const NS_JINGLE_RTP: &str = "urn:xmpp:jingle:apps:rtp:1";

fn audio_content_with_waddle_transport() -> Content {
    let mut content = Content::new(Creator::Initiator, ContentId("audio".into()));
    // Empty description in the RTP namespace — codec list rides
    // session-initiate proper; this is the per-content media tag.
    let description = Element::builder("description", NS_JINGLE_RTP)
        .attr(minidom::rxml::xml_ncname!("media").to_owned(), "audio")
        .build();
    content.description = Some(xmpp_parsers::jingle::Description::Rtp(
        xmpp_parsers::jingle_rtp::Description::try_from(description).expect("rtp desc"),
    ));
    let transport = Element::builder("transport", NS_WADDLE_LIVEKIT_TRANSPORT).build();
    content.transport = Some(Transport::Unknown(transport));
    content
}

fn audio_content_with_issued_waddle_transport() -> Content {
    let mut content = audio_content_with_waddle_transport();
    let transport = Element::builder("transport", NS_WADDLE_LIVEKIT_TRANSPORT)
        .attr(
            minidom::rxml::xml_ncname!("url").to_owned(),
            "wss://evil.test",
        )
        .attr(minidom::rxml::xml_ncname!("room").to_owned(), "evil-room")
        .attr(
            minidom::rxml::xml_ncname!("identity").to_owned(),
            "mallory@waddle.test/laptop",
        )
        .append(
            Element::builder("token", NS_WADDLE_LIVEKIT_TRANSPORT)
                .append("attacker-token")
                .build(),
        )
        .build();
    content.transport = Some(Transport::Unknown(transport));
    content
}

fn fixture_livekit_sfu() -> Arc<LiveKitSfu> {
    let cfg = SfuConfig {
        api_key: ApiKey::new("APIxxxxxxxx"),
        api_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test secret meets min length"),
        webhook_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min")
            .expect("test secret meets min length"),
        ws_url: WebsocketUrl::new("wss://livekit.test/".parse().unwrap()).unwrap(),
        turn_host: TurnHost::new("turn.test"),
        turn_tls_port: 443,
        turn_udp_port: 3478,
        turn_shared_secret: TurnSharedSecret::from_text("turn-secret"),
        token_ttl: Duration::seconds(3600),
        turn_ttl: Duration::seconds(3600),
    };
    Arc::new(LiveKitSfu::new(cfg).expect("LiveKitSfu init in test"))
}

fn ctx<'a>(jid: &'a FullJid) -> StanzaContext<'a> {
    StanzaContext {
        domain: "waddle.test",
        full_jid: jid,
        media_capabilities: None,
    }
}

fn dm_jingle_iq(action: Action, from: &str, to: &str, sid: &str) -> Iq {
    let mut jingle = Jingle::new(action.clone(), SessionId(sid.into()));
    match action {
        Action::SessionInitiate => {
            jingle.initiator = Some(from.parse().expect("valid initiator JID"));
        }
        Action::SessionAccept => {
            jingle.responder = Some(from.parse().expect("valid responder JID"));
        }
        _ => {}
    }
    jingle.contents.push(audio_content_with_waddle_transport());

    Iq::Set {
        from: Some(from.parse().expect("valid from JID")),
        to: Some(to.parse().expect("valid to JID")),
        id: format!("jingle-{sid}"),
        payload: jingle.into(),
    }
}

fn dm_jingle_iq_with_content(
    action: Action,
    from: &str,
    to: &str,
    sid: &str,
    content: Content,
) -> Iq {
    let mut jingle = Jingle::new(action.clone(), SessionId(sid.into()));
    match action {
        Action::SessionInitiate => {
            jingle.initiator = Some(from.parse().expect("valid initiator JID"));
        }
        Action::SessionAccept => {
            jingle.responder = Some(from.parse().expect("valid responder JID"));
        }
        _ => {}
    }
    jingle.contents.push(content);

    Iq::Set {
        from: Some(from.parse().expect("valid from JID")),
        to: Some(to.parse().expect("valid to JID")),
        id: format!("jingle-{sid}"),
        payload: jingle.into(),
    }
}

fn first_error_condition(events: &[OutboundEvent]) -> Option<DefinedCondition> {
    events.iter().find_map(|event| {
        let OutboundEvent::SendStanza(stanza) = event else {
            return None;
        };
        let Stanza::Iq(reply) = stanza.as_ref() else {
            return None;
        };
        let Iq::Error { error, .. } = reply.as_ref() else {
            return None;
        };
        Some(error.defined_condition.clone())
    })
}

fn has_route_to(events: &[OutboundEvent], expected_jid: &str) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            OutboundEvent::RouteToConnection { jid, .. } if jid.to_string() == expected_jid
        )
    })
}

fn parse_jingle(xml: &str) -> Jingle {
    let elem: Element = xml.parse().expect("element parses");
    Jingle::try_from(elem).expect("Jingle parses")
}

// ── §3 namespace ──────────────────────────────────────────────────────

#[test]
fn xep_0166_namespace_matches_spec() {
    // XEP-0166 §3: Jingle stanzas are qualified by `urn:xmpp:jingle:1`.
    assert_eq!(NS_JINGLE, "urn:xmpp:jingle:1");
    assert_eq!(NS_JINGLE, xmpp_parsers::ns::JINGLE);
}

// ── §6.2 session-initiate ─────────────────────────────────────────────

#[test]
fn xep_0166_session_initiate_round_trips_through_typed_surface() {
    // §6.2: session-initiate carries `action='session-initiate'`,
    // `initiator='<full-jid>'`, `sid='<opaque>'` and at least one
    // `<content/>` child. Parses through xmpp-parsers' typed
    // `Jingle::try_from` and the action lands as the typed
    // `Action::SessionInitiate` variant.
    let xml = r#"<jingle xmlns='urn:xmpp:jingle:1'
                         action='session-initiate'
                         initiator='alice@waddle.test/desktop'
                         sid='c1'>
                   <content creator='initiator' name='audio'>
                     <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>
                     <transport xmlns='urn:waddle:transports:livekit:0'/>
                   </content>
                 </jingle>"#;
    let jingle = parse_jingle(xml);
    assert_eq!(jingle.action, Action::SessionInitiate);
    assert_eq!(jingle.sid.0, "c1");
    assert_eq!(
        jingle.initiator.as_ref().map(|j| j.to_string()),
        Some("alice@waddle.test/desktop".to_string())
    );
    assert_eq!(jingle.contents.len(), 1);
    let content = &jingle.contents[0];
    assert_eq!(content.creator, Creator::Initiator);
    assert_eq!(content.name.0, "audio");
}

#[test]
fn xep_0166_session_initiate_builder_emits_initiator_attr_via_minidom() {
    // Builders MUST use typed `minidom::Element::builder` per the
    // CLAUDE.md XML hard rule. Round-trip a hand-built session-initiate
    // through the typed `Jingle::try_from` pass to prove that the
    // builder-emitted shape parses back into the same typed value.
    let mut jingle = Jingle::new(Action::SessionInitiate, SessionId("c2".into()));
    jingle.initiator = Some("alice@waddle.test/desktop".parse().expect("valid jid"));
    jingle.contents.push(audio_content_with_waddle_transport());

    let elem: Element = jingle.into();
    assert_eq!(elem.name(), "jingle");
    assert_eq!(elem.ns(), NS_JINGLE);
    assert_eq!(elem.attr("action"), Some("session-initiate"));
    assert_eq!(elem.attr("sid"), Some("c2"));
    assert_eq!(elem.attr("initiator"), Some("alice@waddle.test/desktop"));

    // Round-trip the serialised form back through the parser.
    let reparsed = Jingle::try_from(elem).expect("reparses");
    assert_eq!(reparsed.action, Action::SessionInitiate);
    assert_eq!(reparsed.sid.0, "c2");
    assert_eq!(reparsed.contents.len(), 1);
}

// ── §6.4 session-accept ───────────────────────────────────────────────

#[test]
fn xep_0166_session_accept_carries_responder_attribute() {
    // §6.4: session-accept identifies the accepting party via
    // `responder='<full-jid>'`. The initiator attribute mirrors the
    // original session-initiate so the server can re-derive the call
    // scope.
    let xml = r#"<jingle xmlns='urn:xmpp:jingle:1'
                         action='session-accept'
                         initiator='alice@waddle.test/desktop'
                         responder='bob@waddle.test/desktop'
                         sid='c1'>
                   <content creator='initiator' name='audio'>
                     <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>
                     <transport xmlns='urn:waddle:transports:livekit:0'/>
                   </content>
                 </jingle>"#;
    let jingle = parse_jingle(xml);
    assert_eq!(jingle.action, Action::SessionAccept);
    assert_eq!(
        jingle.responder.as_ref().map(|j| j.to_string()),
        Some("bob@waddle.test/desktop".to_string())
    );
    assert_eq!(
        jingle.initiator.as_ref().map(|j| j.to_string()),
        Some("alice@waddle.test/desktop".to_string())
    );
}

#[test]
fn xep_0166_dm_session_accept_requires_prior_invite_for_same_full_jid() {
    // Waddle's XEP-0166 LiveKit transport rewrite is server-mediated:
    // the initial session-initiate mints the responder's token and
    // records the invited full JID. A later session-accept may mint
    // back into the initiator-scoped room only for that same invited
    // responder, not an arbitrary authenticated local user.
    let sfu = fixture_livekit_sfu();
    let handler = JingleHandler::new(sfu);
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("valid JID");
    let bob: FullJid = "bob@waddle.test/phone".parse().expect("valid JID");

    let initiate = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-prior-invite",
    );
    let initiate_events = handler.handle(&initiate, &ctx(&alice));
    assert!(
        has_route_to(&initiate_events, "bob@waddle.test/phone"),
        "session-initiate should route the token-bearing invite to Bob: {initiate_events:?}",
    );

    let accept = dm_jingle_iq(
        Action::SessionAccept,
        "bob@waddle.test/phone",
        "alice@waddle.test/desktop",
        "dm-prior-invite",
    );
    let accept_events = handler.handle(&accept, &ctx(&bob));

    assert_eq!(first_error_condition(&accept_events), None);
    assert!(
        has_route_to(&accept_events, "alice@waddle.test/desktop"),
        "invited responder should be allowed to accept and route back to Alice: {accept_events:?}",
    );
}

#[test]
fn xep_0166_dm_session_accept_rejects_fresh_uninvited_third_party() {
    // Alice invited Bob for this sid. A fresh unrelated authenticated
    // third party must not be able to mint back into Alice's scoped
    // call id by sending a session-accept with the same sid.
    let sfu = fixture_livekit_sfu();
    let handler = JingleHandler::new(sfu);
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("valid JID");
    let eve: FullJid = "eve@waddle.test/laptop".parse().expect("valid JID");

    let initiate = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-fresh-third-party",
    );
    let initiate_events = handler.handle(&initiate, &ctx(&alice));
    assert!(
        has_route_to(&initiate_events, "bob@waddle.test/phone"),
        "session-initiate should route the token-bearing invite to Bob: {initiate_events:?}",
    );

    let eve_accept = dm_jingle_iq(
        Action::SessionAccept,
        "eve@waddle.test/laptop",
        "alice@waddle.test/desktop",
        "dm-fresh-third-party",
    );
    let accept_events = handler.handle(&eve_accept, &ctx(&eve));

    assert_eq!(
        first_error_condition(&accept_events),
        Some(DefinedCondition::Forbidden),
        "fresh third-party accept must not satisfy Bob's invite: {accept_events:?}",
    );
    assert!(
        !has_route_to(&accept_events, "alice@waddle.test/desktop"),
        "forbidden third-party accept must not be forwarded"
    );
}

#[test]
fn xep_0166_dm_session_accept_rejects_retargeted_initiator_resource() {
    // Alice's desktop resource initiated the call. Even the invited
    // responder must not retarget the accept to a different Alice
    // resource under the same bare JID and obtain a token for that
    // non-party full JID.
    let sfu = fixture_livekit_sfu();
    let handler = JingleHandler::new(sfu);
    let alice_desktop: FullJid = "alice@waddle.test/desktop".parse().expect("valid JID");
    let bob: FullJid = "bob@waddle.test/phone".parse().expect("valid JID");

    let initiate = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-retargeted-initiator",
    );
    let initiate_events = handler.handle(&initiate, &ctx(&alice_desktop));
    assert!(
        has_route_to(&initiate_events, "bob@waddle.test/phone"),
        "session-initiate should route the token-bearing invite to Bob: {initiate_events:?}",
    );

    let retargeted_accept = dm_jingle_iq(
        Action::SessionAccept,
        "bob@waddle.test/phone",
        "alice@waddle.test/mobile",
        "dm-retargeted-initiator",
    );
    let accept_events = handler.handle(&retargeted_accept, &ctx(&bob));

    assert_eq!(
        first_error_condition(&accept_events),
        Some(DefinedCondition::Forbidden),
        "invited responder must not retarget accept to a different initiator resource: {accept_events:?}",
    );
    assert!(
        !has_route_to(&accept_events, "alice@waddle.test/mobile"),
        "forbidden retargeted accept must not be forwarded"
    );
}

#[test]
fn xep_0166_dm_rejects_client_supplied_issued_transport_on_session_initiate() {
    // Clients request LiveKit credentials with an empty Waddle
    // transport. A pre-populated issued transport is not a proof and
    // must not be forwarded through the server-mediated minting path.
    let sfu = fixture_livekit_sfu();
    let handler = JingleHandler::new(sfu);
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("valid JID");
    let initiate = dm_jingle_iq_with_content(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-issued-initiate",
        audio_content_with_issued_waddle_transport(),
    );

    let events = handler.handle(&initiate, &ctx(&alice));

    assert_eq!(
        first_error_condition(&events),
        Some(DefinedCondition::BadRequest)
    );
    assert!(
        !has_route_to(&events, "bob@waddle.test/phone"),
        "client-supplied issued transport must not be forwarded: {events:?}",
    );
}

#[test]
fn xep_0166_dm_rejects_client_supplied_issued_transport_on_session_accept() {
    // The responder's session-accept is also a credential request,
    // not a place where clients may inject already-issued tokens.
    // The pending invite is restored after this rejection so a
    // corrected accept can still succeed.
    let sfu = fixture_livekit_sfu();
    let handler = JingleHandler::new(sfu);
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("valid JID");
    let bob: FullJid = "bob@waddle.test/phone".parse().expect("valid JID");

    let initiate = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-issued-accept",
    );
    assert!(has_route_to(
        &handler.handle(&initiate, &ctx(&alice)),
        "bob@waddle.test/phone"
    ));

    let issued_accept = dm_jingle_iq_with_content(
        Action::SessionAccept,
        "bob@waddle.test/phone",
        "alice@waddle.test/desktop",
        "dm-issued-accept",
        audio_content_with_issued_waddle_transport(),
    );
    let issued_events = handler.handle(&issued_accept, &ctx(&bob));
    assert_eq!(
        first_error_condition(&issued_events),
        Some(DefinedCondition::BadRequest),
        "client-supplied issued accept must be rejected: {issued_events:?}",
    );
    assert!(
        !has_route_to(&issued_events, "alice@waddle.test/desktop"),
        "bad issued accept must not be forwarded",
    );

    let corrected_accept = dm_jingle_iq(
        Action::SessionAccept,
        "bob@waddle.test/phone",
        "alice@waddle.test/desktop",
        "dm-issued-accept",
    );
    let corrected_events = handler.handle(&corrected_accept, &ctx(&bob));
    assert!(
        has_route_to(&corrected_events, "alice@waddle.test/desktop"),
        "rejected issued accept must not burn the pending invite: {corrected_events:?}",
    );
}

#[test]
fn xep_0166_dm_session_accept_consumes_prior_invite() {
    // A DM responder invite is a one-time authorization to rewrite
    // the responder's session-accept transport. A second accept with
    // the same sid must not mint another token without a fresh invite.
    let sfu = fixture_livekit_sfu();
    let handler = JingleHandler::new(sfu);
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("valid JID");
    let bob: FullJid = "bob@waddle.test/phone".parse().expect("valid JID");

    let initiate = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-one-shot",
    );
    let initiate_events = handler.handle(&initiate, &ctx(&alice));
    assert!(
        has_route_to(&initiate_events, "bob@waddle.test/phone"),
        "session-initiate should route the token-bearing invite to Bob: {initiate_events:?}",
    );

    let accept = dm_jingle_iq(
        Action::SessionAccept,
        "bob@waddle.test/phone",
        "alice@waddle.test/desktop",
        "dm-one-shot",
    );
    let first_accept_events = handler.handle(&accept, &ctx(&bob));
    assert!(
        has_route_to(&first_accept_events, "alice@waddle.test/desktop"),
        "first accept consumes the pending invite and routes to Alice: {first_accept_events:?}",
    );

    let replay_events = handler.handle(&accept, &ctx(&bob));
    assert_eq!(
        first_error_condition(&replay_events),
        Some(DefinedCondition::Forbidden),
        "replayed accept must not reuse the consumed invite: {replay_events:?}",
    );
}

#[test]
fn xep_0166_dm_concurrent_session_accept_consumes_invite_once() {
    // The pending invite is claimed under the mutex before token
    // minting. Two concurrent accepts for the same invited full JID
    // must not both mint and route.
    let sfu = fixture_livekit_sfu();
    let handler = JingleHandler::new(sfu);
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("valid JID");
    let bob: FullJid = "bob@waddle.test/phone".parse().expect("valid JID");

    let initiate = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-concurrent-accept",
    );
    assert!(has_route_to(
        &handler.handle(&initiate, &ctx(&alice)),
        "bob@waddle.test/phone"
    ));

    let accept = dm_jingle_iq(
        Action::SessionAccept,
        "bob@waddle.test/phone",
        "alice@waddle.test/desktop",
        "dm-concurrent-accept",
    );
    let barrier = Arc::new(Barrier::new(3));
    let first_handler = handler.clone();
    let second_handler = handler.clone();
    let first_accept = accept.clone();
    let second_accept = accept;
    let first_bob = bob.clone();
    let second_bob = bob;
    let first_barrier = barrier.clone();
    let second_barrier = barrier.clone();

    let first = thread::spawn(move || {
        first_barrier.wait();
        first_handler.handle(&first_accept, &ctx(&first_bob))
    });
    let second = thread::spawn(move || {
        second_barrier.wait();
        second_handler.handle(&second_accept, &ctx(&second_bob))
    });
    barrier.wait();

    let first_events = first.join().expect("first accept thread joins");
    let second_events = second.join().expect("second accept thread joins");
    let routed = usize::from(has_route_to(&first_events, "alice@waddle.test/desktop"))
        + usize::from(has_route_to(&second_events, "alice@waddle.test/desktop"));
    let forbidden =
        usize::from(first_error_condition(&first_events) == Some(DefinedCondition::Forbidden))
            + usize::from(
                first_error_condition(&second_events) == Some(DefinedCondition::Forbidden),
            );

    assert_eq!(routed, 1, "only one concurrent accept may route");
    assert_eq!(
        forbidden, 1,
        "the losing concurrent accept must be forbidden"
    );
}

#[test]
fn xep_0166_dm_failed_session_accept_keeps_invite_retryable() {
    // The invite is consumed only after the accept transport rewrite
    // succeeds. A malformed/unsupported accept can be corrected and
    // retried without requiring a new session-initiate.
    let sfu = fixture_livekit_sfu();
    let handler = JingleHandler::new(sfu);
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("valid JID");
    let bob: FullJid = "bob@waddle.test/phone".parse().expect("valid JID");

    let initiate = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-retry-accept",
    );
    assert!(has_route_to(
        &handler.handle(&initiate, &ctx(&alice)),
        "bob@waddle.test/phone"
    ));

    let mut bad_content = Content::new(Creator::Initiator, ContentId("audio".into()));
    bad_content.transport = Some(Transport::Unknown(
        Element::builder("transport", "urn:xmpp:jingle:transports:ice-udp:1").build(),
    ));
    let mut bad_jingle = Jingle::new(Action::SessionAccept, SessionId("dm-retry-accept".into()));
    bad_jingle.responder = Some("bob@waddle.test/phone".parse().expect("valid responder"));
    bad_jingle.contents.push(bad_content);
    let bad_accept = Iq::Set {
        from: Some("bob@waddle.test/phone".parse().expect("valid from")),
        to: Some("alice@waddle.test/desktop".parse().expect("valid to")),
        id: "bad-accept".into(),
        payload: bad_jingle.into(),
    };

    let bad_events = handler.handle(&bad_accept, &ctx(&bob));
    assert!(
        bad_events
            .iter()
            .any(|event| matches!(event, OutboundEvent::SendStanza(_))),
        "unsupported transport should fail before consuming the invite: {bad_events:?}",
    );

    let corrected_accept = dm_jingle_iq(
        Action::SessionAccept,
        "bob@waddle.test/phone",
        "alice@waddle.test/desktop",
        "dm-retry-accept",
    );
    let corrected_events = handler.handle(&corrected_accept, &ctx(&bob));
    assert!(
        has_route_to(&corrected_events, "alice@waddle.test/desktop"),
        "corrected accept should still be authorized after failed rewrite: {corrected_events:?}",
    );
}

#[test]
fn xep_0166_dm_session_terminate_clears_pending_invite() {
    // If the initiator hangs up before the responder accepts, the
    // pending invite must be removed. A later stale accept must not
    // mint or route into the ended call.
    let sfu = fixture_livekit_sfu();
    let handler = JingleHandler::new(sfu);
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("valid JID");
    let bob: FullJid = "bob@waddle.test/phone".parse().expect("valid JID");

    let initiate = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-terminated-before-accept",
    );
    assert!(has_route_to(
        &handler.handle(&initiate, &ctx(&alice)),
        "bob@waddle.test/phone"
    ));

    let terminate = dm_jingle_iq(
        Action::SessionTerminate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-terminated-before-accept",
    );
    assert!(has_route_to(
        &handler.handle(&terminate, &ctx(&alice)),
        "bob@waddle.test/phone"
    ));

    let stale_accept = dm_jingle_iq(
        Action::SessionAccept,
        "bob@waddle.test/phone",
        "alice@waddle.test/desktop",
        "dm-terminated-before-accept",
    );
    let stale_events = handler.handle(&stale_accept, &ctx(&bob));
    assert_eq!(
        first_error_condition(&stale_events),
        Some(DefinedCondition::Forbidden),
        "terminate must clear the pending invite: {stale_events:?}",
    );
    assert!(
        !has_route_to(&stale_events, "alice@waddle.test/desktop"),
        "stale accept after terminate must not be forwarded",
    );
}

#[test]
fn xep_0166_dm_session_accept_rejects_stale_participant_on_reused_sid() {
    // The LiveKit room id is `initiator-bare::sid`. If Alice reuses
    // a sid, a stale SFU registry entry must not be enough to mint
    // a fresh token. Authorization is bound to the current invite's
    // responder full JID.
    let sfu = fixture_livekit_sfu();
    let call_id =
        waddle_sfu::CallId::new("alice@waddle.test::dm-reused").expect("valid scoped call id");
    let eve: FullJid = "eve@waddle.test/laptop".parse().expect("valid JID");
    let eve_identity = Identity::from_jid(eve.clone());
    sfu.register_call_participant(&call_id, &eve_identity);
    assert!(
        sfu.has_call_participant(&call_id, &eve_identity),
        "fixture starts with Eve as a stale participant in the reused room"
    );
    let handler = JingleHandler::new(sfu);
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("valid JID");

    let fresh_invite = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-reused",
    );
    let initiate_events = handler.handle(&fresh_invite, &ctx(&alice));
    assert!(
        has_route_to(&initiate_events, "bob@waddle.test/phone"),
        "fresh invite should route to the current responder: {initiate_events:?}",
    );

    let stale_accept = dm_jingle_iq(
        Action::SessionAccept,
        "eve@waddle.test/laptop",
        "alice@waddle.test/desktop",
        "dm-reused",
    );
    let accept_events = handler.handle(&stale_accept, &ctx(&eve));

    assert_eq!(
        first_error_condition(&accept_events),
        Some(DefinedCondition::Forbidden),
        "stale participant must not satisfy the current invite proof: {accept_events:?}",
    );
}

#[test]
fn xep_0166_dm_superseded_invite_revokes_previous_responder() {
    // Reusing the same `sid` for a new responder must supersede the
    // old 1:1 invite. The old responder's already-minted token state
    // is revoked by unregistering them from the scoped SFU room.
    let sfu = fixture_livekit_sfu();
    let handler = JingleHandler::new(sfu.clone());
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("valid JID");
    let bob: FullJid = "bob@waddle.test/phone".parse().expect("valid JID");
    let charlie: FullJid = "charlie@waddle.test/tablet".parse().expect("valid JID");
    let call_id =
        waddle_sfu::CallId::new("alice@waddle.test::dm-superseded").expect("valid scoped call id");
    let bob_identity = Identity::from_jid(bob.clone());
    let charlie_identity = Identity::from_jid(charlie.clone());

    let first_invite = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-superseded",
    );
    let first_events = handler.handle(&first_invite, &ctx(&alice));
    assert!(
        has_route_to(&first_events, "bob@waddle.test/phone"),
        "first invite should route to Bob: {first_events:?}",
    );
    assert!(sfu.has_call_participant(&call_id, &bob_identity));

    let second_invite = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "charlie@waddle.test/tablet",
        "dm-superseded",
    );
    let second_events = handler.handle(&second_invite, &ctx(&alice));
    assert!(
        has_route_to(&second_events, "charlie@waddle.test/tablet"),
        "superseding invite should route to Charlie: {second_events:?}",
    );

    assert!(
        !sfu.has_call_participant(&call_id, &bob_identity),
        "superseded responder must be removed from the scoped SFU room"
    );
    assert!(
        sfu.has_call_participant(&call_id, &charlie_identity),
        "current responder remains registered for the scoped SFU room"
    );
}

#[test]
fn xep_0166_dm_superseded_invite_replaces_pending_accept_authorization() {
    // Reusing the same sid updates the pending accept proof. The
    // superseded responder can no longer accept, while the current
    // responder can.
    let sfu = fixture_livekit_sfu();
    let handler = JingleHandler::new(sfu);
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("valid JID");
    let bob: FullJid = "bob@waddle.test/phone".parse().expect("valid JID");
    let charlie: FullJid = "charlie@waddle.test/tablet".parse().expect("valid JID");

    let first_invite = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-superseded-accept",
    );
    assert!(has_route_to(
        &handler.handle(&first_invite, &ctx(&alice)),
        "bob@waddle.test/phone"
    ));

    let second_invite = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "charlie@waddle.test/tablet",
        "dm-superseded-accept",
    );
    assert!(has_route_to(
        &handler.handle(&second_invite, &ctx(&alice)),
        "charlie@waddle.test/tablet"
    ));

    let stale_bob_accept = dm_jingle_iq(
        Action::SessionAccept,
        "bob@waddle.test/phone",
        "alice@waddle.test/desktop",
        "dm-superseded-accept",
    );
    let bob_events = handler.handle(&stale_bob_accept, &ctx(&bob));
    assert_eq!(
        first_error_condition(&bob_events),
        Some(DefinedCondition::Forbidden),
        "superseded responder must not satisfy the current pending invite: {bob_events:?}",
    );

    let current_charlie_accept = dm_jingle_iq(
        Action::SessionAccept,
        "charlie@waddle.test/tablet",
        "alice@waddle.test/desktop",
        "dm-superseded-accept",
    );
    let charlie_events = handler.handle(&current_charlie_accept, &ctx(&charlie));
    assert!(
        has_route_to(&charlie_events, "alice@waddle.test/desktop"),
        "current responder should satisfy the replacement pending invite: {charlie_events:?}",
    );
}

#[test]
fn xep_0166_dm_accepted_call_reuse_revokes_previous_responder() {
    // Once Bob has accepted, the pending invite is gone. Reusing the
    // same sid for Charlie still has to evict Bob from the scoped SFU
    // room so Bob cannot terminate Charlie's superseding call.
    let sfu = fixture_livekit_sfu();
    let handler = JingleHandler::new(sfu.clone());
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("valid JID");
    let bob: FullJid = "bob@waddle.test/phone".parse().expect("valid JID");
    let call_id = waddle_sfu::CallId::new("alice@waddle.test::dm-accepted-reused")
        .expect("valid scoped call id");
    let bob_identity = Identity::from_jid(bob.clone());

    let first_invite = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-accepted-reused",
    );
    assert!(has_route_to(
        &handler.handle(&first_invite, &ctx(&alice)),
        "bob@waddle.test/phone"
    ));
    let bob_accept = dm_jingle_iq(
        Action::SessionAccept,
        "bob@waddle.test/phone",
        "alice@waddle.test/desktop",
        "dm-accepted-reused",
    );
    assert!(has_route_to(
        &handler.handle(&bob_accept, &ctx(&bob)),
        "alice@waddle.test/desktop"
    ));
    assert!(sfu.has_call_participant(&call_id, &bob_identity));

    let second_invite = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "charlie@waddle.test/tablet",
        "dm-accepted-reused",
    );
    assert!(has_route_to(
        &handler.handle(&second_invite, &ctx(&alice)),
        "charlie@waddle.test/tablet"
    ));
    assert!(
        !sfu.has_call_participant(&call_id, &bob_identity),
        "accepted stale responder must be revoked on same-sid reuse"
    );

    let stale_terminate = dm_jingle_iq(
        Action::SessionTerminate,
        "bob@waddle.test/phone",
        "alice@waddle.test/desktop",
        "dm-accepted-reused",
    );
    let terminate_events = handler.handle(&stale_terminate, &ctx(&bob));
    assert_eq!(
        first_error_condition(&terminate_events),
        Some(DefinedCondition::Forbidden),
        "revoked accepted responder must not terminate the replacement call: {terminate_events:?}",
    );
}

#[test]
fn xep_0166_dm_superseded_responder_cannot_terminate_current_call() {
    // A superseded responder is removed from the SFU room and must
    // not be able to route a stale session-terminate for the reused
    // sid to the initiator.
    let sfu = fixture_livekit_sfu();
    let handler = JingleHandler::new(sfu);
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("valid JID");
    let bob: FullJid = "bob@waddle.test/phone".parse().expect("valid JID");

    let first_invite = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-stale-terminate",
    );
    assert!(has_route_to(
        &handler.handle(&first_invite, &ctx(&alice)),
        "bob@waddle.test/phone"
    ));

    let second_invite = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "charlie@waddle.test/tablet",
        "dm-stale-terminate",
    );
    assert!(has_route_to(
        &handler.handle(&second_invite, &ctx(&alice)),
        "charlie@waddle.test/tablet"
    ));

    let terminate = dm_jingle_iq(
        Action::SessionTerminate,
        "bob@waddle.test/phone",
        "alice@waddle.test/desktop",
        "dm-stale-terminate",
    );
    let terminate_events = handler.handle(&terminate, &ctx(&bob));
    assert_eq!(
        first_error_condition(&terminate_events),
        Some(DefinedCondition::Forbidden),
        "superseded responder must not route stale terminate: {terminate_events:?}",
    );
    assert!(
        !has_route_to(&terminate_events, "alice@waddle.test/desktop"),
        "forbidden stale terminate must not be forwarded"
    );
}

#[test]
fn xep_0166_dm_session_terminate_requires_shared_call_membership() {
    // A caller's own live call with the same sid is not enough to
    // authorize a terminate to an unrelated peer. The sender and
    // addressed peer must share one scoped call id.
    let sfu = fixture_livekit_sfu();
    let eve_call = waddle_sfu::CallId::new("eve@waddle.test::dm-collision").expect("valid call id");
    let alice_call =
        waddle_sfu::CallId::new("alice@waddle.test::dm-collision").expect("valid call id");
    let eve: FullJid = "eve@waddle.test/laptop".parse().expect("valid JID");
    let mallory: FullJid = "mallory@waddle.test/phone".parse().expect("valid JID");
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("valid JID");
    let bob: FullJid = "bob@waddle.test/phone".parse().expect("valid JID");
    let eve_identity = Identity::from_jid(eve.clone());
    let mallory_identity = Identity::from_jid(mallory);
    let alice_identity = Identity::from_jid(alice);
    let bob_identity = Identity::from_jid(bob);
    sfu.register_call_participant(&eve_call, &eve_identity);
    sfu.register_call_participant(&eve_call, &mallory_identity);
    sfu.register_call_participant(&alice_call, &alice_identity);
    sfu.register_call_participant(&alice_call, &bob_identity);

    let handler = JingleHandler::new(sfu.clone());
    let terminate = dm_jingle_iq(
        Action::SessionTerminate,
        "eve@waddle.test/laptop",
        "bob@waddle.test/phone",
        "dm-collision",
    );
    let terminate_events = handler.handle(&terminate, &ctx(&eve));

    assert_eq!(
        first_error_condition(&terminate_events),
        Some(DefinedCondition::Forbidden),
        "sender and peer must share one scoped call: {terminate_events:?}",
    );
    assert_eq!(sfu.participant_count(&eve_call), 2);
    assert_eq!(sfu.participant_count(&alice_call), 2);
}

// ── §6.7 session-terminate ────────────────────────────────────────────

#[test]
fn xep_0166_session_terminate_with_success_reason() {
    // §6.7: session-terminate MAY carry a `<reason/>` child taking
    // one of the XEP-0166 §7.4 conditions. The typed
    // `xmpp_parsers::jingle::Reason::Success` flows through the
    // dedicated reason helper rather than `format!`.
    let term = session_terminate(SessionId("c1".into()), Reason::Success);
    let elem: Element = term.into();
    assert_eq!(elem.name(), "jingle");
    assert_eq!(elem.ns(), NS_JINGLE);
    assert_eq!(elem.attr("action"), Some("session-terminate"));
    assert_eq!(elem.attr("sid"), Some("c1"));

    let reason_child = elem
        .children()
        .find(|c| c.name() == "reason")
        .expect("<reason/> child required when reason supplied");
    assert_eq!(reason_child.ns(), NS_JINGLE);
    let condition = reason_child
        .children()
        .next()
        .expect("reason has condition child");
    assert_eq!(condition.name(), "success");
}

#[test]
fn xep_0166_session_terminate_carries_no_content_per_spec() {
    // §6.7: "the <jingle/> element of a session-terminate action
    // MUST NOT contain any <content/> elements." Waddle's typed
    // `session_terminate` helper enforces this by construction —
    // round-trip through the parser confirms it on the wire.
    let term = session_terminate(SessionId("c1".into()), Reason::Success);
    let elem: Element = term.into();
    let contents: Vec<&Element> = elem.children().filter(|c| c.name() == "content").collect();
    assert!(
        contents.is_empty(),
        "session-terminate MUST NOT carry <content/> children"
    );
}

#[test]
fn xep_0166_session_terminate_reason_helper_uses_typed_condition() {
    // The `reason_element` helper builds a typed
    // `xmpp_parsers::jingle::ReasonElement` directly — proving the
    // typed-payloads rule (no `&str` for the condition) — and
    // serialises to the canonical `<reason><$cond/></reason>` shape.
    for (reason, cond_name) in [
        (Reason::Success, "success"),
        (Reason::Decline, "decline"),
        (Reason::Busy, "busy"),
        (Reason::Cancel, "cancel"),
        (Reason::ConnectivityError, "connectivity-error"),
        (Reason::Expired, "expired"),
    ] {
        let mut jingle = Jingle::new(Action::SessionTerminate, SessionId("c1".into()));
        jingle = jingle.set_reason(reason_element(reason.clone()));
        let elem: Element = jingle.into();
        let reason_child = elem
            .children()
            .find(|c| c.name() == "reason")
            .expect("reason present");
        let cond = reason_child
            .children()
            .next()
            .expect("reason has condition");
        assert_eq!(
            cond.name(),
            cond_name,
            "reason {reason:?} must serialise to <{cond_name}/>"
        );
    }
}

// ── §6.8 transport-info ───────────────────────────────────────────────

#[test]
fn xep_0166_transport_info_round_trips() {
    // §6.8: transport-info carries the SAME sid as the running session
    // and one `<content/>` per transport-update. Waddle forwards these
    // unchanged with a sanitised `from` — the wire shape MUST survive
    // through the typed `Jingle::try_from` parse.
    let xml = r#"<jingle xmlns='urn:xmpp:jingle:1'
                         action='transport-info'
                         initiator='alice@waddle.test/desktop'
                         sid='c1'>
                   <content creator='initiator' name='audio'>
                     <transport xmlns='urn:waddle:transports:livekit:0'/>
                   </content>
                 </jingle>"#;
    let jingle = parse_jingle(xml);
    assert_eq!(jingle.action, Action::TransportInfo);
    assert_eq!(jingle.sid.0, "c1");
    assert_eq!(jingle.contents.len(), 1);
}

// ── Negative cases ────────────────────────────────────────────────────

#[test]
fn xep_0166_non_jingle_namespace_is_rejected() {
    // A `<jingle/>` element in a non-Jingle namespace MUST NOT parse
    // as `Jingle`. Waddle's handler relies on this to reject
    // hand-rolled lookalikes that try to bypass the typed contract.
    let elem = Element::builder("jingle", "urn:waddle:not-jingle")
        .attr(
            minidom::rxml::xml_ncname!("action").to_owned(),
            "session-initiate",
        )
        .attr(minidom::rxml::xml_ncname!("sid").to_owned(), "c1")
        .build();
    assert!(Jingle::try_from(elem).is_err());
}

#[test]
fn xep_0166_unknown_action_is_rejected() {
    // §7.2: the `action` attribute is constrained to the 14 values
    // enumerated in the spec. xmpp-parsers' typed `Action` enum will
    // refuse to parse anything else.
    let elem = Element::builder("jingle", NS_JINGLE)
        .attr(
            minidom::rxml::xml_ncname!("action").to_owned(),
            "session-warp",
        )
        .attr(minidom::rxml::xml_ncname!("sid").to_owned(), "c1")
        .build();
    assert!(Jingle::try_from(elem).is_err());
}

#[test]
fn xep_0166_session_terminate_typed_helper_round_trips() {
    // The `session_terminate` helper builds via the typed
    // `Jingle::new(...).set_reason(...)` chain. Round-trip the
    // serialised form through the parser to prove no information is
    // lost across the typed/wire boundary.
    let term = session_terminate(SessionId("c-roundtrip".into()), Reason::Cancel);
    let elem: Element = term.into();
    let reparsed = Jingle::try_from(elem).expect("reparses");
    assert_eq!(reparsed.action, Action::SessionTerminate);
    assert_eq!(reparsed.sid.0, "c-roundtrip");
    let reason = reparsed.reason.expect("reason preserved");
    assert!(matches!(reason.reason, Reason::Cancel));
}

// ── #1131 survivor / unknown-session terminate ───────────────────────

fn first_result_ack(events: &[OutboundEvent]) -> Option<&Iq> {
    events.iter().find_map(|event| {
        let OutboundEvent::SendStanza(stanza) = event else {
            return None;
        };
        let Stanza::Iq(reply) = stanza.as_ref() else {
            return None;
        };
        matches!(reply.as_ref(), Iq::Result { .. }).then_some(reply.as_ref())
    })
}

fn first_stanza_error(
    events: &[OutboundEvent],
) -> Option<&xmpp_parsers::stanza_error::StanzaError> {
    events.iter().find_map(|event| {
        let OutboundEvent::SendStanza(stanza) = event else {
            return None;
        };
        let Stanza::Iq(reply) = stanza.as_ref() else {
            return None;
        };
        let Iq::Error { error, .. } = reply.as_ref() else {
            return None;
        };
        Some(error)
    })
}

#[test]
fn xep_0166_survivor_terminate_after_peer_swept_acks_and_cleans_up() {
    // #1131: Bob's client crashed; the LiveKit webhook (or the
    // reconciler) already removed his registration. Alice's hangup
    // must NOT be `<forbidden/>` — the server unregisters her,
    // revokes her JTIs, and acks the terminate per XEP-0166 §6.7 on
    // the departed peer's behalf.
    let sfu = fixture_livekit_sfu();
    let handler = JingleHandler::new(sfu.clone());
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("valid JID");
    let bob: FullJid = "bob@waddle.test/phone".parse().expect("valid JID");
    let call = waddle_sfu::CallId::new("alice@waddle.test::dm-survivor").expect("valid call id");
    let alice_identity = Identity::from_jid(alice.clone());
    let bob_identity = Identity::from_jid(bob.clone());

    let invite = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-survivor",
    );
    assert!(has_route_to(
        &handler.handle(&invite, &ctx(&alice)),
        "bob@waddle.test/phone"
    ));
    let accept = dm_jingle_iq(
        Action::SessionAccept,
        "bob@waddle.test/phone",
        "alice@waddle.test/desktop",
        "dm-survivor",
    );
    assert!(has_route_to(
        &handler.handle(&accept, &ctx(&bob)),
        "alice@waddle.test/desktop"
    ));
    // The accept minted Alice's join token; she now holds a live JTI.
    assert_eq!(sfu.issued_count(&call, &alice_identity), 1);

    // Peer crash: the webhook's registry cleanup removed Bob.
    sfu.note_participant_left(&call, &bob_identity, None);
    assert!(!sfu.has_call_participant(&call, &bob_identity));
    assert!(sfu.has_call_participant(&call, &alice_identity));

    // Survivor hangup.
    let terminate = dm_jingle_iq(
        Action::SessionTerminate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-survivor",
    );
    let events = handler.handle(&terminate, &ctx(&alice));

    assert_eq!(
        first_error_condition(&events),
        None,
        "survivor terminate must not error (#1131): {events:?}"
    );
    let ack = first_result_ack(&events).expect("server must ack the survivor terminate");
    assert_eq!(ack.id(), terminate.id(), "ack answers the terminate IQ");
    assert!(
        matches!(ack, Iq::Result { payload: None, .. }),
        "XEP-0166 §6.7 termination ack is an EMPTY IQ result"
    );
    assert_eq!(
        sfu.participant_count(&call),
        0,
        "survivor terminate must unregister the remaining party"
    );
    assert_eq!(
        sfu.issued_count(&call, &alice_identity),
        0,
        "survivor terminate must revoke the survivor's JTIs"
    );
}

#[test]
fn xep_0166_unknown_session_terminate_returns_item_not_found_unknown_session() {
    // #1131 / XEP-0166 error table: a terminate for a sid with no
    // live session anywhere gets `<item-not-found/>` plus the
    // application-specific `<unknown-session/>` — never
    // `<forbidden/>`, and no state is touched (idempotent).
    let sfu = fixture_livekit_sfu();
    let handler = JingleHandler::new(sfu);
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("valid JID");

    let terminate = dm_jingle_iq(
        Action::SessionTerminate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-never-existed",
    );
    let events = handler.handle(&terminate, &ctx(&alice));

    assert_eq!(
        first_error_condition(&events),
        Some(DefinedCondition::ItemNotFound),
        "unknown session terminate must be item-not-found: {events:?}"
    );
    let error = first_stanza_error(&events).expect("stanza error present");
    let unknown = error
        .other
        .as_ref()
        .expect("application-specific condition present");
    assert_eq!(unknown.name(), "unknown-session");
    assert_eq!(unknown.ns(), "urn:xmpp:jingle:errors:1");
}

#[test]
fn xep_0166_terminate_glare_second_terminate_is_not_forbidden() {
    // #1131: both parties hang up at once. The first terminate tears
    // the session down; the second must be answered with the
    // idempotent unknown-session shape, not `<forbidden/>`.
    let sfu = fixture_livekit_sfu();
    let handler = JingleHandler::new(sfu.clone());
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("valid JID");
    let bob: FullJid = "bob@waddle.test/phone".parse().expect("valid JID");
    let call = waddle_sfu::CallId::new("alice@waddle.test::dm-glare").expect("valid call id");

    let invite = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-glare",
    );
    assert!(has_route_to(
        &handler.handle(&invite, &ctx(&alice)),
        "bob@waddle.test/phone"
    ));

    let alice_terminate = dm_jingle_iq(
        Action::SessionTerminate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-glare",
    );
    let first = handler.handle(&alice_terminate, &ctx(&alice));
    assert!(
        has_route_to(&first, "bob@waddle.test/phone"),
        "live terminate forwards to the peer: {first:?}"
    );
    assert_eq!(sfu.participant_count(&call), 0);

    let bob_terminate = dm_jingle_iq(
        Action::SessionTerminate,
        "bob@waddle.test/phone",
        "alice@waddle.test/desktop",
        "dm-glare",
    );
    let second = handler.handle(&bob_terminate, &ctx(&bob));
    assert_ne!(
        first_error_condition(&second),
        Some(DefinedCondition::Forbidden),
        "terminate glare must not be forbidden (#1131): {second:?}"
    );
    assert_eq!(
        first_error_condition(&second),
        Some(DefinedCondition::ItemNotFound),
        "glare loser gets the idempotent unknown-session shape: {second:?}"
    );
    assert_eq!(
        sfu.participant_count(&call),
        0,
        "no state change (idempotent)"
    );
}

#[test]
fn xep_0166_duplicate_terminate_is_idempotent() {
    // #1131: a client retrying its own terminate (lost ack) must not
    // see `<forbidden/>` on the retry.
    let sfu = fixture_livekit_sfu();
    let handler = JingleHandler::new(sfu.clone());
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("valid JID");
    let call = waddle_sfu::CallId::new("alice@waddle.test::dm-dup").expect("valid call id");

    let invite = dm_jingle_iq(
        Action::SessionInitiate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-dup",
    );
    assert!(has_route_to(
        &handler.handle(&invite, &ctx(&alice)),
        "bob@waddle.test/phone"
    ));

    let terminate = dm_jingle_iq(
        Action::SessionTerminate,
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-dup",
    );
    let first = handler.handle(&terminate, &ctx(&alice));
    assert!(has_route_to(&first, "bob@waddle.test/phone"));
    assert_eq!(sfu.participant_count(&call), 0);

    let retry = handler.handle(&terminate, &ctx(&alice));
    assert_ne!(
        first_error_condition(&retry),
        Some(DefinedCondition::Forbidden),
        "duplicate terminate must not be forbidden: {retry:?}"
    );
    assert_eq!(
        first_error_condition(&retry),
        Some(DefinedCondition::ItemNotFound),
        "duplicate terminate resolves to unknown-session: {retry:?}"
    );
}

// ── #1142 one JWT per negotiation stanza ─────────────────────────────

fn video_content_with_waddle_transport() -> Content {
    let mut content = Content::new(Creator::Initiator, ContentId("video".into()));
    let description = Element::builder("description", NS_JINGLE_RTP)
        .attr(minidom::rxml::xml_ncname!("media").to_owned(), "video")
        .build();
    content.description = Some(xmpp_parsers::jingle::Description::Rtp(
        xmpp_parsers::jingle_rtp::Description::try_from(description).expect("rtp desc"),
    ));
    let transport = Element::builder("transport", NS_WADDLE_LIVEKIT_TRANSPORT).build();
    content.transport = Some(Transport::Unknown(transport));
    content
}

fn two_content_initiate(from: &str, to: &str, sid: &str) -> Iq {
    let mut jingle = Jingle::new(Action::SessionInitiate, SessionId(sid.into()));
    jingle.initiator = Some(from.parse().expect("valid initiator JID"));
    jingle.contents.push(audio_content_with_waddle_transport());
    jingle.contents.push(video_content_with_waddle_transport());
    Iq::Set {
        from: Some(from.parse().expect("valid from JID")),
        to: Some(to.parse().expect("valid to JID")),
        id: format!("jingle-{sid}"),
        payload: jingle.into(),
    }
}

fn forwarded_content_tokens(events: &[OutboundEvent]) -> Vec<String> {
    use waddle_xmpp::xep::xep_waddle_livekit_transport::WaddleLiveKitTransport;
    let forwarded = events
        .iter()
        .find_map(|event| {
            let OutboundEvent::RouteToConnection { stanza, .. } = event else {
                return None;
            };
            let Stanza::Iq(iq) = stanza.as_ref() else {
                return None;
            };
            let Iq::Set { payload, .. } = iq.as_ref() else {
                return None;
            };
            Jingle::try_from(payload.clone()).ok()
        })
        .expect("forwarded jingle stanza present");
    forwarded
        .contents
        .iter()
        .map(|content| {
            let Some(Transport::Unknown(elem)) = &content.transport else {
                panic!("content missing Waddle transport");
            };
            match WaddleLiveKitTransport::try_from(elem).expect("transport parses") {
                WaddleLiveKitTransport::Issued(issued) => issued.token.as_str().to_string(),
                WaddleLiveKitTransport::Request => panic!("server must issue the transport"),
            }
        })
        .collect()
}

#[test]
fn xep_0166_multi_content_stanza_mints_one_shared_token() {
    // #1142: an audio+video session-initiate must mint exactly ONE
    // join token (one JTI) for the peer and stamp the same issued
    // transport into every `<content/>` — per the LiveKit model of
    // one identity/credential per participant.
    let sfu = fixture_livekit_sfu();
    let handler = JingleHandler::new(sfu.clone());
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("valid JID");
    let bob: FullJid = "bob@waddle.test/phone".parse().expect("valid JID");
    let call = waddle_sfu::CallId::new("alice@waddle.test::dm-two-content").expect("call id");
    let bob_identity = Identity::from_jid(bob);

    let invite = two_content_initiate(
        "alice@waddle.test/desktop",
        "bob@waddle.test/phone",
        "dm-two-content",
    );
    let events = handler.handle(&invite, &ctx(&alice));

    let tokens = forwarded_content_tokens(&events);
    assert_eq!(tokens.len(), 2, "both contents carry an issued transport");
    assert_eq!(
        tokens[0], tokens[1],
        "audio and video contents must share ONE token (#1142)"
    );
    assert_eq!(
        sfu.issued_count(&call, &bob_identity),
        1,
        "exactly one JTI minted per negotiation stanza"
    );
}

#[test]
fn xep_0166_multi_content_renegotiations_do_not_burn_jti_budget() {
    // #1142: repeated two-content renegotiations of the same call
    // must consume one JTI each, not one per content — otherwise the
    // 16-slot per-participant FIFO evicts still-live JTIs which then
    // can never be revoked on hangup.
    let sfu = fixture_livekit_sfu();
    let handler = JingleHandler::new(sfu.clone());
    let alice: FullJid = "alice@waddle.test/desktop".parse().expect("valid JID");
    let bob: FullJid = "bob@waddle.test/phone".parse().expect("valid JID");
    let call = waddle_sfu::CallId::new("alice@waddle.test::dm-renegotiate").expect("call id");
    let bob_identity = Identity::from_jid(bob);

    for round in 1..=3usize {
        let invite = two_content_initiate(
            "alice@waddle.test/desktop",
            "bob@waddle.test/phone",
            "dm-renegotiate",
        );
        let events = handler.handle(&invite, &ctx(&alice));
        assert!(
            has_route_to(&events, "bob@waddle.test/phone"),
            "renegotiation {round} forwards: {events:?}"
        );
        assert_eq!(
            sfu.issued_count(&call, &bob_identity),
            round,
            "each two-content stanza mints exactly one JTI"
        );
    }
}

mod undeliverable;
