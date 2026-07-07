//! XEP-0425: Moderated Message Retraction (v1).
//!
//! Allows MUC moderators to retract any occupant's message. Replaces
//! the v0 XEP-0422 message-fastening shape with a direct IQ from the
//! moderator and a `<retract>`-shaped broadcast carrying a
//! `<moderated>` attribution child.
//!
//! ## XML Format
//!
//! Moderator → room (IQ request):
//! ```xml
//! <iq type='set' to='room@muc.example.com' id='mod-1'>
//!   <moderate id='target-stanza-id' xmlns='urn:xmpp:message-moderate:1'>
//!     <retract xmlns='urn:xmpp:message-retract:1'/>
//!     <reason>Spam</reason>
//!   </moderate>
//! </iq>
//! ```
//!
//! Room → all occupants (groupchat broadcast):
//! ```xml
//! <message type='groupchat' from='room@muc.example.com'>
//!   <retract id='target-stanza-id' xmlns='urn:xmpp:message-retract:1'>
//!     <moderated by='room@muc.example.com/modnick' xmlns='urn:xmpp:message-moderate:1'>
//!       <occupant-id xmlns='urn:xmpp:occupant-id:0' id='dd72…'/>
//!     </moderated>
//!     <reason>Spam</reason>
//!   </retract>
//! </message>
//! ```
//!
//! Per XEP-0425 v1 §3, `<moderated>` can carry the moderator's
//! XEP-0421 `<occupant-id/>` child alongside the room-nick `by=`
//! attribution.
//!
//! ## Server Behavior
//!
//! The MUC service MUST:
//! - Verify the sender has Moderator role.
//! - Look up the target message in MAM.
//! - Broadcast a `<message>` with `<retract id='…'>` containing a
//!   `<moderated by='…'><occupant-id/></moderated>` child and the
//!   optional `<reason/>`.
//! - Replace the archived message contents with a `<retracted/>`
//!   tombstone (XEP-0424 path) preserving the stanza-id position.

use minidom::Element;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0425 Message Moderation v1.
pub const NS_MESSAGE_MODERATE: &str = "urn:xmpp:message-moderate:1";

/// Namespace for XEP-0424 Message Retraction (used within moderation).
const NS_RETRACT: &str = "urn:xmpp:message-retract:1";

/// Namespace for XEP-0421 Occupant Identifiers (embedded in `<moderated>`).
const NS_OCCUPANT_ID: &str = "urn:xmpp:occupant-id:0";

/// A moderation request from a moderator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModerationRequest {
    /// The ID of the message to moderate.
    pub target_id: String,
    /// Optional reason for the moderation action.
    pub reason: Option<String>,
}

/// Parse a current XEP-0425 IQ moderation request:
/// `<iq type='set'><moderate id='stanza-id'><retract/><reason/></moderate></iq>`.
pub fn parse_moderation_iq(iq: &Iq) -> Option<ModerationRequest> {
    let moderate = match iq {
        Iq::Set { payload: elem, .. }
            if elem.name() == "moderate" && elem.ns() == NS_MESSAGE_MODERATE =>
        {
            elem
        }
        _ => return None,
    };
    let target_id = moderate.attr("id").filter(|value| !value.is_empty())?;
    moderate
        .children()
        .find(|child| child.name() == "retract" && child.ns() == NS_RETRACT)?;
    let reason = moderate
        .children()
        .find(|child| child.name() == "reason" && child.ns() == NS_MESSAGE_MODERATE)
        .map(|child| child.text())
        .filter(|value| !value.trim().is_empty());

    Some(ModerationRequest {
        target_id: target_id.to_owned(),
        reason,
    })
}

impl ModerationRequest {
    /// Create a new moderation request.
    pub fn new(target_id: impl Into<String>) -> Self {
        Self {
            target_id: target_id.into(),
            reason: None,
        }
    }

    /// Set the reason.
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

/// A moderation result broadcast by the server.
///
/// Carries the moderator's MUC JID **and** their XEP-0421
/// `<occupant-id/>` when present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModerationResult {
    /// The ID of the moderated message.
    pub target_id: String,
    /// The MUC JID of the moderator who performed the action.
    pub moderated_by: String,
    /// XEP-0421 occupant-id of the moderator.
    pub moderator_occupant_id: Option<String>,
    /// Optional reason.
    pub reason: Option<String>,
}

/// Trait for types that can carry moderation elements.
pub trait ModerationCarrier {
    /// Extract a moderation result from this carrier.
    fn moderation_result(&self) -> Option<ModerationResult>;

    /// Returns `true` if this is a moderation result (broadcast).
    fn is_moderation_result(&self) -> bool {
        self.moderation_result().is_some()
    }
}

impl ModerationCarrier for Message {
    fn moderation_result(&self) -> Option<ModerationResult> {
        extract_moderation_result(self)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if a message contains a v1 moderation broadcast
/// (`<retract>` with `<moderated>` child).
pub fn is_moderation_result_message(msg: &Message) -> bool {
    extract_moderation_result(msg).is_some()
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract a moderation result from a message broadcast.
pub fn extract_moderation_result(msg: &Message) -> Option<ModerationResult> {
    let retract = msg
        .payloads
        .iter()
        .find(|e| e.name() == "retract" && e.ns() == NS_RETRACT)?;
    let target_id = retract.attr("id").filter(|s| !s.is_empty())?.to_owned();
    let moderated = retract
        .children()
        .find(|c| c.name() == "moderated" && c.ns() == NS_MESSAGE_MODERATE)?;
    let moderated_by = moderated.attr("by").filter(|s| !s.is_empty())?.to_owned();
    let moderator_occupant_id = moderated
        .children()
        .find(|c| c.name() == "occupant-id" && c.ns() == NS_OCCUPANT_ID)
        .and_then(|c| c.attr("id"))
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    let reason = retract
        .children()
        .find(|c| c.name() == "reason" && c.ns() == NS_RETRACT)
        .map(|c| c.text())
        .filter(|t| !t.is_empty());

    Some(ModerationResult {
        target_id,
        moderated_by,
        moderator_occupant_id,
        reason,
    })
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a complete moderation result message for broadcasting.
///
/// `moderator_occupant_id` is the moderator's XEP-0421 occupant id.
/// Supplying it keeps moderation attribution stable alongside the
/// room-nick `by=` value.
pub fn build_moderation_result_message(
    from_room: impl Into<Option<jid::Jid>>,
    target_id: &str,
    moderated_by: &str,
    moderator_occupant_id: Option<&str>,
    reason: Option<&str>,
) -> Message {
    let mut msg = Message::new(None::<jid::Jid>);
    msg.from = from_room.into();
    msg.type_ = xmpp_parsers::message::MessageType::Groupchat;
    msg.id = Some(xmpp_parsers::message::Id(uuid::Uuid::new_v4().to_string()));
    msg.payloads.push(build_moderated_retract_element(
        target_id,
        moderated_by,
        moderator_occupant_id,
        reason,
    ));
    msg
}

/// Build the v1 XEP-0425 broadcast payload:
///
/// ```xml
/// <retract id='target' xmlns='urn:xmpp:message-retract:1'>
///   <moderated by='moderator' xmlns='urn:xmpp:message-moderate:1'>
///     <occupant-id id='…' xmlns='urn:xmpp:occupant-id:0'/>
///   </moderated>
///   <reason>…</reason>
/// </retract>
/// ```
///
/// The `<occupant-id>` child is emitted when
/// `moderator_occupant_id` is `Some`, matching the §3 attribution
/// shape.
pub fn build_moderated_retract_element(
    target_id: &str,
    moderated_by: &str,
    moderator_occupant_id: Option<&str>,
    reason: Option<&str>,
) -> Element {
    let mut moderated = Element::builder("moderated", NS_MESSAGE_MODERATE)
        .attr(minidom::rxml::xml_ncname!("by").to_owned(), moderated_by);
    if let Some(occupant_id) = moderator_occupant_id.filter(|s| !s.is_empty()) {
        moderated = moderated.append(
            Element::builder("occupant-id", NS_OCCUPANT_ID)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), occupant_id)
                .build(),
        );
    }
    let mut retract = Element::builder("retract", NS_RETRACT)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), target_id)
        .append(moderated.build());
    if let Some(reason_text) = reason {
        retract = retract.append(
            Element::builder("reason", NS_RETRACT)
                .append(reason_text)
                .build(),
        );
    }

    retract.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::iq::Iq;
    use xmpp_parsers::message::{Message, MessageType};

    #[test]
    fn test_parse_moderation_request() {
        let xml = "<iq xmlns='jabber:client' type='set' id='mod-1'>\
                    <moderate xmlns='urn:xmpp:message-moderate:1' id='target-1'>\
                      <retract xmlns='urn:xmpp:message-retract:1'/>\
                      <reason>Spam</reason>\
                    </moderate>\
                   </iq>";
        let iq = Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid iq");

        let req = parse_moderation_iq(&iq).expect("has request");
        assert_eq!(req.target_id, "target-1");
        assert_eq!(req.reason.as_deref(), Some("Spam"));
    }

    #[test]
    fn test_parse_moderation_request_no_reason() {
        let xml = "<iq xmlns='jabber:client' type='set' id='mod-2'>\
                    <moderate xmlns='urn:xmpp:message-moderate:1' id='target-2'>\
                      <retract xmlns='urn:xmpp:message-retract:1'/>\
                    </moderate>\
                   </iq>";
        let iq = Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid iq");

        let req = parse_moderation_iq(&iq).expect("has request");
        assert_eq!(req.target_id, "target-2");
        assert_eq!(req.reason, None);
    }

    #[test]
    fn test_parse_moderation_result() {
        // Note the v1 spec shape: `<retract>` outer, `<moderated>`
        // inner with its own `<occupant-id>` child for attribution.
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <retract xmlns='urn:xmpp:message-retract:1' id='target-1'>\
                      <moderated xmlns='urn:xmpp:message-moderate:1' by='room@muc.example.com/modnick'>\
                        <occupant-id xmlns='urn:xmpp:occupant-id:0' id='abc123'/>\
                      </moderated>\
                      <reason>Spam</reason>\
                    </retract>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let result = extract_moderation_result(&msg).expect("has result");
        assert_eq!(result.target_id, "target-1");
        assert_eq!(result.moderated_by, "room@muc.example.com/modnick");
        assert_eq!(result.moderator_occupant_id.as_deref(), Some("abc123"));
        assert_eq!(result.reason.as_deref(), Some("Spam"));
    }

    #[test]
    fn test_extract_absent() {
        let msg = Message::new(None::<jid::Jid>);
        assert!(extract_moderation_result(&msg).is_none());
    }

    #[test]
    fn test_build_moderation_result() {
        let elem = build_moderated_retract_element(
            "msg-42",
            "room@muc.example.com/admin",
            Some("opaque-occupant-id"),
            Some("Spam"),
        );

        let mut msg = Message::new(None::<jid::Jid>);
        msg.type_ = MessageType::Groupchat;
        msg.payloads.push(elem);

        let result = extract_moderation_result(&msg).expect("parseable");
        assert_eq!(result.target_id, "msg-42");
        assert_eq!(result.moderated_by, "room@muc.example.com/admin");
        assert_eq!(
            result.moderator_occupant_id.as_deref(),
            Some("opaque-occupant-id")
        );
        assert_eq!(result.reason.as_deref(), Some("Spam"));
    }

    #[test]
    fn test_build_moderation_result_message() {
        let msg = build_moderation_result_message(
            "room@muc.example.com".parse::<jid::Jid>().ok(),
            "orig-1",
            "room@muc.example.com/mod",
            None,
            None,
        );

        assert_eq!(msg.type_, MessageType::Groupchat);
        let result = extract_moderation_result(&msg).expect("parseable");
        assert_eq!(result.target_id, "orig-1");
        assert_eq!(result.moderator_occupant_id, None);
        assert_eq!(result.reason, None);
    }

    #[test]
    fn test_moderation_carrier_trait_result() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <retract xmlns='urn:xmpp:message-retract:1' id='t-1'>\
                      <moderated xmlns='urn:xmpp:message-moderate:1' by='mod@room'/>\
                    </retract>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(msg.is_moderation_result());
    }

    #[test]
    fn test_moderation_request_builder() {
        let req = ModerationRequest::new("id-1");
        assert_eq!(req.target_id, "id-1");
        assert_eq!(req.reason, None);

        let req2 = ModerationRequest::new("id-2").with_reason("Bad");
        assert_eq!(req2.reason.as_deref(), Some("Bad"));
    }

    #[test]
    fn test_is_helpers() {
        let plain = Message::new(None::<jid::Jid>);
        assert!(!is_moderation_result_message(&plain));
    }
}
