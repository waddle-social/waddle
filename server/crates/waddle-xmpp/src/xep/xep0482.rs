//! XEP-0482: Call Invites
//!
//! Structured call invitations for audio/video calls. Allows inviting
//! users to calls with accept/reject/retract semantics.
//!
//! ## XML Format
//!
//! Invite to a call:
//! ```xml
//! <message to='juliet@example.com' id='call-1'>
//!   <propose xmlns='urn:xmpp:call-invites:0' id='session-123'>
//!     <audio/>
//!     <video/>
//!     <external uri='https://meet.example.com/room-abc'/>
//!   </propose>
//! </message>
//! ```
//!
//! Accept:
//! ```xml
//! <message to='romeo@example.com'>
//!   <accept xmlns='urn:xmpp:call-invites:0' id='session-123'/>
//! </message>
//! ```
//!
//! Reject:
//! ```xml
//! <message to='romeo@example.com'>
//!   <reject xmlns='urn:xmpp:call-invites:0' id='session-123'/>
//! </message>
//! ```

use minidom::Element;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0482 Call Invites.
pub const NS_CALL_INVITES: &str = "urn:xmpp:call-invites:0";

/// Media types available in a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallMedia {
    /// Audio only.
    Audio,
    /// Video (implies audio).
    Video,
}

/// A call invitation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallPropose {
    /// Unique session identifier.
    pub session_id: String,
    /// Available media types.
    pub media: Vec<CallMedia>,
    /// Optional external meeting URL.
    pub external_uri: Option<String>,
}

impl CallPropose {
    /// Create an audio call proposal.
    pub fn audio(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            media: vec![CallMedia::Audio],
            external_uri: None,
        }
    }

    /// Create a video call proposal.
    pub fn video(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            media: vec![CallMedia::Audio, CallMedia::Video],
            external_uri: None,
        }
    }

    /// Set an external meeting URI.
    pub fn with_external_uri(mut self, uri: impl Into<String>) -> Self {
        self.external_uri = Some(uri.into());
        self
    }

    /// Returns `true` if this includes video.
    pub fn has_video(&self) -> bool {
        self.media.contains(&CallMedia::Video)
    }
}

/// A call action (accept, reject, retract).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallAction {
    /// Propose a call.
    Propose(CallPropose),
    /// Accept a call invitation.
    Accept(String),
    /// Reject a call invitation.
    Reject(String),
    /// Retract (cancel) a call invitation.
    Retract(String),
}

impl CallAction {
    /// Get the session ID for any action.
    pub fn session_id(&self) -> &str {
        match self {
            Self::Propose(p) => &p.session_id,
            Self::Accept(id) | Self::Reject(id) | Self::Retract(id) => id,
        }
    }
}

/// Trait for types that can carry call invite elements.
pub trait CallInviteCarrier {
    /// Extract the call action from this carrier.
    fn call_action(&self) -> Option<CallAction>;

    /// Returns `true` if this is a call proposal.
    fn is_call_propose(&self) -> bool {
        matches!(self.call_action(), Some(CallAction::Propose(_)))
    }

    /// Returns `true` if this has any call-related element.
    fn has_call_element(&self) -> bool {
        self.call_action().is_some()
    }
}

impl CallInviteCarrier for Message {
    fn call_action(&self) -> Option<CallAction> {
        extract_call_action(self)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if a message has any call invite element.
pub fn has_call_invite(msg: &Message) -> bool {
    msg.payloads.iter().any(|e| e.ns() == NS_CALL_INVITES)
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract the call action from a message.
pub fn extract_call_action(msg: &Message) -> Option<CallAction> {
    let elem = msg.payloads.iter().find(|e| e.ns() == NS_CALL_INVITES)?;

    match elem.name() {
        "propose" => {
            let session_id = elem.attr("id").filter(|s| !s.is_empty())?.to_owned();
            let mut media = Vec::new();
            for child in elem.children() {
                match child.name() {
                    "audio" => media.push(CallMedia::Audio),
                    "video" => media.push(CallMedia::Video),
                    _ => {}
                }
            }
            let external_uri = elem
                .children()
                .find(|c| c.name() == "external")
                .and_then(|c| c.attr("uri"))
                .filter(|u| !u.is_empty())
                .map(|u| u.to_owned());

            Some(CallAction::Propose(CallPropose {
                session_id,
                media,
                external_uri,
            }))
        }
        "accept" => {
            let id = elem.attr("id").filter(|s| !s.is_empty())?.to_owned();
            Some(CallAction::Accept(id))
        }
        "reject" => {
            let id = elem.attr("id").filter(|s| !s.is_empty())?.to_owned();
            Some(CallAction::Reject(id))
        }
        "retract" => {
            let id = elem.attr("id").filter(|s| !s.is_empty())?.to_owned();
            Some(CallAction::Retract(id))
        }
        _ => None,
    }
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<propose/>` element.
pub fn build_propose_element(propose: &CallPropose) -> Element {
    let mut elem = Element::builder("propose", NS_CALL_INVITES)
        .attr("id", propose.session_id.as_str())
        .build();

    for media in &propose.media {
        match media {
            CallMedia::Audio => {
                elem.append_child(Element::builder("audio", NS_CALL_INVITES).build());
            }
            CallMedia::Video => {
                elem.append_child(Element::builder("video", NS_CALL_INVITES).build());
            }
        }
    }

    if let Some(ref uri) = propose.external_uri {
        let ext = Element::builder("external", NS_CALL_INVITES)
            .attr("uri", uri.as_str())
            .build();
        elem.append_child(ext);
    }

    elem
}

/// Build an `<accept/>` element.
pub fn build_accept_element(session_id: &str) -> Element {
    Element::builder("accept", NS_CALL_INVITES)
        .attr("id", session_id)
        .build()
}

/// Build a `<reject/>` element.
pub fn build_reject_element(session_id: &str) -> Element {
    Element::builder("reject", NS_CALL_INVITES)
        .attr("id", session_id)
        .build()
}

/// Build a `<retract/>` element.
pub fn build_retract_element(session_id: &str) -> Element {
    Element::builder("retract", NS_CALL_INVITES)
        .attr("id", session_id)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_propose() {
        let xml = "<message xmlns='jabber:client'>\
                    <propose xmlns='urn:xmpp:call-invites:0' id='sess-1'>\
                      <audio/>\
                      <video/>\
                      <external uri='https://meet.example.com/room'/>\
                    </propose>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let action = extract_call_action(&msg).expect("has action");
        match action {
            CallAction::Propose(p) => {
                assert_eq!(p.session_id, "sess-1");
                assert!(p.has_video());
                assert_eq!(p.media.len(), 2);
                assert_eq!(
                    p.external_uri.as_deref(),
                    Some("https://meet.example.com/room")
                );
            }
            _ => panic!("Expected Propose"),
        }
    }

    #[test]
    fn test_parse_accept() {
        let xml = "<message xmlns='jabber:client'>\
                    <accept xmlns='urn:xmpp:call-invites:0' id='sess-1'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(matches!(
            extract_call_action(&msg),
            Some(CallAction::Accept(id)) if id == "sess-1"
        ));
    }

    #[test]
    fn test_parse_reject() {
        let xml = "<message xmlns='jabber:client'>\
                    <reject xmlns='urn:xmpp:call-invites:0' id='sess-1'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(matches!(
            extract_call_action(&msg),
            Some(CallAction::Reject(id)) if id == "sess-1"
        ));
    }

    #[test]
    fn test_parse_retract() {
        let xml = "<message xmlns='jabber:client'>\
                    <retract xmlns='urn:xmpp:call-invites:0' id='sess-1'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(matches!(
            extract_call_action(&msg),
            Some(CallAction::Retract(id)) if id == "sess-1"
        ));
    }

    #[test]
    fn test_extract_absent() {
        let msg = Message::new(None::<jid::Jid>);
        assert!(extract_call_action(&msg).is_none());
    }

    #[test]
    fn test_build_propose() {
        let propose = CallPropose::video("s-1").with_external_uri("https://meet.example.com/room");
        let elem = build_propose_element(&propose);

        assert_eq!(elem.name(), "propose");
        assert_eq!(elem.attr("id"), Some("s-1"));
        assert!(elem.children().any(|c| c.name() == "audio"));
        assert!(elem.children().any(|c| c.name() == "video"));
        assert!(elem.children().any(|c| c.name() == "external"));
    }

    #[test]
    fn test_build_accept_reject_retract() {
        let accept = build_accept_element("s-1");
        assert_eq!(accept.name(), "accept");
        assert_eq!(accept.attr("id"), Some("s-1"));

        let reject = build_reject_element("s-2");
        assert_eq!(reject.name(), "reject");

        let retract = build_retract_element("s-3");
        assert_eq!(retract.name(), "retract");
    }

    #[test]
    fn test_call_action_session_id() {
        let propose = CallAction::Propose(CallPropose::audio("s-1"));
        assert_eq!(propose.session_id(), "s-1");

        let accept = CallAction::Accept("s-2".into());
        assert_eq!(accept.session_id(), "s-2");
    }

    #[test]
    fn test_call_invite_carrier_trait() {
        let xml = "<message xmlns='jabber:client'>\
                    <propose xmlns='urn:xmpp:call-invites:0' id='s-1'>\
                      <audio/>\
                    </propose>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(msg.has_call_element());
        assert!(msg.is_call_propose());
    }

    #[test]
    fn test_propose_builders() {
        let audio = CallPropose::audio("s-1");
        assert!(!audio.has_video());
        assert_eq!(audio.media.len(), 1);

        let video = CallPropose::video("s-2");
        assert!(video.has_video());
        assert_eq!(video.media.len(), 2);
    }
}
