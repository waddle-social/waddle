//! XEP-0085: Chat State Notifications dedicated suite.

use jid::Jid;
use waddle_xmpp::disco::{server_features, Feature};
use waddle_xmpp::xep::{classify_message_urgency, StanzaUrgency};
use xmpp_parsers::message::{Body, Message};

const CHAT_STATES_NS: &str = "http://jabber.org/protocol/chatstates";

#[test]
fn xep0085_bodyless_chat_state_message_is_non_urgent() {
    let to: Jid = "peer@localhost".parse().expect("valid jid");
    let msg = Message::new(Some(to));

    assert_eq!(classify_message_urgency(&msg), StanzaUrgency::NonUrgent);
}

#[test]
fn xep0085_message_with_body_is_urgent() {
    let to: Jid = "peer@localhost".parse().expect("valid jid");
    let mut msg = Message::new(Some(to));
    msg.bodies
        .insert(String::new(), Body("real message".to_string()));

    assert_eq!(classify_message_urgency(&msg), StanzaUrgency::Urgent);
}

#[test]
fn xep0085_advertisement_consistency_no_false_feature_claim() {
    let features = server_features();
    assert!(!features.contains(&Feature::new(CHAT_STATES_NS)));
}
