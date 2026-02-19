//! XEP-0461: Message Replies
//!
//! Provides helpers for parsing and building reply references:
//! `<reply xmlns='urn:xmpp:reply:0' id='message-id' to='sender@domain'/>`
//!
//! Thread identifiers continue to use RFC 6121 `<thread/>`.

use minidom::Element;
use xmpp_parsers::message::{Message, Thread};

/// Namespace for XEP-0461 Message Replies.
pub const NS_REPLY: &str = "urn:xmpp:reply:0";

/// A reply reference attached to a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyReference {
    /// The ID of the message being replied to.
    pub id: String,
    /// Optional JID of the original message sender.
    pub to: Option<String>,
}

impl ReplyReference {
    /// Create a new reply reference with a target message ID.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            to: None,
        }
    }

    /// Attach the optional original sender JID.
    pub fn with_to(mut self, to: impl Into<String>) -> Self {
        self.to = Some(to.into());
        self
    }
}

/// Check whether an element is an XEP-0461 `<reply/>` payload.
pub fn is_reply_element(elem: &Element) -> bool {
    elem.name() == "reply" && elem.ns() == NS_REPLY
}

/// Parse an XEP-0461 reply payload from a message.
///
/// Accepts both:
/// - `<reply id='message-id' to='sender@domain'/>` (preferred)
/// - `<reply to='message-id'/>` (legacy compatibility)
pub fn parse_reply_from_message(msg: &Message) -> Option<ReplyReference> {
    let reply_elem = msg.payloads.iter().find(|elem| is_reply_element(elem))?;

    let id_attr = reply_elem
        .attr("id")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    let to_attr = reply_elem
        .attr("to")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    // Backward compatibility with older examples that used @to as the message id.
    match id_attr {
        Some(id) => Some(ReplyReference { id, to: to_attr }),
        None => to_attr.map(ReplyReference::new),
    }
}

/// Build an XEP-0461 `<reply/>` payload element.
pub fn build_reply_element(reply: &ReplyReference) -> Element {
    let mut builder = Element::builder("reply", NS_REPLY).attr("id", reply.id.as_str());
    if let Some(to) = reply.to.as_deref() {
        builder = builder.attr("to", to);
    }
    builder.build()
}

/// Replace any existing XEP-0461 reply payload on a message.
pub fn set_reply_payload(msg: &mut Message, reply: &ReplyReference) {
    msg.payloads.retain(|elem| !is_reply_element(elem));
    msg.payloads.push(build_reply_element(reply));
}

/// Read RFC 6121 `<thread/>` identifier from a message.
pub fn thread_id_from_message(msg: &Message) -> Option<String> {
    msg.thread.as_ref().map(|thread| thread.0.clone())
}

/// Set RFC 6121 `<thread/>` identifier on a message.
pub fn set_thread_id(msg: &mut Message, thread_id: impl Into<String>) {
    msg.thread = Some(Thread(thread_id.into()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::Message;

    #[test]
    fn test_parse_reply_with_id_and_to() {
        let xml = "<message xmlns='jabber:client'><reply xmlns='urn:xmpp:reply:0' id='parent-1' to='alice@example.com'/></message>";
        let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("message");
        let reply = parse_reply_from_message(&msg).expect("reply present");
        assert_eq!(reply.id, "parent-1");
        assert_eq!(reply.to.as_deref(), Some("alice@example.com"));
    }

    #[test]
    fn test_parse_reply_legacy_to_as_id() {
        let xml =
            "<message xmlns='jabber:client'><reply xmlns='urn:xmpp:reply:0' to='legacy-id'/></message>";
        let msg = Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("message");
        let reply = parse_reply_from_message(&msg).expect("reply present");
        assert_eq!(reply.id, "legacy-id");
        assert_eq!(reply.to, None);
    }

    #[test]
    fn test_set_reply_payload_replaces_existing() {
        let xml =
            "<message xmlns='jabber:client'><reply xmlns='urn:xmpp:reply:0' id='old'/></message>";
        let mut msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("message");

        set_reply_payload(
            &mut msg,
            &ReplyReference::new("new-id").with_to("bob@example.com"),
        );

        let reply = parse_reply_from_message(&msg).expect("reply present");
        assert_eq!(reply.id, "new-id");
        assert_eq!(reply.to.as_deref(), Some("bob@example.com"));
        assert_eq!(
            msg.payloads
                .iter()
                .filter(|elem| is_reply_element(elem))
                .count(),
            1
        );
    }

    #[test]
    fn test_thread_helpers() {
        let mut msg = Message::new(None::<jid::Jid>);
        assert_eq!(thread_id_from_message(&msg), None);

        set_thread_id(&mut msg, "thread-root-1");
        assert_eq!(
            thread_id_from_message(&msg).as_deref(),
            Some("thread-root-1")
        );
    }
}
