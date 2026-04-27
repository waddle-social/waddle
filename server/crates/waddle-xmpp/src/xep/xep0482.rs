//! XEP-0482: Call Invites.
//!
//! The call invite payload is ordinary message content.  Join methods are
//! modelled as typed values, while unknown extension methods are preserved as
//! XML elements so callers can route/archive the original stanza without
//! losing data.

use minidom::Element;
use std::fmt;
use thiserror::Error;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0482 Call Invites.
pub const NS_CALL_INVITES: &str = "urn:xmpp:call-invites:0";

/// Jingle join method element name.
pub const CALL_INVITE_JINGLE: &str = "jingle";

/// External join method element name.
pub const CALL_INVITE_EXTERNAL: &str = "external";

/// Errors that can occur when parsing call invite payloads.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CallInviteError {
    #[error("not a call invite payload")]
    WrongElement,
    #[error("missing required attribute: {0}")]
    MissingAttribute(&'static str),
    #[error("invalid boolean attribute: {0}")]
    InvalidBoolean(&'static str),
    #[error("invalid JID attribute: {0}")]
    InvalidJid(String),
    #[error("invalid URI attribute: {0}")]
    InvalidUri(String),
    #[error("missing join method")]
    MissingJoinMethod,
    #[error("too many join methods")]
    TooManyJoinMethods,
}

/// XEP-0166 Jingle session identifier carried by a XEP-0482 Jingle join method.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JingleSessionId(String);

impl JingleSessionId {
    pub fn new(value: impl Into<String>) -> Result<Self, CallInviteError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(CallInviteError::MissingAttribute("sid"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for JingleSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// XEP-0482 invite message identifier used by lifecycle payloads.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallInviteId(String);

impl CallInviteId {
    pub fn new(value: impl Into<String>) -> Result<Self, CallInviteError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(CallInviteError::MissingAttribute("id"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CallInviteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A concrete method by which a client can join a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinMethod {
    /// XEP-0166 Jingle session join method.
    Jingle {
        /// Jingle session id.
        sid: JingleSessionId,
        /// Optional component/full JID to address for the Jingle session.
        jid: Option<jid::Jid>,
    },
    /// External URI join method.
    External {
        /// URI to open.
        uri: url::Url,
    },
    /// Unknown extension method preserved as XML.
    Unknown(Element),
}

impl JoinMethod {
    /// Return the Jingle session id for this method when it is a Jingle join.
    pub fn jingle_sid(&self) -> Option<&JingleSessionId> {
        match self {
            Self::Jingle { sid, .. } => Some(sid),
            _ => None,
        }
    }

    /// Return the Jingle target JID for this method when one is present.
    pub fn jingle_jid(&self) -> Option<&jid::Jid> {
        match self {
            Self::Jingle { jid, .. } => jid.as_ref(),
            _ => None,
        }
    }
}

/// XEP-0482 call invite payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallInvite {
    /// Whether this invite includes audio intent. Defaults to `true`.
    pub audio: bool,
    /// Whether this invite includes video intent. Defaults to `false`.
    pub video: bool,
    /// Advertised join methods.
    pub methods: Vec<JoinMethod>,
}

impl Default for CallInvite {
    fn default() -> Self {
        Self {
            audio: true,
            video: false,
            methods: Vec::new(),
        }
    }
}

impl CallInvite {
    /// Create an invite with default XEP-0482 media intent.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a join method.
    pub fn with_method(mut self, method: JoinMethod) -> Self {
        self.methods.push(method);
        self
    }

    /// Return the first Jingle join method, if any.
    pub fn first_jingle_method(&self) -> Option<&JoinMethod> {
        self.methods
            .iter()
            .find(|method| matches!(method, JoinMethod::Jingle { .. }))
    }
}

/// A non-invite lifecycle payload that references the invite id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallInviteReference {
    /// The invite message id. In MUCs this must be the room-assigned
    /// XEP-0359 stanza-id, not the client-generated message id.
    pub id: CallInviteId,
}

/// Parsed XEP-0482 message payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallInvitePayload {
    Invite(CallInvite),
    Retract(CallInviteReference),
    Accept {
        reference: CallInviteReference,
        method: JoinMethod,
    },
    Reject(CallInviteReference),
    Left(CallInviteReference),
}

impl CallInvitePayload {
    /// Return the referenced invite id for lifecycle payloads.
    pub fn reference_id(&self) -> Option<&CallInviteId> {
        match self {
            Self::Invite(_) => None,
            Self::Retract(reference)
            | Self::Reject(reference)
            | Self::Left(reference)
            | Self::Accept { reference, .. } => Some(&reference.id),
        }
    }
}

/// Check if an element is one of the XEP-0482 payload elements.
pub fn is_call_invite_element(elem: &Element) -> bool {
    elem.ns() == NS_CALL_INVITES
        && matches!(
            elem.name(),
            "invite" | "retract" | "accept" | "reject" | "left"
        )
}

/// Check if a message carries a XEP-0482 payload.
pub fn has_call_invite_payload(message: &Message) -> bool {
    message.payloads.iter().any(is_call_invite_element)
}

/// Extract the first XEP-0482 payload from a message.
pub fn extract_call_invite_payload(message: &Message) -> Option<CallInvitePayload> {
    try_extract_call_invite_payload(message).ok().flatten()
}

/// Extract the first XEP-0482 payload, preserving parse errors for malformed
/// official call-invite XML.
pub fn try_extract_call_invite_payload(
    message: &Message,
) -> Result<Option<CallInvitePayload>, CallInviteError> {
    message
        .payloads
        .iter()
        .find(|payload| is_call_invite_element(payload))
        .map(parse_call_invite_payload)
        .transpose()
}

/// Parse a XEP-0482 payload element.
pub fn parse_call_invite_payload(elem: &Element) -> Result<CallInvitePayload, CallInviteError> {
    if elem.ns() != NS_CALL_INVITES {
        return Err(CallInviteError::WrongElement);
    }

    match elem.name() {
        "invite" => {
            let audio = optional_bool(elem, "audio")?.unwrap_or(true);
            let video = optional_bool(elem, "video")?.unwrap_or(false);
            let methods = elem
                .children()
                .cloned()
                .map(parse_join_method)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CallInvitePayload::Invite(CallInvite {
                audio,
                video,
                methods,
            }))
        }
        "retract" => Ok(CallInvitePayload::Retract(parse_reference(elem)?)),
        "reject" => Ok(CallInvitePayload::Reject(parse_reference(elem)?)),
        "left" => Ok(CallInvitePayload::Left(parse_reference(elem)?)),
        "accept" => {
            let reference = parse_reference(elem)?;
            let mut methods = elem.children().cloned();
            let method = methods
                .next()
                .ok_or(CallInviteError::MissingJoinMethod)
                .and_then(parse_join_method)?;
            if methods.next().is_some() {
                return Err(CallInviteError::TooManyJoinMethods);
            }
            Ok(CallInvitePayload::Accept { reference, method })
        }
        _ => Err(CallInviteError::WrongElement),
    }
}

/// Build a XEP-0482 invite element.
pub fn build_invite_element(invite: &CallInvite) -> Element {
    let mut elem = Element::builder("invite", NS_CALL_INVITES).build();
    if !invite.audio {
        elem.set_attr("audio", "false");
    }
    if invite.video {
        elem.set_attr("video", "true");
    }
    for method in &invite.methods {
        elem.append_child(build_join_method_element(method));
    }
    elem
}

/// Build a `<retract/>` element.
pub fn build_retract_element(id: &CallInviteId) -> Element {
    build_reference_element("retract", id)
}

/// Build an `<accept/>` element.
pub fn build_accept_element(id: &CallInviteId, method: &JoinMethod) -> Element {
    let mut elem = build_reference_element("accept", id);
    elem.append_child(build_join_method_element(method));
    elem
}

/// Build a `<reject/>` element.
pub fn build_reject_element(id: &CallInviteId) -> Element {
    build_reference_element("reject", id)
}

/// Build a `<left/>` element.
pub fn build_left_element(id: &CallInviteId) -> Element {
    build_reference_element("left", id)
}

/// Build a Jingle join method element.
pub fn build_jingle_method(sid: &JingleSessionId, jid: Option<&jid::Jid>) -> Element {
    let mut elem = Element::builder(CALL_INVITE_JINGLE, NS_CALL_INVITES)
        .attr("sid", sid.as_str())
        .build();
    if let Some(jid) = jid {
        elem.set_attr("jid", jid.to_string());
    }
    elem
}

/// Build an external join method element.
pub fn build_external_method(uri: &url::Url) -> Element {
    Element::builder(CALL_INVITE_EXTERNAL, NS_CALL_INVITES)
        .attr("uri", uri.as_str())
        .build()
}

fn build_reference_element(name: &str, id: &CallInviteId) -> Element {
    Element::builder(name, NS_CALL_INVITES)
        .attr("id", id.as_str())
        .build()
}

fn build_join_method_element(method: &JoinMethod) -> Element {
    match method {
        JoinMethod::Jingle { sid, jid } => build_jingle_method(sid, jid.as_ref()),
        JoinMethod::External { uri } => build_external_method(uri),
        JoinMethod::Unknown(elem) => elem.clone(),
    }
}

fn parse_reference(elem: &Element) -> Result<CallInviteReference, CallInviteError> {
    let id = elem
        .attr("id")
        .filter(|id| !id.is_empty())
        .ok_or(CallInviteError::MissingAttribute("id"))?;
    Ok(CallInviteReference {
        id: CallInviteId::new(id.to_owned())?,
    })
}

fn parse_join_method(elem: Element) -> Result<JoinMethod, CallInviteError> {
    match (elem.name(), elem.ns().as_ref()) {
        (CALL_INVITE_JINGLE, NS_CALL_INVITES) => {
            let sid = elem
                .attr("sid")
                .filter(|sid| !sid.is_empty())
                .ok_or(CallInviteError::MissingAttribute("sid"))?
                .to_owned();
            let sid = JingleSessionId::new(sid)?;
            let jid = elem
                .attr("jid")
                .map(|value| {
                    value
                        .parse::<jid::Jid>()
                        .map_err(|_| CallInviteError::InvalidJid(value.to_owned()))
                })
                .transpose()?;
            Ok(JoinMethod::Jingle { sid, jid })
        }
        (CALL_INVITE_EXTERNAL, NS_CALL_INVITES) => {
            let uri = elem
                .attr("uri")
                .filter(|uri| !uri.is_empty())
                .ok_or(CallInviteError::MissingAttribute("uri"))?
                .parse::<url::Url>()
                .map_err(|_| {
                    CallInviteError::InvalidUri(elem.attr("uri").unwrap_or("").to_owned())
                })?;
            Ok(JoinMethod::External { uri })
        }
        _ => Ok(JoinMethod::Unknown(elem)),
    }
}

fn optional_bool(elem: &Element, attr: &'static str) -> Result<Option<bool>, CallInviteError> {
    match elem.attr(attr) {
        None => Ok(None),
        Some("true" | "1") => Ok(Some(true)),
        Some("false" | "0") => Ok(Some(false)),
        Some(_) => Err(CallInviteError::InvalidBoolean(attr)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_defaults_audio_true_video_false() {
        let elem: Element = "<invite xmlns='urn:xmpp:call-invites:0'><jingle sid='s1'/></invite>"
            .parse()
            .expect("valid call invite");
        let payload = parse_call_invite_payload(&elem).expect("parsed");
        match payload {
            CallInvitePayload::Invite(invite) => {
                assert!(invite.audio);
                assert!(!invite.video);
                assert_eq!(
                    invite.methods[0].jingle_sid().map(JingleSessionId::as_str),
                    Some("s1")
                );
            }
            _ => panic!("expected invite"),
        }
    }

    #[test]
    fn invite_builds_jingle_method_with_jid_attribute() {
        let jid: jid::Jid = "media.example/s1".parse().expect("valid jid");
        let invite = CallInvite::new().with_method(JoinMethod::Jingle {
            sid: JingleSessionId::new("s1").expect("sid"),
            jid: Some(jid),
        });
        let elem = build_invite_element(&invite);
        let method = elem.get_child("jingle", NS_CALL_INVITES).expect("jingle");
        assert_eq!(method.attr("sid"), Some("s1"));
        assert_eq!(method.attr("jid"), Some("media.example/s1"));
    }

    #[test]
    fn lifecycle_payloads_require_id() {
        let elem: Element = "<accept xmlns='urn:xmpp:call-invites:0' id='room-stanza-id'><jingle sid='s1'/></accept>"
            .parse()
            .expect("valid accept");
        let payload = parse_call_invite_payload(&elem).expect("parsed");
        assert_eq!(
            payload.reference_id().map(CallInviteId::as_str),
            Some("room-stanza-id")
        );

        let missing: Element = "<left xmlns='urn:xmpp:call-invites:0'/>"
            .parse()
            .expect("valid xml");
        assert_eq!(
            parse_call_invite_payload(&missing),
            Err(CallInviteError::MissingAttribute("id"))
        );
    }

    #[test]
    fn accept_requires_exactly_one_join_method() {
        let missing: Element = "<accept xmlns='urn:xmpp:call-invites:0' id='room-stanza-id'/>"
            .parse()
            .expect("valid xml");
        assert_eq!(
            parse_call_invite_payload(&missing),
            Err(CallInviteError::MissingJoinMethod)
        );

        let duplicate: Element = "<accept xmlns='urn:xmpp:call-invites:0' id='room-stanza-id'><jingle sid='s1'/><jingle sid='s2'/></accept>"
            .parse()
            .expect("valid xml");
        assert_eq!(
            parse_call_invite_payload(&duplicate),
            Err(CallInviteError::TooManyJoinMethods)
        );
    }

    #[test]
    fn message_detection_rejects_wrong_namespace() {
        let mut msg = Message::new(None);
        msg.payloads
            .push(Element::builder("invite", "urn:other").build());
        assert!(!has_call_invite_payload(&msg));
        msg.payloads.push(build_invite_element(&CallInvite::new()));
        assert!(has_call_invite_payload(&msg));
    }
}
