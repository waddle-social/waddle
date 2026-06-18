//! `urn:waddle:in-call:0` — transient in-call signaling carrier.

use waddle_xmpp::xep::{
    build_in_call_reaction_message, parse_in_call_signal, parse_in_call_signal_child, Hint,
    InCallParseError, InCallReactionEmoji, InCallReactionSignal, InCallSessionId, InCallSignal,
    NS_WADDLE_IN_CALL,
};
use xmpp_parsers::message::MessageType;

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
