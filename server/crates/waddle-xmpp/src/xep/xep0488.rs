//! XEP-0488: MUC Token Invite
//!
//! Provides invite tokens for MUC rooms, enabling easy room onboarding
//! via shareable links. Tokens can be single-use or multi-use.
//!
//! ## XML Format
//!
//! Request an invite token:
//! ```xml
//! <iq type='set' to='room@muc.example.com' id='inv-1'>
//!   <request xmlns='urn:xmpp:muc-token-invite:0'/>
//! </iq>
//! ```
//!
//! Server responds with token:
//! ```xml
//! <iq type='result' from='room@muc.example.com' id='inv-1'>
//!   <invite xmlns='urn:xmpp:muc-token-invite:0' token='abc123def456'/>
//! </iq>
//! ```
//!
//! Share invite via message:
//! ```xml
//! <message to='friend@example.com'>
//!   <body>Join our room: xmpp:room@muc.example.com?join;password=abc123def456</body>
//!   <invite xmlns='urn:xmpp:muc-token-invite:0'
//!           token='abc123def456'
//!           jid='room@muc.example.com'/>
//! </message>
//! ```
//!
//! ## Use Cases
//!
//! - Generate shareable invite links for private rooms
//! - One-click room join without knowing the JID
//! - Token expiry and usage limits for security

use minidom::Element;
use thiserror::Error;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0488 MUC Token Invite.
pub const NS_MUC_TOKEN_INVITE: &str = "urn:xmpp:muc-token-invite:0";

/// Errors that can occur with invite tokens.
#[derive(Debug, Error)]
pub enum InviteTokenError {
    /// Token generation failed.
    #[error("failed to generate invite token: {0}")]
    GenerationFailed(String),
    /// Token is invalid or expired.
    #[error("invalid or expired invite token")]
    InvalidToken,
}

/// An invite token for a MUC room.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InviteToken {
    /// The opaque invite token string.
    pub token: String,
    /// The room JID this token is for.
    pub room_jid: Option<String>,
}

impl InviteToken {
    /// Create a new invite token.
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            room_jid: None,
        }
    }

    /// Set the room JID.
    pub fn with_room(mut self, jid: impl Into<String>) -> Self {
        self.room_jid = Some(jid.into());
        self
    }

    /// Generate an XMPP invite URI.
    ///
    /// Format: `xmpp:room@muc.example.com?join;password=TOKEN`
    pub fn to_uri(&self) -> Option<String> {
        self.room_jid
            .as_ref()
            .map(|jid| format!("xmpp:{}?join;password={}", jid, self.token))
    }
}

impl std::fmt::Display for InviteToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.token)
    }
}

/// Trait for types that can carry invite token elements.
pub trait InviteTokenCarrier {
    /// Extract an invite token from this carrier.
    fn invite_token(&self) -> Option<InviteToken>;

    /// Returns `true` if this carrier has an invite token.
    fn has_invite_token(&self) -> bool {
        self.invite_token().is_some()
    }
}

impl InviteTokenCarrier for Message {
    fn invite_token(&self) -> Option<InviteToken> {
        extract_invite_from_message(self)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is an invite token element.
pub fn is_invite_element(elem: &Element) -> bool {
    elem.ns() == NS_MUC_TOKEN_INVITE && matches!(elem.name(), "invite" | "request")
}

/// Check if an IQ is an invite token request.
pub fn is_invite_request(iq: &Iq) -> bool {
    matches!(iq, Iq::Set { payload: elem, .. } if elem.name() == "request" && elem.ns() == NS_MUC_TOKEN_INVITE)
}

/// Check if a message contains an invite token.
pub fn has_invite_in_message(msg: &Message) -> bool {
    msg.payloads
        .iter()
        .any(|e| e.ns() == NS_MUC_TOKEN_INVITE && e.name() == "invite")
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract an invite token from an IQ result.
pub fn extract_invite_from_iq(iq: &Iq) -> Option<InviteToken> {
    let elem = match iq {
        Iq::Result {
            payload: Some(elem),
            ..
        } if elem.name() == "invite" && elem.ns() == NS_MUC_TOKEN_INVITE => elem,
        _ => return None,
    };

    let token = elem.attr("token").filter(|t| !t.is_empty())?.to_owned();
    let room_jid = iq.from().map(|j| j.to_string());

    Some(InviteToken { token, room_jid })
}

/// Extract an invite token from a message.
pub fn extract_invite_from_message(msg: &Message) -> Option<InviteToken> {
    let elem = msg
        .payloads
        .iter()
        .find(|e| e.ns() == NS_MUC_TOKEN_INVITE && e.name() == "invite")?;

    let token = elem.attr("token").filter(|t| !t.is_empty())?.to_owned();
    let room_jid = elem
        .attr("jid")
        .filter(|j| !j.is_empty())
        .map(|j| j.to_owned());

    Some(InviteToken { token, room_jid })
}

// ── Building ─────────────────────────────────────────────────────────

/// Build an invite token request IQ.
pub fn build_invite_request(to_room: jid::Jid, id: &str) -> Iq {
    let request_elem = Element::builder("request", NS_MUC_TOKEN_INVITE).build();
    Iq::Set {
        from: None,
        to: Some(to_room),
        id: id.to_owned(),
        payload: request_elem,
    }
}

/// Build an invite token response IQ.
pub fn build_invite_response(original_iq: &Iq, token: &str) -> Iq {
    let invite_elem = Element::builder("invite", NS_MUC_TOKEN_INVITE)
        .attr(minidom::rxml::xml_ncname!("token").to_owned(), token)
        .build();
    Iq::Result {
        from: original_iq.to().cloned(),
        to: original_iq.from().cloned(),
        id: original_iq.id().to_string(),
        payload: Some(invite_elem),
    }
}

/// Build an invite element for inclusion in a message.
pub fn build_invite_message_element(token: &str, room_jid: &str) -> Element {
    Element::builder("invite", NS_MUC_TOKEN_INVITE)
        .attr(minidom::rxml::xml_ncname!("token").to_owned(), token)
        .attr(minidom::rxml::xml_ncname!("jid").to_owned(), room_jid)
        .build()
}

/// Build a complete invite message to share with another user.
pub fn build_invite_share_message(
    to: jid::Jid,
    from: impl Into<Option<jid::Jid>>,
    token: &InviteToken,
) -> Option<Message> {
    let room_jid = token.room_jid.as_ref()?;
    let uri = token.to_uri()?;

    let mut msg = Message::new(Some(to));
    msg.from = from.into();
    msg.type_ = xmpp_parsers::message::MessageType::Chat;
    msg.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        format!("You've been invited to join: {uri}"),
    );
    msg.payloads
        .push(build_invite_message_element(&token.token, room_jid));
    Some(msg)
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Add an invite token to a message.
pub fn set_invite_on_message(msg: &mut Message, token: &str, room_jid: &str) {
    msg.payloads.retain(|e| e.ns() != NS_MUC_TOKEN_INVITE);
    msg.payloads
        .push(build_invite_message_element(token, room_jid));
}

/// Remove invite token from a message.
pub fn strip_invite_from_message(msg: &mut Message) {
    msg.payloads.retain(|e| e.ns() != NS_MUC_TOKEN_INVITE);
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::MessageType;

    #[test]
    fn test_is_invite_request() {
        let room: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
        let iq = build_invite_request(room, "inv-1");
        assert!(is_invite_request(&iq));
    }

    #[test]
    fn test_is_invite_request_false() {
        let elem = Element::builder("query", "jabber:iq:roster").build();
        let iq = Iq::Set {
            from: None,
            to: None,
            id: "inv-2".to_owned(),
            payload: elem,
        };
        assert!(!is_invite_request(&iq));
    }

    #[test]
    fn test_build_and_extract_iq() {
        let room: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
        let request = build_invite_request(room, "inv-1");
        let response = build_invite_response(&request, "abc123");

        let token = extract_invite_from_iq(&response).expect("has token");
        assert_eq!(token.token, "abc123");
        assert_eq!(token.room_jid.as_deref(), Some("room@muc.example.com"));
    }

    #[test]
    fn test_extract_invite_from_message() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <body>Join us!</body>\
                    <invite xmlns='urn:xmpp:muc-token-invite:0' token='xyz789' jid='room@muc.example.com'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let token = extract_invite_from_message(&msg).expect("has token");
        assert_eq!(token.token, "xyz789");
        assert_eq!(token.room_jid.as_deref(), Some("room@muc.example.com"));
    }

    #[test]
    fn test_extract_absent() {
        let msg = Message::new(None::<jid::Jid>);
        assert!(extract_invite_from_message(&msg).is_none());
    }

    #[test]
    fn test_invite_token_to_uri() {
        let token = InviteToken::new("abc123").with_room("room@muc.example.com");
        assert_eq!(
            token.to_uri(),
            Some("xmpp:room@muc.example.com?join;password=abc123".to_owned())
        );

        let no_room = InviteToken::new("abc123");
        assert_eq!(no_room.to_uri(), None);
    }

    #[test]
    fn test_build_invite_share_message() {
        let to: jid::Jid = "friend@example.com".parse().expect("valid jid");
        let token = InviteToken::new("tok123").with_room("room@muc.example.com");
        let msg =
            build_invite_share_message(to.clone(), None::<jid::Jid>, &token).expect("has room jid");

        assert_eq!(msg.to, Some(to));
        assert_eq!(msg.type_, MessageType::Chat);
        assert!(msg.bodies.values().next().is_some());
        assert!(has_invite_in_message(&msg));
    }

    #[test]
    fn test_set_invite_on_message() {
        let mut msg = Message::new(None::<jid::Jid>);
        set_invite_on_message(&mut msg, "tok1", "room@muc.example.com");
        assert!(has_invite_in_message(&msg));

        // Replace
        set_invite_on_message(&mut msg, "tok2", "room@muc.example.com");
        let token = extract_invite_from_message(&msg).expect("has token");
        assert_eq!(token.token, "tok2");
        assert_eq!(
            msg.payloads
                .iter()
                .filter(|e| e.ns() == NS_MUC_TOKEN_INVITE)
                .count(),
            1
        );
    }

    #[test]
    fn test_strip_invite() {
        let mut msg = Message::new(None::<jid::Jid>);
        set_invite_on_message(&mut msg, "tok1", "room@muc.example.com");
        strip_invite_from_message(&mut msg);
        assert!(!has_invite_in_message(&msg));
    }

    #[test]
    fn test_invite_token_carrier_trait() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <invite xmlns='urn:xmpp:muc-token-invite:0' token='trait-test' jid='room@muc'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(msg.has_invite_token());
        let token = msg.invite_token().expect("has token");
        assert_eq!(token.token, "trait-test");
    }

    #[test]
    fn test_invite_token_display() {
        let token = InviteToken::new("display-test");
        assert_eq!(token.to_string(), "display-test");
    }

    #[test]
    fn test_invite_token_builder() {
        let token = InviteToken::new("abc").with_room("room@muc.example.com");
        assert_eq!(token.token, "abc");
        assert_eq!(token.room_jid.as_deref(), Some("room@muc.example.com"));
    }
}
