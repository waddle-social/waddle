//! XEP-0488: MUC Token Invite — dedicated suite.
//!
//! Pins:
//! - the registrar namespace `urn:xmpp:muc-token-invite:0`,
//! - the request/response IQ shapes (`<request/>` set to the room,
//!   `<invite token/>` result mirroring id and addressing),
//! - the message-embedded `<invite token jid/>` share shape and its
//!   `xmpp:…?join;password=TOKEN` URI rendering,
//! - extraction robustness (empty/missing token, wrong namespace,
//!   IQ result direction/room recovery).
//!
//! Known spec divergence (reported in issue #1150, not pinned as
//! conformant): xep-0488.xml replies with a `<token>` element whose
//! token is the TEXT CONTENT (`<token xmlns='…'>abc</token>`), and
//! shares tokens only via the `?join;password=TOKEN` URI. This module
//! instead replies `<invite token='…'/>` and adds a message-embedded
//! `<invite token jid/>` share element the spec does not define. Only
//! the URI rendering below is fully conformant; the rest pins actual
//! behaviour pending reconciliation.

use minidom::Element;
use waddle_xmpp::xep::xep0488::{
    build_invite_message_element, build_invite_request, build_invite_response,
    build_invite_share_message, extract_invite_from_iq, extract_invite_from_message,
    has_invite_in_message, is_invite_element, is_invite_request, set_invite_on_message,
    strip_invite_from_message, InviteToken, InviteTokenCarrier, NS_MUC_TOKEN_INVITE,
};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Message, MessageType};

// ── Namespace exactness ──────────────────────────────────────────────

#[test]
fn xep0488_namespace_matches_spec() {
    // xep-0488.xml registrar entry.
    assert_eq!(NS_MUC_TOKEN_INVITE, "urn:xmpp:muc-token-invite:0");
}

// ── IQ request/response round-trip ───────────────────────────────────

#[test]
fn xep0488_request_iq_has_spec_shape() {
    let room: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
    let iq = build_invite_request(room.clone(), "inv-1");

    assert!(is_invite_request(&iq));
    match &iq {
        Iq::Set {
            to, id, payload, ..
        } => {
            assert_eq!(to.as_ref(), Some(&room));
            assert_eq!(id, "inv-1");
            assert_eq!(payload.name(), "request");
            assert_eq!(payload.ns(), NS_MUC_TOKEN_INVITE);
            assert_eq!(payload.children().count(), 0);
        }
        other => panic!("invite request must be an IQ set, got {other:?}"),
    }
}

#[test]
fn xep0488_response_mirrors_request_addressing_and_round_trips() {
    let room: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
    let request = build_invite_request(room.clone(), "inv-2");
    let response = build_invite_response(&request, "abc123def456");

    match &response {
        Iq::Result { from, id, .. } => {
            // The room the request was sent `to` becomes the result's
            // `from`, which is where the extractor recovers the room.
            assert_eq!(from.as_ref(), Some(&room));
            assert_eq!(id, "inv-2");
        }
        other => panic!("invite response must be an IQ result, got {other:?}"),
    }

    let token = extract_invite_from_iq(&response).expect("token present");
    assert_eq!(token.token, "abc123def456");
    assert_eq!(token.room_jid.as_deref(), Some("room@muc.example.com"));
}

#[test]
fn xep0488_extract_from_iq_rejects_non_invite_results() {
    // A result carrying some other payload must not be misread.
    let other = Iq::Result {
        from: None,
        to: None,
        id: "x-1".to_owned(),
        payload: Some(Element::builder("query", "jabber:iq:roster").build()),
    };
    assert!(extract_invite_from_iq(&other).is_none());

    // An invite with an empty token is unusable.
    let empty = Iq::Result {
        from: None,
        to: None,
        id: "x-2".to_owned(),
        payload: Some(
            Element::builder("invite", NS_MUC_TOKEN_INVITE)
                .attr(minidom::rxml::xml_ncname!("token").to_owned(), "")
                .build(),
        ),
    };
    assert!(extract_invite_from_iq(&empty).is_none());
}

#[test]
fn xep0488_is_invite_request_rejects_foreign_payloads() {
    let foreign = Iq::Set {
        from: None,
        to: None,
        id: "f-1".to_owned(),
        payload: Element::builder("request", "urn:xmpp:other:0").build(),
    };
    assert!(!is_invite_request(&foreign));

    let wrong_name = Iq::Set {
        from: None,
        to: None,
        id: "f-2".to_owned(),
        payload: Element::builder("invite", NS_MUC_TOKEN_INVITE).build(),
    };
    assert!(!is_invite_request(&wrong_name));
}

// ── Message-embedded invite ──────────────────────────────────────────

#[test]
fn xep0488_message_invite_survives_serialize_reparse_round_trip() {
    let mut msg = Message::new(None::<jid::Jid>);
    set_invite_on_message(&mut msg, "xyz789", "room@muc.example.com");

    let elem = Element::from(msg);
    let xml = String::from(&elem);
    let reparsed =
        Message::try_from(xml.parse::<Element>().expect("reparses")).expect("valid message");

    assert!(has_invite_in_message(&reparsed));
    assert!(reparsed.has_invite_token());
    let token = reparsed.invite_token().expect("token present");
    assert_eq!(token.token, "xyz789");
    assert_eq!(token.room_jid.as_deref(), Some("room@muc.example.com"));
}

#[test]
fn xep0488_share_message_carries_join_uri_body_and_invite() {
    let to: jid::Jid = "friend@example.com".parse().expect("valid jid");
    let token = InviteToken::new("tok123").with_room("room@muc.example.com");

    let msg =
        build_invite_share_message(to.clone(), None::<jid::Jid>, &token).expect("token has a room");
    assert_eq!(msg.to, Some(to));
    assert_eq!(msg.type_, MessageType::Chat);

    let body = msg.bodies.values().next().expect("fallback body present");
    assert!(
        body.contains("xmpp:room@muc.example.com?join;password=tok123"),
        "body must embed the join URI: {body}"
    );

    let extracted = extract_invite_from_message(&msg).expect("invite present");
    assert_eq!(extracted.token, "tok123");
    assert_eq!(extracted.room_jid.as_deref(), Some("room@muc.example.com"));
}

#[test]
fn xep0488_share_message_requires_room_jid() {
    let to: jid::Jid = "friend@example.com".parse().expect("valid jid");
    let roomless = InviteToken::new("tok123");
    assert!(build_invite_share_message(to, None::<jid::Jid>, &roomless).is_none());
    assert_eq!(roomless.to_uri(), None);
}

#[test]
fn xep0488_to_uri_renders_join_with_password() {
    let token = InviteToken::new("abc123").with_room("room@muc.example.com");
    assert_eq!(
        token.to_uri().as_deref(),
        Some("xmpp:room@muc.example.com?join;password=abc123")
    );
}

// ── Extraction robustness ────────────────────────────────────────────

#[test]
fn xep0488_message_extract_requires_non_empty_token() {
    for invite in [
        "<invite xmlns='urn:xmpp:muc-token-invite:0' jid='room@muc'/>",
        "<invite xmlns='urn:xmpp:muc-token-invite:0' token='' jid='room@muc'/>",
    ] {
        let msg = Message::try_from(
            format!("<message xmlns='jabber:client' type='chat'>{invite}</message>")
                .parse::<Element>()
                .expect("valid xml"),
        )
        .expect("valid message");
        assert!(
            extract_invite_from_message(&msg).is_none(),
            "`{invite}` must not extract"
        );
    }
}

#[test]
fn xep0488_message_extract_tolerates_missing_room_jid() {
    let msg = Message::try_from(
        "<message xmlns='jabber:client' type='chat'>\
            <invite xmlns='urn:xmpp:muc-token-invite:0' token='t-1'/>\
        </message>"
            .parse::<Element>()
            .expect("valid xml"),
    )
    .expect("valid message");
    let token = extract_invite_from_message(&msg).expect("token present");
    assert_eq!(token.token, "t-1");
    assert_eq!(token.room_jid, None);
}

#[test]
fn xep0488_wrong_namespace_invite_is_not_recognized() {
    let msg = Message::try_from(
        "<message xmlns='jabber:client' type='chat'>\
            <invite xmlns='urn:xmpp:muc-token-invite:1' token='t' jid='room@muc'/>\
        </message>"
            .parse::<Element>()
            .expect("valid xml"),
    )
    .expect("valid message");
    assert!(!has_invite_in_message(&msg));
    assert!(extract_invite_from_message(&msg).is_none());

    let foreign = Element::builder("invite", "urn:xmpp:other:0").build();
    assert!(!is_invite_element(&foreign));
    assert!(is_invite_element(&build_invite_message_element("t", "r@m")));
}

// ── Mutation semantics ───────────────────────────────────────────────

#[test]
fn xep0488_set_replaces_prior_invite_keeping_exactly_one() {
    let mut msg = Message::new(None::<jid::Jid>);
    set_invite_on_message(&mut msg, "tok1", "room@muc.example.com");
    set_invite_on_message(&mut msg, "tok2", "room@muc.example.com");

    assert_eq!(
        msg.payloads
            .iter()
            .filter(|e| e.ns() == NS_MUC_TOKEN_INVITE)
            .count(),
        1
    );
    assert_eq!(
        extract_invite_from_message(&msg).expect("invite").token,
        "tok2"
    );

    strip_invite_from_message(&mut msg);
    assert!(!has_invite_in_message(&msg));
}
