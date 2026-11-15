//! Stanzas the router answers with.

use jid::{BareJid, DomainRef, Jid};
use minidom::Element;
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::presence::{Presence, Type as PresenceType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

/// A stanza error this server generated, attributed to `home` (RFC 6120
/// §8.3.2).
pub(super) fn error(
    home: &DomainRef,
    type_: ErrorType,
    condition: DefinedCondition,
    text: &str,
) -> StanzaError {
    let mut error = StanzaError::new(type_, condition, "en", text);
    error.by = Some(Jid::from(BareJid::from_parts(None, home)));
    error
}

/// RFC 6121 §8.5.1 and ADR-0020 D10: refused as if no such entity existed.
pub(super) fn service_unavailable(home: &DomainRef) -> StanzaError {
    error(
        home,
        ErrorType::Cancel,
        DefinedCondition::ServiceUnavailable,
        "The service is not available to remote senders.",
    )
}

/// RFC 6120 §8.3.3.8: an address in a spelling the identity profile
/// refuses.
pub(super) fn jid_malformed(home: &DomainRef) -> StanzaError {
    error(
        home,
        ErrorType::Modify,
        DefinedCondition::JidMalformed,
        "An address is written in a form this server does not accept.",
    )
}

/// The error a local requester receives instead of a remote entity's
/// response that this server refused to process because an address it
/// carries is outside the identity profile.
pub(super) fn unusable_response(home: &DomainRef) -> StanzaError {
    error(
        home,
        ErrorType::Cancel,
        DefinedCondition::ServiceUnavailable,
        "The remote entity's response carried an address this server does not accept.",
    )
}

/// RFC 6120 §8.3.3.1: a stanza that is not a valid stanza of its kind.
pub(super) fn bad_request(home: &DomainRef) -> StanzaError {
    error(
        home,
        ErrorType::Modify,
        DefinedCondition::BadRequest,
        "The stanza is not valid.",
    )
}

/// The error answering `stanza`, from its recipient to its sender, or
/// `None` when no error may answer it: an error of any kind or an IQ result
/// (RFC 6120 §8.3.1 and §8.2.3). The request's payload is not echoed.
pub(super) fn error_reply(stanza: &Stanza, error: StanzaError) -> Option<Stanza> {
    match stanza {
        Stanza::Iq(iq) => match iq.as_ref() {
            Iq::Get { .. } | Iq::Set { .. } => Some(Stanza::Iq(Box::new(Iq::Error {
                from: iq.to().cloned(),
                to: iq.from().cloned(),
                id: iq.id().to_owned(),
                payload: None,
                error,
            }))),
            Iq::Result { .. } | Iq::Error { .. } => None,
        },
        Stanza::Message(message) => {
            if message.type_ == MessageType::Error {
                return None;
            }
            let mut reply = Message::error(message.from.clone());
            reply.from = message.to.clone();
            reply.id = message.id.clone();
            reply.payloads.push(Element::from(error));
            Some(Stanza::Message(reply))
        }
        Stanza::Presence(presence) => {
            if presence.type_ == PresenceType::Error {
                return None;
            }
            let mut reply = Presence::new(PresenceType::Error);
            reply.from = presence.to.clone();
            reply.to = presence.from.clone();
            reply.id = presence.id.clone();
            reply.payloads.push(Element::from(error));
            Some(Stanza::Presence(reply))
        }
    }
}

/// The error answering a received element that is not a valid stanza of
/// its kind, built from its name and attributes alone: its addresses are
/// already canonical. `None` when the element is an error or an IQ result,
/// or carries no usable address or IQ id.
pub(super) fn error_reply_to_element(element: &Element, error: StanzaError) -> Option<Stanza> {
    let address = |name: &str| {
        element
            .attr(name)
            .and_then(|value| waddle_xmpp::identity::parse_jid(value).ok())
    };
    let to = address("from")?;
    let from = address("to")?;
    let type_ = element.attr("type");
    match element.name() {
        "iq" => {
            if !matches!(type_, Some("get" | "set")) {
                return None;
            }
            let id = element.attr("id")?.to_owned();
            Some(Stanza::Iq(Box::new(Iq::Error {
                from: Some(from),
                to: Some(to),
                id,
                payload: None,
                error,
            })))
        }
        "message" => {
            if type_ == Some("error") {
                return None;
            }
            let mut reply = Message::error(Some(to));
            reply.from = Some(from);
            reply.id = element
                .attr("id")
                .map(|id| xmpp_parsers::message::Id(id.to_owned()));
            reply.payloads.push(Element::from(error));
            Some(Stanza::Message(reply))
        }
        "presence" => {
            if type_ == Some("error") {
                return None;
            }
            let mut reply = Presence::new(PresenceType::Error);
            reply.from = Some(from);
            reply.to = Some(to);
            reply.id = element.attr("id").map(str::to_owned);
            reply.payloads.push(Element::from(error));
            Some(Stanza::Presence(reply))
        }
        _ => None,
    }
}

/// Parse a received element, qualified by `jabber:client`, into a typed
/// stanza.
pub(super) fn parse_stanza(element: Element) -> Option<Stanza> {
    match element.name() {
        "message" => Message::try_from(element).ok().map(Stanza::Message),
        "presence" => Presence::try_from(element).ok().map(Stanza::Presence),
        "iq" => Iq::try_from(element)
            .ok()
            .map(|iq| Stanza::Iq(Box::new(iq))),
        _ => None,
    }
}

/// The `from` and `to` of a typed stanza.
pub(super) fn addresses(stanza: &Stanza) -> (Option<&Jid>, Option<&Jid>) {
    match stanza {
        Stanza::Iq(iq) => (iq.from(), iq.to()),
        Stanza::Message(message) => (message.from.as_ref(), message.to.as_ref()),
        Stanza::Presence(presence) => (presence.from.as_ref(), presence.to.as_ref()),
    }
}
