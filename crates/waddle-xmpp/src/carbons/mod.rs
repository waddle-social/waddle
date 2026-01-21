//! Message Carbons (XEP-0280) implementation.
//!
//! Message Carbons automatically synchronizes messages across all of a user's
//! connected clients. When a user sends or receives a message on one client,
//! copies are delivered to all other connected clients.
//!
//! ## Protocol Overview
//!
//! Clients enable carbons by sending:
//! ```xml
//! <iq type='set' id='enable-1'>
//!   <enable xmlns='urn:xmpp:carbons:2'/>
//! </iq>
//! ```
//!
//! When enabled:
//! - Messages sent by the user from any client are forwarded to all other clients
//!   as "sent" carbons wrapped in `<forwarded>`
//! - Messages received by the user are forwarded to all other clients as
//!   "received" carbons wrapped in `<forwarded>`
//!
//! ## Features
//!
//! - `urn:xmpp:carbons:2` - Message Carbons namespace
//! - Enable/disable carbons per-connection
//! - Sent carbons for outgoing messages
//! - Received carbons for incoming messages
//! - Respects `<no-copy/>` and `<private/>` elements to prevent carbon copying

use chrono::Utc;
use minidom::Element;
use tracing::debug;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::Message;

use crate::XmppError;

/// Namespace for XEP-0280 Message Carbons.
pub const CARBONS_NS: &str = "urn:xmpp:carbons:2";

/// Namespace for XEP-0297 Stanza Forwarding.
pub const FORWARDED_NS: &str = "urn:xmpp:forward:0";

/// Namespace for XEP-0203 Delayed Delivery.
pub const DELAY_NS: &str = "urn:xmpp:delay";

/// Check if an IQ is a carbons enable request.
///
/// Returns true if the IQ contains:
/// ```xml
/// <iq type='set'>
///   <enable xmlns='urn:xmpp:carbons:2'/>
/// </iq>
/// ```
pub fn is_carbons_enable(iq: &Iq) -> bool {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Set(elem) => {
            elem.name() == "enable" && elem.ns() == CARBONS_NS
        }
        _ => false,
    }
}

/// Check if an IQ is a carbons disable request.
///
/// Returns true if the IQ contains:
/// ```xml
/// <iq type='set'>
///   <disable xmlns='urn:xmpp:carbons:2'/>
/// </iq>
/// ```
pub fn is_carbons_disable(iq: &Iq) -> bool {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Set(elem) => {
            elem.name() == "disable" && elem.ns() == CARBONS_NS
        }
        _ => false,
    }
}

/// Build a success response for carbons enable/disable.
///
/// Returns an empty result IQ per XEP-0280.
pub fn build_carbons_result(original_iq: &Iq) -> Iq {
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: xmpp_parsers::iq::IqType::Result(None),
    }
}

/// Check if a message should be excluded from carbon copying.
///
/// Messages are excluded if they contain:
/// - `<no-copy xmlns='urn:xmpp:hints'/>` - Explicit no-copy hint
/// - `<private xmlns='urn:xmpp:carbons:2'/>` - Private message
/// - Groupchat type (MUC messages have their own delivery)
/// - Error type messages
/// - No body (e.g., chat states only)
pub fn should_copy_message(msg: &Message) -> bool {
    use xmpp_parsers::message::MessageType;

    // Don't copy groupchat or error messages
    match msg.type_ {
        MessageType::Groupchat | MessageType::Error => return false,
        _ => {}
    }

    // Check for <no-copy/> hint (urn:xmpp:hints)
    for payload in &msg.payloads {
        if payload.name() == "no-copy" && payload.ns() == "urn:xmpp:hints" {
            debug!("Message has <no-copy/> hint, skipping carbon");
            return false;
        }
    }

    // Check for <private/> element (carbons namespace)
    for payload in &msg.payloads {
        if payload.name() == "private" && payload.ns() == CARBONS_NS {
            debug!("Message has <private/> element, skipping carbon");
            return false;
        }
    }

    // Only copy messages with a body
    // This excludes chat states and other non-content messages
    if msg.bodies.is_empty() {
        debug!("Message has no body, skipping carbon");
        return false;
    }

    true
}

/// Build a "sent" carbon message wrapper.
///
/// When a user sends a message from one client, other clients receive:
/// ```xml
/// <message from='user@domain' to='user@domain/otherresource'>
///   <sent xmlns='urn:xmpp:carbons:2'>
///     <forwarded xmlns='urn:xmpp:forward:0'>
///       <delay xmlns='urn:xmpp:delay' stamp='...'/>
///       <message ...>original message</message>
///     </forwarded>
///   </sent>
/// </message>
/// ```
pub fn build_sent_carbon(
    original_msg: &Message,
    from_jid: &str,
    to_jid: &str,
) -> Message {
    let forwarded = build_forwarded_element(original_msg);

    let sent = Element::builder("sent", CARBONS_NS)
        .append(forwarded)
        .build();

    let mut carbon_msg = Message::new(Some(to_jid.parse().unwrap()));
    carbon_msg.from = Some(from_jid.parse().unwrap());
    carbon_msg.payloads.push(sent);

    carbon_msg
}

/// Build a "received" carbon message wrapper.
///
/// When a user receives a message, other clients receive:
/// ```xml
/// <message from='user@domain' to='user@domain/otherresource'>
///   <received xmlns='urn:xmpp:carbons:2'>
///     <forwarded xmlns='urn:xmpp:forward:0'>
///       <delay xmlns='urn:xmpp:delay' stamp='...'/>
///       <message ...>original message</message>
///     </forwarded>
///   </received>
/// </message>
/// ```
pub fn build_received_carbon(
    original_msg: &Message,
    from_jid: &str,
    to_jid: &str,
) -> Message {
    let forwarded = build_forwarded_element(original_msg);

    let received = Element::builder("received", CARBONS_NS)
        .append(forwarded)
        .build();

    let mut carbon_msg = Message::new(Some(to_jid.parse().unwrap()));
    carbon_msg.from = Some(from_jid.parse().unwrap());
    carbon_msg.payloads.push(received);

    carbon_msg
}

/// Build a <forwarded/> element containing the original message.
///
/// Per XEP-0297 (Stanza Forwarding), the forwarded element includes:
/// - Optional <delay/> element with timestamp
/// - The complete original stanza
fn build_forwarded_element(original_msg: &Message) -> Element {
    // Create delay element with current timestamp
    let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let delay = Element::builder("delay", DELAY_NS)
        .attr("stamp", timestamp)
        .build();

    // Convert the message to an Element for inclusion
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
        msg.bodies.insert(String::new(), xmpp_parsers::message::Body("Hello".to_string()));

        assert!(should_copy_message(&msg));
    }

    #[test]
    fn test_should_not_copy_groupchat() {
        let mut msg = Message::new(Some("room@muc.example.com".parse().unwrap()));
        msg.type_ = MessageType::Groupchat;
        msg.bodies.insert(String::new(), xmpp_parsers::message::Body("Hello".to_string()));

        assert!(!should_copy_message(&msg));
    }

    #[test]
    fn test_should_not_copy_no_body() {
        let msg = Message::new(Some("recipient@example.com".parse().unwrap()));
        // No body - e.g., chat state notification

        assert!(!should_copy_message(&msg));
    }

    #[test]
    fn test_should_not_copy_with_private_element() {
        let mut msg = Message::new(Some("recipient@example.com".parse().unwrap()));
        msg.type_ = MessageType::Chat;
        msg.bodies.insert(String::new(), xmpp_parsers::message::Body("Hello".to_string()));

        // Add <private/> element
        let private = Element::builder("private", CARBONS_NS).build();
        msg.payloads.push(private);

        assert!(!should_copy_message(&msg));
    }

    #[test]
    fn test_should_not_copy_with_no_copy_hint() {
        let mut msg = Message::new(Some("recipient@example.com".parse().unwrap()));
        msg.type_ = MessageType::Chat;
        msg.bodies.insert(String::new(), xmpp_parsers::message::Body("Hello".to_string()));

        // Add <no-copy/> hint
        let no_copy = Element::builder("no-copy", "urn:xmpp:hints").build();
        msg.payloads.push(no_copy);

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
        assert!(matches!(result.payload, xmpp_parsers::iq::IqType::Result(None)));
    }

    #[test]
    fn test_build_sent_carbon() {
        let mut original = Message::new(Some("recipient@example.com".parse().unwrap()));
        original.from = Some("sender@example.com/resource1".parse().unwrap());
        original.type_ = MessageType::Chat;
        original.bodies.insert(String::new(), xmpp_parsers::message::Body("Hello".to_string()));

        let carbon = build_sent_carbon(
            &original,
            "sender@example.com",
            "sender@example.com/resource2",
        );

        assert_eq!(
            carbon.to.as_ref().unwrap().to_string(),
            "sender@example.com/resource2"
        );
        assert_eq!(
            carbon.from.as_ref().unwrap().to_string(),
            "sender@example.com"
        );

        // Check for sent element
        let sent = carbon.payloads.iter().find(|p| p.name() == "sent" && p.ns() == CARBONS_NS);
        assert!(sent.is_some());

        // Check for forwarded element
        let forwarded = sent.unwrap().children().find(|c| c.name() == "forwarded" && c.ns() == FORWARDED_NS);
        assert!(forwarded.is_some());
    }

    #[test]
    fn test_build_received_carbon() {
        let mut original = Message::new(Some("user@example.com/resource1".parse().unwrap()));
        original.from = Some("sender@example.com".parse().unwrap());
        original.type_ = MessageType::Chat;
        original.bodies.insert(String::new(), xmpp_parsers::message::Body("Hello".to_string()));

        let carbon = build_received_carbon(
            &original,
            "user@example.com",
            "user@example.com/resource2",
        );

        assert_eq!(
            carbon.to.as_ref().unwrap().to_string(),
            "user@example.com/resource2"
        );

        // Check for received element
        let received = carbon.payloads.iter().find(|p| p.name() == "received" && p.ns() == CARBONS_NS);
        assert!(received.is_some());
    }
}
