//! Typed builders for stanza-error replies emitted by the MUC handler chain.
//!
//! Mirrors [`super::super::handlers::errors`], the 1:1 chain's typed-error
//! chokepoint, but for room-locality concerns. Co-locating XEP-0045
//! §7.4 / §7.5 / managed-room error construction here keeps each named
//! constructor traceable to its XEP citation and forbids inline
//! `format!`/concat XML construction (per the XML hard rule) by giving
//! handlers a single typed entry-point.
//!
//! Every builder returns a typed [`Message`] addressed *from the room
//! JID* back to the offending sender's full JID — handlers that emit
//! these wrap the result in
//! [`super::super::handlers::errors::send_message_error`].
//!
//! Per the typed-payloads hard rule, the public API takes typed
//! [`BareJid`] / [`FullJid`] and a borrowed [`Message`]; no `String` /
//! `&str` payload fields appear on the boundary.

use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

/// Build the XEP-0045 §7.4 typed `<not-acceptable type='cancel'/>` reply
/// for a non-occupant attempting to send a message to `room`.
///
/// Per [XEP-0045 §7.4]:
///
/// > Only occupants are allowed to send messages to the room. If a
/// > non-occupant sends a message to the room, the service MUST refuse
/// > to deliver the message and return a `<not-acceptable/>` error to
/// > the sender.
///
/// The reply is addressed *from* the room's bare JID *to* the sender's
/// full JID so the client can attribute the rejection.
///
/// [XEP-0045 §7.4]: https://xmpp.org/extensions/xep-0045.html#message
pub fn xep_0045_not_acceptable_reply(
    incoming: &Message,
    room: &BareJid,
    sender: &FullJid,
) -> Message {
    build_room_error_reply(
        incoming,
        room,
        sender,
        StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::NotAcceptable,
            "en",
            "Only room occupants may send messages to this room.",
        ),
    )
}

/// Build the XEP-0045 §7.5 typed `<forbidden type='auth'/>` reply for a
/// visitor attempting to send a message to a moderated `room`.
///
/// Per XEP-0045 §7.5: visitors (role='visitor') cannot send messages in
/// a moderated room; the service MUST return a `<forbidden/>` error.
///
/// The reply is addressed *from* the room's bare JID *to* the sender's
/// full JID.
pub fn xep_0045_visitor_forbidden_reply(
    incoming: &Message,
    room: &BareJid,
    sender: &FullJid,
) -> Message {
    build_room_error_reply(
        incoming,
        room,
        sender,
        StanzaError::new(
            ErrorType::Auth,
            DefinedCondition::Forbidden,
            "en",
            "Visitors may not send messages to this moderated room.",
        ),
    )
}

/// Build the Waddle managed-room `<forbidden type='auth'/>` reply for a
/// non-owner attempting to post to a managed room (e.g. the
/// `announcements` room).
///
/// This is a Waddle-specific authorization gate layered on top of the
/// standard XEP-0045 occupancy semantics; the typed wire shape is the
/// same `<error type='auth'><forbidden/></error>` clients already
/// understand from §7.5.
///
/// The reply is addressed *from* the room's bare JID *to* the sender's
/// full JID.
pub fn managed_room_forbidden_reply(
    incoming: &Message,
    room: &BareJid,
    sender: &FullJid,
) -> Message {
    build_room_error_reply(
        incoming,
        room,
        sender,
        StanzaError::new(
            ErrorType::Auth,
            DefinedCondition::Forbidden,
            "en",
            "Sender is not permitted to address this resource.",
        ),
    )
}

/// Internal: clone `incoming`, retag as `type='error'`, override
/// addressing so the reply originates from `room` and targets the
/// `sender` full JID, and attach the typed [`StanzaError`] payload.
///
/// MUC error replies always come "from the room" so clients can route
/// the rejection back into the room context — `message_error_reply`
/// from the 1:1 chain swaps from/to verbatim, which would put the
/// original `to=room` value into `from`, but it does not carry the
/// `sender`'s full JID through to `to`. This helper centralizes the
/// MUC-specific addressing so the three named constructors above stay
/// declarative.
fn build_room_error_reply(
    incoming: &Message,
    room: &BareJid,
    sender: &FullJid,
    error: StanzaError,
) -> Message {
    let mut reply = incoming.clone();
    reply.type_ = MessageType::Error;
    reply.from = Some(Jid::from(room.clone()));
    reply.to = Some(Jid::from(sender.clone()));
    reply.payloads.push(Element::from(error));
    reply
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::{Body, Message, MessageType};

    fn bare(s: &str) -> BareJid {
        s.parse().expect("valid bare jid")
    }
    fn full(s: &str) -> FullJid {
        s.parse().expect("valid full jid")
    }

    fn groupchat_to(room: &BareJid, sender: &FullJid, body: &str) -> Message {
        let mut m = Message::new(Some(Jid::from(room.clone())));
        m.from = Some(Jid::from(sender.clone()));
        m.type_ = MessageType::Groupchat;
        m.bodies.insert(String::new(), Body(body.to_string()));
        m
    }

    fn extract_typed_error(reply: &Message) -> StanzaError {
        let elem = reply
            .payloads
            .iter()
            .find(|p| p.name() == "error")
            .expect("error payload present");
        StanzaError::try_from(elem.clone()).expect("typed StanzaError parses from element")
    }

    #[test]
    fn xep_0045_not_acceptable_reply_matches_section_7_4_wire_shape() {
        // XEP-0045 §7.4: non-occupant → <not-acceptable type='cancel'/>
        // addressed from the room JID back to the sender.
        let room = bare("room@conf.example.com");
        let sender = full("alice@example.com/web");
        let incoming = groupchat_to(&room, &sender, "hi");

        let reply = xep_0045_not_acceptable_reply(&incoming, &room, &sender);

        assert_eq!(reply.type_, MessageType::Error);
        assert_eq!(
            reply.from.as_ref().map(|j| j.to_string()),
            Some(room.to_string()),
            "reply.from must be the room JID per XEP-0045 §7.4 attribution",
        );
        assert_eq!(
            reply.to.as_ref().map(|j| j.to_string()),
            Some(sender.to_string()),
            "reply.to must be the sender's full JID",
        );
        let parsed = extract_typed_error(&reply);
        assert_eq!(parsed.type_, ErrorType::Cancel);
        assert_eq!(parsed.defined_condition, DefinedCondition::NotAcceptable);
    }

    #[test]
    fn xep_0045_visitor_forbidden_reply_matches_section_7_5_wire_shape() {
        // XEP-0045 §7.5: visitor in moderated room → <forbidden type='auth'/>.
        let room = bare("moderated@conf.example.com");
        let sender = full("alice@example.com/web");
        let incoming = groupchat_to(&room, &sender, "hi (as visitor)");

        let reply = xep_0045_visitor_forbidden_reply(&incoming, &room, &sender);

        assert_eq!(reply.type_, MessageType::Error);
        assert_eq!(
            reply.from.as_ref().map(|j| j.to_string()),
            Some(room.to_string()),
        );
        assert_eq!(
            reply.to.as_ref().map(|j| j.to_string()),
            Some(sender.to_string()),
        );
        let parsed = extract_typed_error(&reply);
        assert_eq!(parsed.type_, ErrorType::Auth);
        assert_eq!(parsed.defined_condition, DefinedCondition::Forbidden);
    }

    #[test]
    fn managed_room_forbidden_reply_matches_forbidden_auth_wire_shape() {
        // Waddle managed-room policy: non-owner → <forbidden type='auth'/>.
        let room = bare("announcements@conf.example.com");
        let sender = full("alice@example.com/web");
        let incoming = groupchat_to(&room, &sender, "important announcement");

        let reply = managed_room_forbidden_reply(&incoming, &room, &sender);

        assert_eq!(reply.type_, MessageType::Error);
        assert_eq!(
            reply.from.as_ref().map(|j| j.to_string()),
            Some(room.to_string()),
        );
        assert_eq!(
            reply.to.as_ref().map(|j| j.to_string()),
            Some(sender.to_string()),
        );
        let parsed = extract_typed_error(&reply);
        assert_eq!(parsed.type_, ErrorType::Auth);
        assert_eq!(parsed.defined_condition, DefinedCondition::Forbidden);
    }

    #[test]
    fn room_error_replies_preserve_original_payloads() {
        // Cloning `incoming` is the contract: the reply must carry the
        // original payloads (e.g. stanza-id, body) so clients can
        // correlate the rejection with the offending stanza.
        let room = bare("room@conf.example.com");
        let sender = full("alice@example.com/web");
        let mut incoming = groupchat_to(&room, &sender, "correlation body");
        // Drop a sentinel payload so we can assert it survives the clone.
        let sentinel = Element::builder("sentinel", "urn:waddle:test").build();
        incoming.payloads.push(sentinel);

        let reply = xep_0045_not_acceptable_reply(&incoming, &room, &sender);

        assert!(
            reply
                .payloads
                .iter()
                .any(|p| p.name() == "sentinel" && p.ns() == "urn:waddle:test"),
            "original payloads must survive into the error reply",
        );
        assert_eq!(
            reply.bodies.get("").map(|b| b.0.as_str()),
            Some("correlation body"),
            "original body must survive into the error reply",
        );
    }
}
