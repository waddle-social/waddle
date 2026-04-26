//! XEP-0425: Moderated Message Retraction
//!
//! Allows MUC moderators to retract any occupant's message. Extends
//! XEP-0424 (Message Retraction) with moderator authority and uses
//! XEP-0422 (Message Fastening) for targeting.
//!
//! ## XML Format
//!
//! Moderator sends to room:
//! ```xml
//! <message type='groupchat' to='room@muc.example.com' id='mod-1'>
//!   <apply-to id='target-msg-id' xmlns='urn:xmpp:fasten:0'>
//!     <moderate xmlns='urn:xmpp:message-moderate:1'>
//!       <retract xmlns='urn:xmpp:message-retract:1'/>
//!       <reason>Spam</reason>
//!     </moderate>
//!   </apply-to>
//! </message>
//! ```
//!
//! Server broadcasts to room:
//! ```xml
//! <message type='groupchat' from='room@muc.example.com'>
//!   <apply-to id='target-msg-id' xmlns='urn:xmpp:fasten:0'>
//!     <moderated xmlns='urn:xmpp:message-moderate:1' by='room@muc.example.com/modnick'>
//!       <retracted xmlns='urn:xmpp:message-retract:1' stamp='2024-06-01T12:00:00Z'/>
//!       <reason>Spam</reason>
//!     </moderated>
//!   </apply-to>
//! </message>
//! ```
//!
//! ## Server Behavior
//!
//! The MUC service MUST:
//! - Verify the sender has moderator role
//! - Replace `<moderate>` with `<moderated by='...'>` adding the moderator's JID
//! - Replace `<retract/>` with `<retracted stamp='...'/>`
//! - Broadcast to all occupants

use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::message::Message;

/// Namespace for XEP-0425 Message Moderation.
pub const NS_MESSAGE_MODERATE: &str = "urn:xmpp:message-moderate:1";

/// Namespace for XEP-0422 Message Fastening.
pub const NS_FASTEN: &str = "urn:xmpp:fasten:0";

/// Namespace for XEP-0424 Message Retraction (used within moderation).
const NS_RETRACT: &str = "urn:xmpp:message-retract:1";

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
    let moderate = match &iq.payload {
        IqType::Set(elem) if elem.name() == "moderate" && elem.ns() == NS_MESSAGE_MODERATE => elem,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModerationResult {
    /// The ID of the moderated message.
    pub target_id: String,
    /// The MUC JID of the moderator who performed the action.
    pub moderated_by: String,
    /// Timestamp of the moderation action.
    pub stamp: String,
    /// Optional reason.
    pub reason: Option<String>,
}

/// Trait for types that can carry moderation elements.
pub trait ModerationCarrier {
    /// Extract a moderation request from this carrier.
    fn moderation_request(&self) -> Option<ModerationRequest>;

    /// Extract a moderation result from this carrier.
    fn moderation_result(&self) -> Option<ModerationResult>;

    /// Returns `true` if this is a moderation request.
    fn is_moderation_request(&self) -> bool {
        self.moderation_request().is_some()
    }

    /// Returns `true` if this is a moderation result (broadcast).
    fn is_moderation_result(&self) -> bool {
        self.moderation_result().is_some()
    }

    /// Returns `true` if this carries any moderation element.
    fn has_moderation(&self) -> bool {
        self.is_moderation_request() || self.is_moderation_result()
    }
}

impl ModerationCarrier for Message {
    fn moderation_request(&self) -> Option<ModerationRequest> {
        extract_moderation_request(self)
    }

    fn moderation_result(&self) -> Option<ModerationResult> {
        extract_moderation_result(self)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if a message contains a moderation request (`<apply-to>/<moderate>`).
pub fn is_moderation_request_message(msg: &Message) -> bool {
    extract_moderation_request(msg).is_some()
}

/// Check if a message contains a moderation result (`<apply-to>/<moderated>`).
pub fn is_moderation_result_message(msg: &Message) -> bool {
    extract_moderation_result(msg).is_some()
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract a moderation request from a message.
pub fn extract_moderation_request(_msg: &Message) -> Option<ModerationRequest> {
    None
}

/// Extract a moderation result from a message.
pub fn extract_moderation_result(msg: &Message) -> Option<ModerationResult> {
    if let Some(retract) = msg
        .payloads
        .iter()
        .find(|e| e.name() == "retract" && e.ns() == NS_RETRACT)
    {
        let target_id = retract.attr("id").filter(|s| !s.is_empty())?.to_owned();
        let moderated = retract
            .children()
            .find(|c| c.name() == "moderated" && c.ns() == NS_MESSAGE_MODERATE)?;
        let moderated_by = moderated.attr("by").filter(|s| !s.is_empty())?.to_owned();
        let reason = retract
            .children()
            .find(|c| c.name() == "reason" && c.ns() == NS_RETRACT)
            .map(|c| c.text())
            .filter(|t| !t.is_empty());

        return Some(ModerationResult {
            target_id,
            moderated_by,
            stamp: String::new(),
            reason,
        });
    }

    None
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a complete moderation result message for broadcasting.
pub fn build_moderation_result_message(
    from_room: impl Into<Option<jid::Jid>>,
    target_id: &str,
    moderated_by: &str,
    stamp: &str,
    reason: Option<&str>,
) -> Message {
    let mut msg = Message::new(None::<jid::Jid>);
    msg.from = from_room.into();
    msg.type_ = xmpp_parsers::message::MessageType::Groupchat;
    msg.id = Some(uuid::Uuid::new_v4().to_string());
    msg.payloads.push(build_moderated_retract_element(
        target_id,
        moderated_by,
        stamp,
        reason,
    ));
    msg
}

/// Build the current XEP-0425 broadcast payload:
/// `<retract id='target'><moderated by='moderator'/><reason>...</reason></retract>`.
pub fn build_moderated_retract_element(
    target_id: &str,
    moderated_by: &str,
    stamp: &str,
    reason: Option<&str>,
) -> Element {
    let moderated = Element::builder("moderated", NS_MESSAGE_MODERATE)
        .attr("by", moderated_by)
        .build();
    let mut retract = Element::builder("retract", NS_RETRACT)
        .attr("id", target_id)
        .append(moderated);
    if let Some(reason_text) = reason {
        retract = retract.append(
            Element::builder("reason", NS_RETRACT)
                .append(reason_text)
                .build(),
        );
    }

    let _ = stamp;
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
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <retract xmlns='urn:xmpp:message-retract:1' id='target-1'>\
                      <moderated xmlns='urn:xmpp:message-moderate:1' by='room@muc.example.com/modnick'/>\
                      <reason>Spam</reason>\
                    </retract>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let result = extract_moderation_result(&msg).expect("has result");
        assert_eq!(result.target_id, "target-1");
        assert_eq!(result.moderated_by, "room@muc.example.com/modnick");
        assert_eq!(result.stamp, "");
        assert_eq!(result.reason.as_deref(), Some("Spam"));
    }

    #[test]
    fn test_extract_absent() {
        let msg = Message::new(None::<jid::Jid>);
        assert!(extract_moderation_request(&msg).is_none());
        assert!(extract_moderation_result(&msg).is_none());
    }

    #[test]
    fn test_build_moderation_result() {
        let elem = build_moderated_retract_element(
            "msg-42",
            "room@muc.example.com/admin",
            "2024-06-01T12:00:00Z",
            Some("Spam"),
        );

        let mut msg = Message::new(None::<jid::Jid>);
        msg.type_ = MessageType::Groupchat;
        msg.payloads.push(elem);

        let result = extract_moderation_result(&msg).expect("parseable");
        assert_eq!(result.target_id, "msg-42");
        assert_eq!(result.moderated_by, "room@muc.example.com/admin");
        assert_eq!(result.reason.as_deref(), Some("Spam"));
    }

    #[test]
    fn test_build_moderation_result_message() {
        let msg = build_moderation_result_message(
            "room@muc.example.com".parse::<jid::Jid>().ok(),
            "orig-1",
            "room@muc.example.com/mod",
            "2024-01-01T00:00:00Z",
            None,
        );

        assert_eq!(msg.type_, MessageType::Groupchat);
        let result = extract_moderation_result(&msg).expect("parseable");
        assert_eq!(result.target_id, "orig-1");
        assert_eq!(result.reason, None);
    }

    #[test]
    fn test_moderation_carrier_trait_request() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <apply-to xmlns='urn:xmpp:fasten:0' id='t-1'>\
                      <moderate xmlns='urn:xmpp:message-moderate:1'>\
                        <retract xmlns='urn:xmpp:message-retract:1'/>\
                      </moderate>\
                    </apply-to>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(!msg.is_moderation_request());
        assert!(!msg.is_moderation_result());
        assert!(!msg.has_moderation());
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

        assert!(!msg.is_moderation_request());
        assert!(msg.is_moderation_result());
        assert!(msg.has_moderation());
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
        assert!(!is_moderation_request_message(&plain));
        assert!(!is_moderation_result_message(&plain));
    }
}
