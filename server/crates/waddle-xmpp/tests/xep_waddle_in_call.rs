//! `urn:waddle:in-call:0` — in-call signaling carrier. Carries both
//! message-transient signals (reactions) and presence-durable state
//! (raised hand).

use waddle_xmpp::xep::{
    build_in_call_presence_state_element, build_in_call_reaction_message,
    parse_in_call_presence_state, parse_in_call_signal, parse_in_call_signal_child, Hint,
    InCallParseError, InCallPresenceState, InCallReactionEmoji, InCallReactionSignal,
    InCallSessionId, InCallSignal, NS_WADDLE_IN_CALL,
};
use xmpp_parsers::message::MessageType;

#[test]
fn hand_raised_presence_state_round_trips() {
    let element = build_in_call_presence_state_element(&InCallPresenceState { hand_raised: true });

    // Carried as `<in-call xmlns='urn:waddle:in-call:0'>` so it sits next to
    // (never inside) the `<muji/>` element in MUC call presence.
    assert_eq!(element.name(), "in-call");
    assert_eq!(element.ns(), NS_WADDLE_IN_CALL);
    assert!(
        element
            .children()
            .any(|child| child.name() == "hand-raised" && child.ns() == NS_WADDLE_IN_CALL),
        "raised hand serializes a <hand-raised/> child"
    );

    assert_eq!(
        parse_in_call_presence_state(&element),
        Ok(InCallPresenceState { hand_raised: true })
    );
}

#[test]
fn in_call_reaction_message_round_trips_with_transient_hints() {
    let to = "bob@example.test/phone".parse().expect("peer full jid");
    let from = "alice@example.test/laptop"
        .parse()
        .expect("sender full jid");
    let sid = InCallSessionId::new("call-123").expect("valid sid");
    let emoji = InCallReactionEmoji::new("👍").expect("valid emoji");
    let message = build_in_call_reaction_message(to, from, &sid, &emoji, MessageType::Chat);

    assert_eq!(message.type_, MessageType::Chat);
    assert!(message.bodies.is_empty());

    let carrier = message
        .payloads
        .iter()
        .find(|payload| payload.name() == "in-call" && payload.ns() == NS_WADDLE_IN_CALL)
        .expect("in-call payload");
    assert_eq!(carrier.attr("sid"), Some("call-123"));

    let reaction = carrier
        .children()
        .find(|child| child.name() == "reaction" && child.ns() == NS_WADDLE_IN_CALL)
        .expect("reaction child");
    assert_eq!(reaction.attr("emoji"), Some("👍"));

    assert!(waddle_xmpp::xep::has_hint(&message, Hint::NoStore));
    assert!(waddle_xmpp::xep::has_hint(&message, Hint::NoCopy));

    assert_eq!(
        parse_in_call_signal_child(&message),
        Some(InCallSignal::Reaction(InCallReactionSignal { sid, emoji }))
    );
}

#[test]
fn lowered_hand_is_empty_state_with_no_marker_child() {
    let lowered = InCallPresenceState { hand_raised: false };
    assert!(lowered.is_empty());
    assert!(!InCallPresenceState { hand_raised: true }.is_empty());

    let element = build_in_call_presence_state_element(&lowered);
    assert_eq!(element.name(), "in-call");
    assert_eq!(
        element.children().count(),
        0,
        "a lowered hand carries no marker child"
    );
    assert_eq!(
        parse_in_call_presence_state(&element),
        Ok(InCallPresenceState { hand_raised: false })
    );
}

#[test]
fn parse_in_call_presence_state_rejects_foreign_element() {
    let muji: minidom::Element = "<muji xmlns='urn:xmpp:jingle:muji:0'><preparing/></muji>"
        .parse()
        .expect("muji xml");
    assert_eq!(
        parse_in_call_presence_state(&muji),
        Err(InCallParseError::NotInCall)
    );
}

#[test]
fn in_call_reaction_rejects_empty_sid() {
    let carrier: minidom::Element = "<in-call xmlns='urn:waddle:in-call:0' sid=''>\
       <reaction emoji='👍'/>\
       </in-call>"
        .parse()
        .expect("carrier xml");

    assert_eq!(
        parse_in_call_signal(&carrier),
        Err(InCallParseError::EmptySessionId)
    );
}

#[test]
fn in_call_reaction_values_are_trimmed_before_serialization() {
    let sid = InCallSessionId::new(" call-123 ").expect("valid sid");
    let emoji = InCallReactionEmoji::new(" 👍 ").expect("valid emoji");
    assert_eq!(sid.as_str(), "call-123");
    assert_eq!(emoji.as_str(), "👍");
}

#[test]
fn in_call_reaction_reports_missing_reaction_child() {
    let carrier: minidom::Element = "<in-call xmlns='urn:waddle:in-call:0' sid='call-123'/>"
        .parse()
        .expect("carrier xml");

    assert_eq!(
        parse_in_call_signal(&carrier),
        Err(InCallParseError::MissingChild("reaction"))
    );
}
