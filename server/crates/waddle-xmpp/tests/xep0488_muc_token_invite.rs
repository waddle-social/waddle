//! XEP-0488: MUC Token Invite dedicated suite.
//!
//! Pins the request/response IQ shapes, text-content token replies, token
//! listing, revocation, and URI-only message sharing.

use minidom::Element;
use waddle_xmpp::xep::xep0488::{
    build_invite_request, build_invite_request_with_constraints, build_invite_response,
    build_invite_response_from_token, build_invite_share_message, build_revoke_request,
    build_revoke_response, build_tokens_request, build_tokens_response, extract_invite_from_iq,
    extract_invite_from_message, extract_invite_request_from_iq, extract_revoke_from_iq,
    extract_tokens_from_iq, has_invite_in_message, is_invite_element, is_invite_request,
    is_revoke_request, is_tokens_request, strip_invite_from_message, InviteToken,
    InviteTokenCarrier, InviteTokenRequest, NS_MUC_TOKEN_INVITE,
};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Message, MessageType};

#[test]
fn xep0488_namespace_matches_spec() {
    assert_eq!(NS_MUC_TOKEN_INVITE, "urn:xmpp:muc-token-invite:0");
}

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
fn xep0488_constrained_request_and_response_use_delay_counter_attrs() {
    let room: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
    let request = build_invite_request_with_constraints(
        room.clone(),
        "inv-constrained",
        &InviteTokenRequest::new()
            .with_delay(2_678_400)
            .with_counter(5),
    );
    let parsed_request = extract_invite_request_from_iq(&request).expect("request");
    assert_eq!(parsed_request.delay, Some(2_678_400));
    assert_eq!(parsed_request.counter, Some(5));

    let Iq::Set { payload, .. } = &request else {
        panic!("request must be IQ set");
    };
    assert_eq!(payload.attr("delay"), Some("2678400"));
    assert_eq!(payload.attr("counter"), Some("5"));

    let response = build_invite_response_from_token(
        &request,
        &InviteToken::new("abc123def456")
            .with_delay(604_800)
            .with_counter(5),
    );
    let Iq::Result {
        payload: Some(payload),
        ..
    } = &response
    else {
        panic!("response must carry token");
    };
    assert_eq!(payload.text(), "abc123def456");
    assert_eq!(payload.attr("delay"), Some("604800"));
    assert_eq!(payload.attr("counter"), Some("5"));

    let parsed_token = extract_invite_from_iq(&response).expect("token");
    assert_eq!(parsed_token.delay, Some(604_800));
    assert_eq!(parsed_token.counter, Some(5));
    assert_eq!(
        parsed_token.room_jid.as_deref(),
        Some("room@muc.example.com")
    );
}

#[test]
fn xep0488_response_token_is_text_content() {
    let room: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
    let request = build_invite_request(room.clone(), "inv-2");
    let response = build_invite_response(&request, "abc123def456");

    let Iq::Result {
        from,
        id,
        payload: Some(payload),
        ..
    } = &response
    else {
        panic!("invite response must be an IQ result with payload");
    };
    assert_eq!(from.as_ref(), Some(&room));
    assert_eq!(id, "inv-2");
    assert_eq!(payload.name(), "token");
    assert_eq!(payload.ns(), NS_MUC_TOKEN_INVITE);
    assert_eq!(payload.attr("token"), None);
    assert_eq!(payload.text(), "abc123def456");

    let token = extract_invite_from_iq(&response).expect("token present");
    assert_eq!(token.token, "abc123def456");
    assert_eq!(token.room_jid.as_deref(), Some("room@muc.example.com"));
}

#[test]
fn xep0488_extract_from_iq_rejects_empty_or_attribute_only_tokens() {
    let empty = Iq::Result {
        from: None,
        to: None,
        id: "x-1".to_owned(),
        payload: Some(Element::builder("token", NS_MUC_TOKEN_INVITE).build()),
    };
    assert!(extract_invite_from_iq(&empty).is_none());

    let attribute_only = Iq::Result {
        from: None,
        to: None,
        id: "x-2".to_owned(),
        payload: Some(
            Element::builder("token", NS_MUC_TOKEN_INVITE)
                .attr(minidom::rxml::xml_ncname!("token").to_owned(), "legacy")
                .build(),
        ),
    };
    assert!(extract_invite_from_iq(&attribute_only).is_none());
}

#[test]
fn xep0488_tokens_listing_round_trips() {
    let room: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
    let request = build_tokens_request(room.clone(), "list-1");
    assert!(is_tokens_request(&request));

    let response = build_tokens_response(
        &request,
        &[
            InviteToken::new("one").with_counter(1),
            InviteToken::new("two").with_delay(60),
        ],
    );
    let Iq::Result {
        payload: Some(payload),
        ..
    } = &response
    else {
        panic!("listing response must carry <tokens/>");
    };
    assert_eq!(payload.name(), "tokens");
    assert_eq!(payload.ns(), NS_MUC_TOKEN_INVITE);
    assert_eq!(payload.children().count(), 2);

    let parsed = extract_tokens_from_iq(&response);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].token, "one");
    assert_eq!(parsed[0].counter, Some(1));
    assert_eq!(parsed[0].room_jid.as_deref(), Some("room@muc.example.com"));
    assert_eq!(parsed[1].token, "two");
    assert_eq!(parsed[1].delay, Some(60));
}

#[test]
fn xep0488_revoke_request_and_empty_result_round_trip() {
    let room: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
    let request = build_revoke_request(room.clone(), "tok-1", "rev-1");
    assert!(is_revoke_request(&request));

    let token = extract_revoke_from_iq(&request).expect("revoke token");
    assert_eq!(token.token, "tok-1");
    assert_eq!(token.room_jid.as_deref(), Some("room@muc.example.com"));

    let response = build_revoke_response(&request);
    let Iq::Result {
        from, id, payload, ..
    } = response
    else {
        panic!("revoke response must be IQ result");
    };
    assert_eq!(from, Some(room));
    assert_eq!(id, "rev-1");
    assert_eq!(payload, None);
}

#[test]
fn xep0488_to_uri_renders_join_with_password() {
    let token = InviteToken::new("abc123").with_room("room@muc.example.com");
    assert_eq!(
        token.to_uri().as_deref(),
        Some("xmpp:room@muc.example.com?join;password=abc123")
    );
}

#[test]
fn xep0488_share_message_carries_join_uri_body_only() {
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
    assert!(msg.payloads.is_empty());
    assert!(!has_invite_in_message(&msg));
    assert!(extract_invite_from_message(&msg).is_none());
    assert!(!msg.has_invite_token());
}

#[test]
fn xep0488_share_message_requires_room_jid() {
    let to: jid::Jid = "friend@example.com".parse().expect("valid jid");
    let roomless = InviteToken::new("tok123");
    assert!(build_invite_share_message(to, None::<jid::Jid>, &roomless).is_none());
    assert_eq!(roomless.to_uri(), None);
}

#[test]
fn xep0488_legacy_message_payload_is_not_recognized() {
    let msg = Message::try_from(
        "<message xmlns='jabber:client' type='chat'>\
           <invite xmlns='urn:xmpp:muc-token-invite:0' token='legacy' jid='room@muc'/>\
         </message>"
            .parse::<Element>()
            .expect("valid xml"),
    )
    .expect("valid message");
    assert!(!has_invite_in_message(&msg));
    assert!(extract_invite_from_message(&msg).is_none());

    let mut stripped = msg.clone();
    strip_invite_from_message(&mut stripped);
    assert!(stripped.payloads.is_empty());
}

#[test]
fn xep0488_element_classifier_accepts_spec_payload_names_only() {
    for name in ["request", "token", "tokens", "revoke", "expired-token"] {
        assert!(is_invite_element(
            &Element::builder(name, NS_MUC_TOKEN_INVITE).build()
        ));
    }
    assert!(!is_invite_element(
        &Element::builder("invite", NS_MUC_TOKEN_INVITE).build()
    ));
    assert!(!is_invite_element(
        &Element::builder("token", "urn:xmpp:muc-token-invite:1").build()
    ));
}
