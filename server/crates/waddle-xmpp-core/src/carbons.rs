//! Message Carbons (XEP-0280) helpers shared by server and client crates.

use chrono::Utc;
use minidom::Element;
use tracing::debug;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0280 Message Carbons.
pub const CARBONS_NS: &str = "urn:xmpp:carbons:2";

/// Namespace for XEP-0297 Stanza Forwarding.
pub const FORWARDED_NS: &str = "urn:xmpp:forward:0";

/// Namespace for XEP-0203 Delayed Delivery.
pub const DELAY_NS: &str = "urn:xmpp:delay";

const HINTS_NS: &str = "urn:xmpp:hints";
const CHAT_STATES_NS: &str = "http://jabber.org/protocol/chatstates";
const RECEIPTS_NS: &str = "urn:xmpp:receipts";
const CHAT_MARKERS_NS: &str = "urn:xmpp:chat-markers:0";

fn is_instant_messaging_payload(payload: &Element) -> bool {
    matches!(
        (payload.ns().as_str(), payload.name()),
        (
            CHAT_STATES_NS,
            "active" | "composing" | "paused" | "inactive" | "gone"
        ) | (RECEIPTS_NS, "request" | "received")
            | (
                CHAT_MARKERS_NS,
                "markable" | "received" | "displayed" | "acknowledged"
            )
    )
}

/// Check if an IQ is a carbons enable request.
pub fn is_carbons_enable(iq: &Iq) -> bool {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Set(elem) => elem.name() == "enable" && elem.ns() == CARBONS_NS,
        _ => false,
    }
}

/// Check if an IQ is a carbons disable request.
pub fn is_carbons_disable(iq: &Iq) -> bool {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Set(elem) => elem.name() == "disable" && elem.ns() == CARBONS_NS,
        _ => false,
    }
}

/// Build a success response for carbons enable/disable.
pub fn build_carbons_result(original_iq: &Iq) -> Iq {
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: xmpp_parsers::iq::IqType::Result(None),
    }
}

/// Check whether a message is eligible for XEP-0280 carbon copying.
pub fn should_copy_message(msg: &Message) -> bool {
    use xmpp_parsers::message::MessageType;

    if msg
        .payloads
        .iter()
        .any(|payload| payload.name() == "no-copy" && payload.ns() == HINTS_NS)
    {
        debug!("Message has <no-copy/> hint, skipping carbon");
        return false;
    }

    if msg
        .payloads
        .iter()
        .any(|payload| payload.name() == "private" && payload.ns() == CARBONS_NS)
    {
        debug!("Message has <private/> element, skipping carbon");
        return false;
    }

    if matches!(msg.type_, MessageType::Groupchat | MessageType::Error) {
        debug!("Message type is not eligible for carbons");
        return false;
    }

    if matches!(msg.type_, MessageType::Chat) {
        return true;
    }

    if matches!(msg.type_, MessageType::Normal) && !msg.bodies.is_empty() {
        return true;
    }

    if msg.payloads.iter().any(is_instant_messaging_payload) {
        return true;
    }

    debug!("Message is not XEP-0280 eligible, skipping carbon");
    false
}

/// Build a "sent" carbon message wrapper.
pub fn build_sent_carbon(
    original_msg: &Message,
    from_jid: &str,
    to_jid: &str,
) -> Result<Message, jid::Error> {
    let forwarded = build_forwarded_element(original_msg);

    let sent = Element::builder("sent", CARBONS_NS)
        .append(forwarded)
        .build();

    let mut carbon_msg = Message::new(Some(to_jid.parse()?));
    carbon_msg.from = Some(from_jid.parse()?);
    carbon_msg.payloads.push(sent);

    Ok(carbon_msg)
}

/// Build a "received" carbon message wrapper.
pub fn build_received_carbon(
    original_msg: &Message,
    from_jid: &str,
    to_jid: &str,
) -> Result<Message, jid::Error> {
    let forwarded = build_forwarded_element(original_msg);

    let received = Element::builder("received", CARBONS_NS)
        .append(forwarded)
        .build();

    let mut carbon_msg = Message::new(Some(to_jid.parse()?));
    carbon_msg.from = Some(from_jid.parse()?);
    carbon_msg.payloads.push(received);

    Ok(carbon_msg)
}

fn build_forwarded_element(original_msg: &Message) -> Element {
    let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let delay = Element::builder("delay", DELAY_NS)
        .attr("stamp", timestamp)
        .build();
    let msg_element: Element = original_msg.clone().into();

    Element::builder("forwarded", FORWARDED_NS)
        .append(delay)
        .append(msg_element)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::MessageType;

    #[test]
    fn test_is_carbons_enable() {
        let enable_elem = Element::builder("enable", CARBONS_NS).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "enable-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(enable_elem),
        };

        assert!(is_carbons_enable(&iq));
    }

    #[test]
    fn test_is_not_carbons_enable_wrong_ns() {
        let enable_elem = Element::builder("enable", "wrong:ns").build();
        let iq = Iq {
            from: None,
            to: None,
            id: "enable-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(enable_elem),
        };

        assert!(!is_carbons_enable(&iq));
    }

    #[test]
    fn test_is_not_carbons_enable_get() {
        let enable_elem = Element::builder("enable", CARBONS_NS).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "enable-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(enable_elem),
        };

        assert!(!is_carbons_enable(&iq));
    }

    #[test]
    fn test_is_carbons_disable() {
        let disable_elem = Element::builder("disable", CARBONS_NS).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "disable-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(disable_elem),
        };

        assert!(is_carbons_disable(&iq));
    }

    #[test]
    fn test_should_copy_message_normal_chat() {
        let mut msg = Message::new(Some("recipient@example.com".parse().unwrap()));
        msg.type_ = MessageType::Chat;
        msg.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body("Hello".to_string()),
        );

        assert!(should_copy_message(&msg));
    }

    #[test]
    fn test_should_not_copy_groupchat() {
        let mut msg = Message::new(Some("room@muc.example.com".parse().unwrap()));
        msg.type_ = MessageType::Groupchat;
        msg.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body("Hello".to_string()),
        );

        assert!(!should_copy_message(&msg));
    }

    #[test]
    fn test_should_copy_bodyless_chat() {
        let mut msg = Message::new(Some("recipient@example.com".parse().unwrap()));
        msg.type_ = MessageType::Chat;
        assert!(should_copy_message(&msg));
    }

    #[test]
    fn test_should_not_copy_bodyless_normal_without_im_payload() {
        let mut msg = Message::new(Some("recipient@example.com".parse().unwrap()));
        msg.type_ = MessageType::Normal;
        assert!(!should_copy_message(&msg));
    }

    #[test]
    fn test_should_copy_bodyless_with_chat_state_payload() {
        let mut msg = Message::new(Some("recipient@example.com".parse().unwrap()));
        msg.type_ = MessageType::Normal;
        msg.payloads
            .push(Element::builder("composing", CHAT_STATES_NS).build());
        assert!(should_copy_message(&msg));
    }

    #[test]
    fn test_should_copy_bodyless_with_receipt_payload() {
        let mut msg = Message::new(Some("recipient@example.com".parse().unwrap()));
        msg.type_ = MessageType::Normal;
        msg.payloads
            .push(Element::builder("request", RECEIPTS_NS).build());
        assert!(should_copy_message(&msg));
    }

    #[test]
    fn test_should_copy_bodyless_with_chat_marker_payload() {
        let mut msg = Message::new(Some("recipient@example.com".parse().unwrap()));
        msg.type_ = MessageType::Normal;
        msg.payloads.push(
            Element::builder("displayed", CHAT_MARKERS_NS)
                .attr("id", "msg-1")
                .build(),
        );
        assert!(should_copy_message(&msg));
    }

    #[test]
    fn test_should_not_copy_with_private_element() {
        let mut msg = Message::new(Some("recipient@example.com".parse().unwrap()));
        msg.type_ = MessageType::Chat;
        msg.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body("Hello".to_string()),
        );
        msg.payloads
            .push(Element::builder("private", CARBONS_NS).build());

        assert!(!should_copy_message(&msg));
    }

    #[test]
    fn test_should_not_copy_with_no_copy_hint() {
        let mut msg = Message::new(Some("recipient@example.com".parse().unwrap()));
        msg.type_ = MessageType::Chat;
        msg.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body("Hello".to_string()),
        );
        msg.payloads
            .push(Element::builder("no-copy", HINTS_NS).build());

        assert!(!should_copy_message(&msg));
    }

    #[test]
    fn test_build_carbons_result() {
        let enable_elem = Element::builder("enable", CARBONS_NS).build();
        let iq = Iq {
            from: Some("user@example.com/resource".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "enable-1".to_string(),
            payload: xmpp_parsers::iq::IqType::Set(enable_elem),
        };

        let result = build_carbons_result(&iq);

        assert_eq!(result.id, "enable-1");
        assert!(matches!(
            result.payload,
            xmpp_parsers::iq::IqType::Result(None)
        ));
    }

    #[test]
    fn test_build_sent_carbon() {
        let mut original = Message::new(Some("recipient@example.com".parse().unwrap()));
        original.from = Some("sender@example.com/resource1".parse().unwrap());
        original.type_ = MessageType::Chat;
        original.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body("Hello".to_string()),
        );

        let carbon = build_sent_carbon(
            &original,
            "sender@example.com",
            "sender@example.com/resource2",
        )
        .expect("valid JIDs should succeed");

        assert_eq!(
            carbon.to.as_ref().unwrap().to_string(),
            "sender@example.com/resource2"
        );
        assert_eq!(
            carbon.from.as_ref().unwrap().to_string(),
            "sender@example.com"
        );

        let sent = carbon
            .payloads
            .iter()
            .find(|p| p.name() == "sent" && p.ns() == CARBONS_NS);
        assert!(sent.is_some());

        let forwarded = sent
            .unwrap()
            .children()
            .find(|c| c.name() == "forwarded" && c.ns() == FORWARDED_NS);
        assert!(forwarded.is_some());
    }

    #[test]
    fn test_build_received_carbon() {
        let mut original = Message::new(Some("user@example.com/resource1".parse().unwrap()));
        original.from = Some("sender@example.com".parse().unwrap());
        original.type_ = MessageType::Chat;
        original.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body("Hello".to_string()),
        );

        let carbon =
            build_received_carbon(&original, "user@example.com", "user@example.com/resource2")
                .expect("valid JIDs should succeed");

        assert_eq!(
            carbon.to.as_ref().unwrap().to_string(),
            "user@example.com/resource2"
        );

        let received = carbon
            .payloads
            .iter()
            .find(|p| p.name() == "received" && p.ns() == CARBONS_NS);
        assert!(received.is_some());
    }

    #[test]
    fn test_build_sent_carbon_invalid_jid() {
        let original = Message::new(Some("recipient@example.com".parse().unwrap()));

        let result = build_sent_carbon(&original, "@", "valid@example.com/res");
        assert!(result.is_err(), "Empty-local JID should return Err");

        let result = build_sent_carbon(&original, "valid@example.com", "@");
        assert!(result.is_err(), "Empty-local to_jid should return Err");
    }

    #[test]
    fn test_build_received_carbon_invalid_jid() {
        let original = Message::new(Some("recipient@example.com".parse().unwrap()));
        let result = build_received_carbon(&original, "@", "valid@example.com/res");
        assert!(result.is_err(), "Empty-local JID should return Err");

        let result = build_received_carbon(&original, "valid@example.com", "@");
        assert!(result.is_err(), "Empty-local to_jid should return Err");
    }
}
