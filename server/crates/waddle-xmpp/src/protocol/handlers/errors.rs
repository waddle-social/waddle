//! Typed builders for stanza-error replies emitted by message handlers.
//!
//! Per Q6(d) of the #229 design, handlers that need to emit a typed
//! error reply use [`xmpp_parsers::stanza_error::StanzaError`] and
//! `OutboundEvent::SendStanza` directly — no separate `ReplyWithError`
//! event variant. The helpers here keep the per-XEP error mappings
//! (XEP-0086 → defined-condition; XEP-0191 → `<blocked/>` application
//! condition; XEP-0424 → `<item-not-found>`) co-located with the
//! handlers so a reviewer can trace each XEP's error response back to a
//! single typed builder.

use crate::Stanza;
use minidom::Element;
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

/// Application-condition namespace for XEP-0191 blocking errors.
pub const NS_BLOCKING_ERRORS: &str = "urn:xmpp:blocking:errors";

/// Build a typed message-error reply by cloning `incoming`, swapping
/// `from` / `to`, setting `type='error'`, and attaching the
/// [`StanzaError`].
///
/// This is the single chokepoint for constructing message-error replies
/// from a [`super::super::traits::MessageHandler`]; per the typed-payloads
/// hard rule the result is a typed [`Message`] all the way to the
/// transport boundary.
pub fn message_error_reply(incoming: &Message, error: StanzaError) -> Message {
    let mut reply = incoming.clone();
    reply.type_ = MessageType::Error;
    // Swap addresses: the error flows back to the sender of the original.
    let from = incoming.to.clone();
    let to = incoming.from.clone();
    reply.from = from;
    reply.to = to;
    reply.payloads.push(Element::from(error));
    reply
}

/// Convenience wrapper: build a typed `not-acceptable type='cancel'`
/// reply for `incoming`, with the XEP-0191 `<blocked/>` application
/// condition attached.
///
/// XEP-0191 §3.2 mandates this exact shape when a user attempts to send
/// a stanza to a JID on their own blocklist:
///
/// ```xml
/// <error type='cancel'>
///   <not-acceptable xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>
///   <blocked xmlns='urn:xmpp:blocking:errors'/>
/// </error>
/// ```
pub fn outgoing_block_error_reply(incoming: &Message) -> Message {
    let mut error = StanzaError::new(
        ErrorType::Cancel,
        DefinedCondition::NotAcceptable,
        "en",
        "Recipient is on your blocklist.",
    );
    error.other = Some(Element::builder("blocked", NS_BLOCKING_ERRORS).build());
    message_error_reply(incoming, error)
}

/// Wrap a typed message-error reply in `OutboundEvent::SendStanza` —
/// the convention for handlers that emit error replies.
pub fn send_message_error(reply: Message) -> super::super::event::OutboundEvent {
    super::super::event::OutboundEvent::SendStanza(Box::new(Stanza::Message(reply)))
}

/// Build an `<item-not-found type='cancel'/>` reply for a rich-target
/// reference (XEP-0308 / 0424 / 0461) whose target message is not in
/// the archive.
pub fn item_not_found_reply(incoming: &Message, text: &str) -> Message {
    let error = StanzaError::new(
        ErrorType::Cancel,
        DefinedCondition::ItemNotFound,
        "en",
        text,
    );
    message_error_reply(incoming, error)
}

/// Build a `<not-acceptable type='cancel'/>` reply for a rich-target
/// where the requesting user is not the original author (XEP-0308 §7.1
/// correction; XEP-0424 §3.4 retraction).
pub fn not_acceptable_reply(incoming: &Message, text: &str) -> Message {
    let error = StanzaError::new(
        ErrorType::Cancel,
        DefinedCondition::NotAcceptable,
        "en",
        text,
    );
    message_error_reply(incoming, error)
}

/// Build a `<bad-request type='modify'/>` reply for a malformed or
/// inapplicable rich-target request (e.g. retraction of an
/// already-tombstoned message — XEP-0424 §3.5).
pub fn bad_request_reply(incoming: &Message, text: &str) -> Message {
    let error = StanzaError::new(ErrorType::Modify, DefinedCondition::BadRequest, "en", text);
    message_error_reply(incoming, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::Message;

    fn chat_msg(from: &str, to: &str) -> Message {
        let mut m = Message::new(Some(to.parse().expect("jid")));
        m.from = Some(from.parse().expect("jid"));
        m.type_ = MessageType::Chat;
        m
    }

    #[test]
    fn message_error_reply_swaps_from_and_to() {
        let incoming = chat_msg("alice@example.com/web", "bob@example.com");
        let error = StanzaError::new(ErrorType::Cancel, DefinedCondition::NotAcceptable, "en", "");
        let reply = message_error_reply(&incoming, error);
        assert_eq!(reply.type_, MessageType::Error);
        assert_eq!(
            reply.from.as_ref().map(|j| j.to_string()),
            Some("bob@example.com".to_string())
        );
        assert_eq!(
            reply.to.as_ref().map(|j| j.to_string()),
            Some("alice@example.com/web".to_string())
        );
    }

    #[test]
    fn outgoing_block_error_carries_typed_blocked_application_condition() {
        let incoming = chat_msg("alice@example.com/web", "blocked@example.com");
        let reply = outgoing_block_error_reply(&incoming);
        assert_eq!(reply.type_, MessageType::Error);
        // Parse out the typed StanzaError from the payloads to assert on
        // its shape rather than text-matching XML.
        let error_elem = reply
            .payloads
            .iter()
            .find(|p| p.name() == "error")
            .expect("error payload present");
        let parsed = StanzaError::try_from(error_elem.clone()).expect("typed StanzaError parse");
        assert_eq!(parsed.type_, ErrorType::Cancel);
        assert_eq!(parsed.defined_condition, DefinedCondition::NotAcceptable);
        let blocked = parsed.other.expect("application condition present");
        assert_eq!(blocked.name(), "blocked");
        assert_eq!(blocked.ns(), NS_BLOCKING_ERRORS);
    }
}
