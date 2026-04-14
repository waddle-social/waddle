//! XEP-0482: Call Invites
//!
//! Structured call invitations using invite semantics and explicit join methods.
//!
//! ## XML Format
//!
//! Invite to a call:
//! ```xml
//! <message to='juliet@example.com' id='call-1'>
//!   <invite xmlns='urn:xmpp:call-invites:0'>
//!     <muji/>
//!     <jingle sid='jingle-session-123' jid='romeo@example.com/desktop'/>
//!     <external uri='https://meet.example.com/room-abc'/>
//!   </invite>
//! </message>
//! ```
//!
//! Accept with chosen method:
//! ```xml
//! <message to='romeo@example.com'>
//!   <accept xmlns='urn:xmpp:call-invites:0' id='call-1'>
//!     <external uri='https://meet.example.com/room-abc'/>
//!   </accept>
//! </message>
//! ```
//!
//! Leave:
//! ```xml
//! <message to='romeo@example.com'>
//!   <left xmlns='urn:xmpp:call-invites:0' id='call-1'/>
//! </message>
//! ```

use minidom::Element;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0482 Call Invites.
pub const NS_CALL_INVITES: &str = "urn:xmpp:call-invites:0";

/// A call join method advertised by an invitation or selected in acceptance.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CallJoinMethod {
    /// Join via Muji.
    Muji,
    /// Join via Jingle session reference.
    Jingle {
        /// Jingle session identifier.
        sid: String,
        /// Optional JID of the session endpoint.
        jid: Option<String>,
    },
    /// Join an external meeting URL.
    External {
        /// External URI (for example, HTTPS meeting link).
        uri: String,
    },
}

/// A call invitation containing one or more available join methods.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallInvite {
    /// Available join methods offered to recipients.
    pub methods: Vec<CallJoinMethod>,
}

impl CallInvite {
    /// Create an invitation with a single Muji join method.
    pub fn muji() -> Self {
        Self {
            methods: vec![CallJoinMethod::Muji],
        }
    }

    /// Create an invitation with a single external meeting URI.
    pub fn external(uri: impl Into<String>) -> Self {
        Self {
            methods: vec![CallJoinMethod::External { uri: uri.into() }],
        }
    }

    /// Add another join method to this invitation.
    pub fn with_method(mut self, method: CallJoinMethod) -> Self {
        self.methods.push(method);
        self
    }
}

/// A call acceptance with the chosen join method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallAccept {
    /// Invite message ID being accepted.
    pub invite_id: String,
    /// Chosen join method.
    pub method: CallJoinMethod,
}

/// A call action (invite, accept, reject, retract, left).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallAction {
    /// Invite others to a call with one or more join methods.
    Invite(CallInvite),
    /// Accept a call invitation and provide the chosen join method.
    Accept(CallAccept),
    /// Reject a call invitation.
    Reject(String),
    /// Retract (cancel) a previously sent call invitation.
    Retract(String),
    /// Signal that the participant has left the call.
    Left(String),
}

impl CallAction {
    /// Get the invite message ID for actions that reference one.
    pub fn invite_id(&self) -> Option<&str> {
        match self {
            Self::Invite(_) => None,
            Self::Accept(accept) => Some(accept.invite_id.as_str()),
            Self::Reject(id) | Self::Retract(id) | Self::Left(id) => Some(id),
        }
    }
}

/// Trait for types that can carry call invite elements.
pub trait CallInviteCarrier {
    /// Extract the call action from this carrier.
    fn call_action(&self) -> Option<CallAction>;

    /// Returns `true` if this is a call invitation.
    fn is_call_invite(&self) -> bool {
        matches!(self.call_action(), Some(CallAction::Invite(_)))
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
        "invite" => {
            let methods = parse_join_methods(elem);
            if methods.is_empty() {
                return None;
            }
            Some(CallAction::Invite(CallInvite { methods }))
        }
        "accept" => {
            let invite_id = elem.attr("id").filter(|s| !s.is_empty())?.to_owned();
            let mut methods = parse_join_methods(elem).into_iter();
            let method = methods.next()?;
            if methods.next().is_some() {
                return None;
            }
            Some(CallAction::Accept(CallAccept { invite_id, method }))
        }
        "reject" => {
            let id = elem.attr("id").filter(|s| !s.is_empty())?.to_owned();
            Some(CallAction::Reject(id))
        }
        "retract" => {
            let id = elem.attr("id").filter(|s| !s.is_empty())?.to_owned();
            Some(CallAction::Retract(id))
        }
        "left" => {
            let id = elem.attr("id").filter(|s| !s.is_empty())?.to_owned();
            Some(CallAction::Left(id))
        }
        _ => None,
    }
}

fn parse_join_methods(elem: &Element) -> Vec<CallJoinMethod> {
    let mut methods = Vec::new();

    for child in elem.children() {
        if child.ns() != NS_CALL_INVITES {
            continue;
        }

        match child.name() {
            "muji" => methods.push(CallJoinMethod::Muji),
            "jingle" => {
                if let Some(sid) = child.attr("sid").filter(|s| !s.is_empty()) {
                    let jid = child
                        .attr("jid")
                        .filter(|j| !j.is_empty())
                        .map(|j| j.to_owned());
                    methods.push(CallJoinMethod::Jingle {
                        sid: sid.to_owned(),
                        jid,
                    });
                }
            }
            "external" => {
                if let Some(uri) = child.attr("uri").filter(|u| !u.is_empty()) {
                    methods.push(CallJoinMethod::External {
                        uri: uri.to_owned(),
                    });
                }
            }
            _ => {}
        }
    }

    methods
}

// ── Building ─────────────────────────────────────────────────────────

/// Build an `<invite/>` element.
pub fn build_invite_element(invite: &CallInvite) -> Element {
    let mut elem = Element::builder("invite", NS_CALL_INVITES).build();

    for method in &invite.methods {
        elem.append_child(build_join_method_element(method));
    }

    elem
}

/// Build an `<accept/>` element with the chosen join method.
pub fn build_accept_element(invite_id: &str, method: &CallJoinMethod) -> Element {
    let mut elem = Element::builder("accept", NS_CALL_INVITES)
        .attr("id", invite_id)
        .build();

    elem.append_child(build_join_method_element(method));
    elem
}

/// Build a `<reject/>` element.
pub fn build_reject_element(invite_id: &str) -> Element {
    Element::builder("reject", NS_CALL_INVITES)
        .attr("id", invite_id)
        .build()
}

/// Build a `<retract/>` element.
pub fn build_retract_element(invite_id: &str) -> Element {
    Element::builder("retract", NS_CALL_INVITES)
        .attr("id", invite_id)
        .build()
}

/// Build a `<left/>` element.
pub fn build_left_element(invite_id: &str) -> Element {
    Element::builder("left", NS_CALL_INVITES)
        .attr("id", invite_id)
        .build()
}

fn build_join_method_element(method: &CallJoinMethod) -> Element {
    match method {
        CallJoinMethod::Muji => Element::builder("muji", NS_CALL_INVITES).build(),
        CallJoinMethod::Jingle { sid, jid } => {
            let mut builder = Element::builder("jingle", NS_CALL_INVITES).attr("sid", sid.as_str());
            if let Some(jid) = jid {
                builder = builder.attr("jid", jid.as_str());
            }
            builder.build()
        }
        CallJoinMethod::External { uri } => Element::builder("external", NS_CALL_INVITES)
            .attr("uri", uri.as_str())
            .build(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_invite() {
        let xml = "<message xmlns='jabber:client'>\
                    <invite xmlns='urn:xmpp:call-invites:0'>\
                      <muji/>\
                      <jingle sid='sid-1' jid='romeo@example.com/desktop'/>\
                      <external uri='https://meet.example.com/room'/>\
                    </invite>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let action = extract_call_action(&msg).expect("has action");
        match action {
            CallAction::Invite(invite) => {
                assert_eq!(invite.methods.len(), 3);
                assert!(invite.methods.contains(&CallJoinMethod::Muji));
                assert!(invite.methods.iter().any(|m| {
                    matches!(
                        m,
                        CallJoinMethod::Jingle { sid, jid } if sid == "sid-1" && jid.as_deref() == Some("romeo@example.com/desktop")
                    )
                }));
                assert!(invite.methods.iter().any(|m| {
                    matches!(
                        m,
                        CallJoinMethod::External { uri } if uri == "https://meet.example.com/room"
                    )
                }));
            }
            _ => panic!("Expected Invite"),
        }
    }

    #[test]
    fn test_parse_accept_with_method() {
        let xml = "<message xmlns='jabber:client'>\
                    <accept xmlns='urn:xmpp:call-invites:0' id='call-1'>\
                      <external uri='https://meet.example.com/room'/>\
                    </accept>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(matches!(
            extract_call_action(&msg),
            Some(CallAction::Accept(CallAccept { invite_id, method: CallJoinMethod::External { uri } }))
                if invite_id == "call-1" && uri == "https://meet.example.com/room"
        ));
    }

    #[test]
    fn test_parse_reject() {
        let xml = "<message xmlns='jabber:client'>\
                    <reject xmlns='urn:xmpp:call-invites:0' id='call-1'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(matches!(
            extract_call_action(&msg),
            Some(CallAction::Reject(id)) if id == "call-1"
        ));
    }

    #[test]
    fn test_parse_retract() {
        let xml = "<message xmlns='jabber:client'>\
                    <retract xmlns='urn:xmpp:call-invites:0' id='call-1'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(matches!(
            extract_call_action(&msg),
            Some(CallAction::Retract(id)) if id == "call-1"
        ));
    }

    #[test]
    fn test_parse_left() {
        let xml = "<message xmlns='jabber:client'>\
                    <left xmlns='urn:xmpp:call-invites:0' id='call-1'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(matches!(
            extract_call_action(&msg),
            Some(CallAction::Left(id)) if id == "call-1"
        ));
    }

    #[test]
    fn test_extract_absent() {
        let msg = Message::new(None::<jid::Jid>);
        assert!(extract_call_action(&msg).is_none());
    }

    #[test]
    fn test_build_invite() {
        let invite = CallInvite::muji()
            .with_method(CallJoinMethod::Jingle {
                sid: "sid-1".into(),
                jid: Some("romeo@example.com/desktop".into()),
            })
            .with_method(CallJoinMethod::External {
                uri: "https://meet.example.com/room".into(),
            });
        let elem = build_invite_element(&invite);

        assert_eq!(elem.name(), "invite");
        assert!(elem.children().any(|c| c.name() == "muji"));
        assert!(elem
            .children()
            .any(|c| c.name() == "jingle" && c.attr("sid") == Some("sid-1")));
        assert!(elem.children().any(|c| c.name() == "external"));
    }

    #[test]
    fn test_build_accept_reject_retract_left() {
        let accept = build_accept_element(
            "call-1",
            &CallJoinMethod::External {
                uri: "https://meet.example.com/room".into(),
            },
        );
        assert_eq!(accept.name(), "accept");
        assert_eq!(accept.attr("id"), Some("call-1"));
        assert!(accept.children().any(|c| c.name() == "external"));

        let reject = build_reject_element("call-2");
        assert_eq!(reject.name(), "reject");

        let retract = build_retract_element("call-3");
        assert_eq!(retract.name(), "retract");

        let left = build_left_element("call-4");
        assert_eq!(left.name(), "left");
    }

    #[test]
    fn test_call_action_invite_id() {
        let invite = CallAction::Invite(CallInvite::external("https://meet.example.com/room"));
        assert_eq!(invite.invite_id(), None);

        let accept = CallAction::Accept(CallAccept {
            invite_id: "call-1".into(),
            method: CallJoinMethod::Muji,
        });
        assert_eq!(accept.invite_id(), Some("call-1"));
    }

    #[test]
    fn test_call_invite_carrier_trait() {
        let xml = "<message xmlns='jabber:client'>\
                    <invite xmlns='urn:xmpp:call-invites:0'>\
                      <muji/>\
                    </invite>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(msg.has_call_element());
        assert!(msg.is_call_invite());
    }

    #[test]
    fn test_accept_requires_method() {
        let xml = "<message xmlns='jabber:client'>\
                    <accept xmlns='urn:xmpp:call-invites:0' id='call-1'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(extract_call_action(&msg).is_none());
    }

    #[test]
    fn test_accept_rejects_multiple_methods() {
        let xml = "<message xmlns='jabber:client'>\
                    <accept xmlns='urn:xmpp:call-invites:0' id='call-1'>\
                      <muji/>\
                      <external uri='https://meet.example.com/room'/>\
                    </accept>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(extract_call_action(&msg).is_none());
    }

    #[test]
    fn test_join_methods_ignore_foreign_namespace_children() {
        let xml = "<message xmlns='jabber:client'>\
                    <invite xmlns='urn:xmpp:call-invites:0'>\
                      <muji xmlns='urn:xmpp:muji:0'/>\
                    </invite>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(extract_call_action(&msg).is_none());
    }
}
