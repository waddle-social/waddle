//! XEP-0488: MUC Token Invite
//!
//! Provides invite tokens for MUC rooms. Tokens are requested, listed, and
//! revoked with IQ payloads in `urn:xmpp:muc-token-invite:0`; tokens are
//! shared through a `xmpp:room?join;password=TOKEN` URI.

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
    /// Optional token delay value advertised by the room.
    pub delay: Option<u32>,
    /// Optional token counter.
    pub counter: Option<u32>,
}

/// Optional constraints requested when issuing a MUC invite token.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct InviteTokenRequest {
    /// Optional requested token lifetime, in seconds.
    pub delay: Option<u32>,
    /// Optional requested remaining-use counter.
    pub counter: Option<u32>,
}

impl InviteTokenRequest {
    /// Create an unconstrained token issuance request.
    pub fn new() -> Self {
        Self::default()
    }

    /// Request a token lifetime in seconds.
    pub fn with_delay(mut self, delay: u32) -> Self {
        self.delay = Some(delay);
        self
    }

    /// Request a token use counter.
    pub fn with_counter(mut self, counter: u32) -> Self {
        self.counter = Some(counter);
        self
    }
}

impl InviteToken {
    /// Create a new invite token.
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            room_jid: None,
            delay: None,
            counter: None,
        }
    }

    /// Set the room JID.
    pub fn with_room(mut self, jid: impl Into<String>) -> Self {
        self.room_jid = Some(jid.into());
        self
    }

    /// Set the optional delay value.
    pub fn with_delay(mut self, delay: u32) -> Self {
        self.delay = Some(delay);
        self
    }

    /// Set the optional counter value.
    pub fn with_counter(mut self, counter: u32) -> Self {
        self.counter = Some(counter);
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

/// Trait for message carriers. XEP-0488 does not define a message payload, so
/// this always returns `None`.
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

/// Check if an element is one of the XEP-0488 payload elements.
pub fn is_invite_element(elem: &Element) -> bool {
    elem.ns() == NS_MUC_TOKEN_INVITE
        && matches!(
            elem.name(),
            "request" | "token" | "tokens" | "revoke" | "expired-token"
        )
}

/// Check if an IQ is an invite token request.
pub fn is_invite_request(iq: &Iq) -> bool {
    matches!(iq, Iq::Set { payload: elem, .. } if elem.is("request", NS_MUC_TOKEN_INVITE))
}

/// Check if an IQ is a token listing request.
pub fn is_tokens_request(iq: &Iq) -> bool {
    matches!(iq, Iq::Get { payload: elem, .. } if elem.is("tokens", NS_MUC_TOKEN_INVITE))
}

/// Check if an IQ is a token revocation request.
pub fn is_revoke_request(iq: &Iq) -> bool {
    matches!(iq, Iq::Set { payload: elem, .. } if elem.is("revoke", NS_MUC_TOKEN_INVITE))
}

/// XEP-0488 has no message-embedded token element.
pub fn has_invite_in_message(_msg: &Message) -> bool {
    false
}

/// Extract an invite token from an IQ result.
pub fn extract_invite_from_iq(iq: &Iq) -> Option<InviteToken> {
    let elem = match iq {
        Iq::Result {
            payload: Some(elem),
            ..
        } if elem.is("token", NS_MUC_TOKEN_INVITE) => elem,
        _ => return None,
    };

    parse_token_element(elem, iq.from().map(ToString::to_string))
}

/// Extract optional issuance constraints from an invite token request.
pub fn extract_invite_request_from_iq(iq: &Iq) -> Option<InviteTokenRequest> {
    let elem = match iq {
        Iq::Set { payload: elem, .. } if elem.is("request", NS_MUC_TOKEN_INVITE) => elem,
        _ => return None,
    };
    Some(InviteTokenRequest {
        delay: elem.attr("delay").and_then(|raw| raw.parse().ok()),
        counter: elem.attr("counter").and_then(|raw| raw.parse().ok()),
    })
}

/// XEP-0488 has no message-embedded token element.
pub fn extract_invite_from_message(_msg: &Message) -> Option<InviteToken> {
    None
}

/// Extract the token string from a revoke request.
pub fn extract_revoke_from_iq(iq: &Iq) -> Option<InviteToken> {
    let elem = match iq {
        Iq::Set { payload: elem, .. } if elem.is("revoke", NS_MUC_TOKEN_INVITE) => elem,
        _ => return None,
    };
    let token = elem.text().trim().to_owned();
    (!token.is_empty()).then(|| InviteToken::new(token).with_room_iq_to(iq))
}

/// Extract listed tokens from an IQ result.
pub fn extract_tokens_from_iq(iq: &Iq) -> Vec<InviteToken> {
    let elem = match iq {
        Iq::Result {
            payload: Some(elem),
            ..
        } if elem.is("tokens", NS_MUC_TOKEN_INVITE) => elem,
        _ => return Vec::new(),
    };
    let room = iq.from().map(ToString::to_string);
    elem.children()
        .filter(|child| child.is("token", NS_MUC_TOKEN_INVITE))
        .filter_map(|child| parse_token_element(child, room.clone()))
        .collect()
}

fn parse_token_element(elem: &Element, room_jid: Option<String>) -> Option<InviteToken> {
    let token = elem.text().trim().to_owned();
    if token.is_empty() {
        return None;
    }
    let mut token = InviteToken::new(token);
    token.room_jid = room_jid;
    token.delay = elem.attr("delay").and_then(|raw| raw.parse().ok());
    token.counter = elem.attr("counter").and_then(|raw| raw.parse().ok());
    Some(token)
}

trait WithRoomFromIqTo {
    fn with_room_iq_to(self, iq: &Iq) -> Self;
}

impl WithRoomFromIqTo for InviteToken {
    fn with_room_iq_to(mut self, iq: &Iq) -> Self {
        self.room_jid = iq.to().map(ToString::to_string);
        self
    }
}

/// Build an invite token request IQ.
pub fn build_invite_request(to_room: jid::Jid, id: &str) -> Iq {
    build_invite_request_with_constraints(to_room, id, &InviteTokenRequest::new())
}

/// Build an invite token request IQ with optional constraints.
pub fn build_invite_request_with_constraints(
    to_room: jid::Jid,
    id: &str,
    request: &InviteTokenRequest,
) -> Iq {
    Iq::Set {
        from: None,
        to: Some(to_room),
        id: id.to_owned(),
        payload: build_request_element(request),
    }
}

/// Build an invite token response IQ.
pub fn build_invite_response(original_iq: &Iq, token: &str) -> Iq {
    build_invite_response_from_token(original_iq, &InviteToken::new(token))
}

/// Build an invite token response IQ from the applied token state.
pub fn build_invite_response_from_token(original_iq: &Iq, token: &InviteToken) -> Iq {
    Iq::Result {
        from: original_iq.to().cloned(),
        to: original_iq.from().cloned(),
        id: original_iq.id().to_string(),
        payload: Some(build_token_element(token)),
    }
}

/// Build a token listing request IQ.
pub fn build_tokens_request(to_room: jid::Jid, id: &str) -> Iq {
    Iq::Get {
        from: None,
        to: Some(to_room),
        id: id.to_owned(),
        payload: Element::builder("tokens", NS_MUC_TOKEN_INVITE).build(),
    }
}

/// Build a token listing response IQ.
pub fn build_tokens_response(original_iq: &Iq, tokens: &[InviteToken]) -> Iq {
    let mut tokens_elem = Element::builder("tokens", NS_MUC_TOKEN_INVITE);
    for token in tokens {
        tokens_elem = tokens_elem.append(build_token_element(token));
    }
    Iq::Result {
        from: original_iq.to().cloned(),
        to: original_iq.from().cloned(),
        id: original_iq.id().to_string(),
        payload: Some(tokens_elem.build()),
    }
}

fn build_request_element(request: &InviteTokenRequest) -> Element {
    let mut request_elem = Element::builder("request", NS_MUC_TOKEN_INVITE);
    if let Some(delay) = request.delay {
        request_elem = request_elem.attr(
            minidom::rxml::xml_ncname!("delay").to_owned(),
            delay.to_string(),
        );
    }
    if let Some(counter) = request.counter {
        request_elem = request_elem.attr(
            minidom::rxml::xml_ncname!("counter").to_owned(),
            counter.to_string(),
        );
    }
    request_elem.build()
}

fn build_token_element(token: &InviteToken) -> Element {
    let mut token_elem = Element::builder("token", NS_MUC_TOKEN_INVITE);
    if let Some(delay) = token.delay {
        token_elem = token_elem.attr(
            minidom::rxml::xml_ncname!("delay").to_owned(),
            delay.to_string(),
        );
    }
    if let Some(counter) = token.counter {
        token_elem = token_elem.attr(
            minidom::rxml::xml_ncname!("counter").to_owned(),
            counter.to_string(),
        );
    }
    token_elem.append(token.token.as_str()).build()
}

/// Build a token revocation request IQ.
pub fn build_revoke_request(to_room: jid::Jid, token: &str, id: &str) -> Iq {
    Iq::Set {
        from: None,
        to: Some(to_room),
        id: id.to_owned(),
        payload: Element::builder("revoke", NS_MUC_TOKEN_INVITE)
            .append(token)
            .build(),
    }
}

/// Build an empty token revocation success response.
pub fn build_revoke_response(original_iq: &Iq) -> Iq {
    Iq::Result {
        from: original_iq.to().cloned(),
        to: original_iq.from().cloned(),
        id: original_iq.id().to_string(),
        payload: None,
    }
}

/// Build a complete invite message to share with another user via URI.
pub fn build_invite_share_message(
    to: jid::Jid,
    from: impl Into<Option<jid::Jid>>,
    token: &InviteToken,
) -> Option<Message> {
    let uri = token.to_uri()?;

    let mut msg = Message::new(Some(to));
    msg.from = from.into();
    msg.type_ = xmpp_parsers::message::MessageType::Chat;
    msg.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        format!("You've been invited to join: {uri}"),
    );
    Some(msg)
}

/// Remove any legacy non-conformant invite-token payload from a message.
pub fn strip_invite_from_message(msg: &mut Message) {
    msg.payloads.retain(|e| e.ns() != NS_MUC_TOKEN_INVITE);
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::MessageType;

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
    fn test_tokens_listing() {
        let room: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
        let request = build_tokens_request(room, "tok-1");
        assert!(is_tokens_request(&request));

        let response = build_tokens_response(
            &request,
            &[
                InviteToken::new("one").with_counter(1),
                InviteToken::new("two").with_delay(60),
            ],
        );
        let parsed = extract_tokens_from_iq(&response);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].token, "one");
        assert_eq!(parsed[0].counter, Some(1));
        assert_eq!(parsed[1].delay, Some(60));
    }

    #[test]
    fn test_revoke_request() {
        let room: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
        let revoke = build_revoke_request(room, "tok", "rev-1");
        assert!(is_revoke_request(&revoke));
        assert_eq!(
            extract_revoke_from_iq(&revoke).expect("revoke token").token,
            "tok"
        );
        assert!(matches!(
            build_revoke_response(&revoke),
            Iq::Result { payload: None, .. }
        ));
    }

    #[test]
    fn test_invite_token_to_uri() {
        let token = InviteToken::new("abc123").with_room("room@muc.example.com");
        assert_eq!(
            token.to_uri(),
            Some("xmpp:room@muc.example.com?join;password=abc123".to_owned())
        );
    }

    #[test]
    fn test_build_invite_share_message_has_no_payload() {
        let to: jid::Jid = "friend@example.com".parse().expect("valid jid");
        let token = InviteToken::new("tok123").with_room("room@muc.example.com");
        let msg =
            build_invite_share_message(to.clone(), None::<jid::Jid>, &token).expect("has room jid");

        assert_eq!(msg.to, Some(to));
        assert_eq!(msg.type_, MessageType::Chat);
        assert!(msg.bodies.values().next().is_some());
        assert!(msg.payloads.is_empty());
        assert!(!has_invite_in_message(&msg));
    }
}
