//! XEP-0166: Jingle
//!
//! Core Jingle framework subset for call/session negotiation.

use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};

/// Namespace for XEP-0166 Jingle.
pub const NS_JINGLE: &str = "urn:xmpp:jingle:1";

/// Jingle action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JingleAction {
    SessionInitiate,
    SessionAccept,
    SessionTerminate,
    SessionInfo,
    TransportInfo,
    ContentAdd,
    ContentRemove,
}

impl JingleAction {
    pub fn from_str_attr(value: &str) -> Option<Self> {
        match value {
            "session-initiate" => Some(Self::SessionInitiate),
            "session-accept" => Some(Self::SessionAccept),
            "session-terminate" => Some(Self::SessionTerminate),
            "session-info" => Some(Self::SessionInfo),
            "transport-info" => Some(Self::TransportInfo),
            "content-add" => Some(Self::ContentAdd),
            "content-remove" => Some(Self::ContentRemove),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionInitiate => "session-initiate",
            Self::SessionAccept => "session-accept",
            Self::SessionTerminate => "session-terminate",
            Self::SessionInfo => "session-info",
            Self::TransportInfo => "transport-info",
            Self::ContentAdd => "content-add",
            Self::ContentRemove => "content-remove",
        }
    }
}

/// Content creator role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentCreator {
    Initiator,
    Responder,
}

impl ContentCreator {
    pub fn from_str_attr(value: &str) -> Option<Self> {
        match value {
            "initiator" => Some(Self::Initiator),
            "responder" => Some(Self::Responder),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Initiator => "initiator",
            Self::Responder => "responder",
        }
    }
}

/// Content sender policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Senders {
    Both,
    Initiator,
    Responder,
    None,
}

impl Senders {
    pub fn from_str_attr(value: &str) -> Option<Self> {
        match value {
            "both" => Some(Self::Both),
            "initiator" => Some(Self::Initiator),
            "responder" => Some(Self::Responder),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::Initiator => "initiator",
            Self::Responder => "responder",
            Self::None => "none",
        }
    }
}

/// Session termination reason condition (subset).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ReasonCondition {
    Success,
    Decline,
    Busy,
    Cancel,
    ConnectivityError,
    FailedApplication,
    FailedTransport,
    GeneralError,
    MediaError,
    SecurityError,
    Timeout,
    UnsupportedApplications,
    UnsupportedTransports,
    AlternativeSession(String),
    Other(String),
}

impl ReasonCondition {
    fn from_reason_element(elem: &Element) -> Self {
        match elem.name() {
            "success" => Self::Success,
            "decline" => Self::Decline,
            "busy" => Self::Busy,
            "cancel" => Self::Cancel,
            "connectivity-error" => Self::ConnectivityError,
            "failed-application" => Self::FailedApplication,
            "failed-transport" => Self::FailedTransport,
            "general-error" => Self::GeneralError,
            "media-error" => Self::MediaError,
            "security-error" => Self::SecurityError,
            "timeout" => Self::Timeout,
            "unsupported-applications" => Self::UnsupportedApplications,
            "unsupported-transports" => Self::UnsupportedTransports,
            "alternative-session" => Self::AlternativeSession(elem.text().trim().to_owned()),
            other => Self::Other(other.to_owned()),
        }
    }

    fn to_element(&self) -> Element {
        match self {
            Self::Success => Element::builder("success", NS_JINGLE).build(),
            Self::Decline => Element::builder("decline", NS_JINGLE).build(),
            Self::Busy => Element::builder("busy", NS_JINGLE).build(),
            Self::Cancel => Element::builder("cancel", NS_JINGLE).build(),
            Self::ConnectivityError => Element::builder("connectivity-error", NS_JINGLE).build(),
            Self::FailedApplication => Element::builder("failed-application", NS_JINGLE).build(),
            Self::FailedTransport => Element::builder("failed-transport", NS_JINGLE).build(),
            Self::GeneralError => Element::builder("general-error", NS_JINGLE).build(),
            Self::MediaError => Element::builder("media-error", NS_JINGLE).build(),
            Self::SecurityError => Element::builder("security-error", NS_JINGLE).build(),
            Self::Timeout => Element::builder("timeout", NS_JINGLE).build(),
            Self::UnsupportedApplications => {
                Element::builder("unsupported-applications", NS_JINGLE).build()
            }
            Self::UnsupportedTransports => {
                Element::builder("unsupported-transports", NS_JINGLE).build()
            }
            Self::AlternativeSession(sid) => Element::builder("alternative-session", NS_JINGLE)
                .append(sid.as_str())
                .build(),
            Self::Other(name) => Element::builder(name.as_str(), NS_JINGLE).build(),
        }
    }
}

/// Session reason payload.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JingleReason {
    pub condition: ReasonCondition,
    pub text: Option<String>,
}

/// Jingle content payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JingleContent {
    pub creator: ContentCreator,
    pub name: String,
    pub senders: Option<Senders>,
    pub description: Option<Element>,
    pub transport: Option<Element>,
}

/// Jingle stanza payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jingle {
    pub action: JingleAction,
    pub sid: String,
    pub initiator: Option<String>,
    pub responder: Option<String>,
    pub contents: Vec<JingleContent>,
    pub reason: Option<JingleReason>,
}

impl Jingle {
    /// Create a Jingle stanza with required action and session id.
    pub fn new(action: JingleAction, sid: impl Into<String>) -> Self {
        Self {
            action,
            sid: sid.into(),
            initiator: None,
            responder: None,
            contents: Vec::new(),
            reason: None,
        }
    }

    /// Add a content entry.
    pub fn with_content(mut self, content: JingleContent) -> Self {
        self.contents.push(content);
        self
    }
}

/// Check whether an element is `<jingle xmlns='urn:xmpp:jingle:1'/>`.
pub fn is_jingle_element(elem: &Element) -> bool {
    elem.name() == "jingle" && elem.ns() == NS_JINGLE
}

/// Check whether an IQ contains Jingle payload.
pub fn is_jingle_iq(iq: &Iq) -> bool {
    matches!(
        &iq.payload,
        IqType::Set(elem) | IqType::Get(elem) if is_jingle_element(elem)
    )
}

/// Parse Jingle content element.
pub fn parse_jingle_content_element(elem: &Element) -> Option<JingleContent> {
    if elem.name() != "content" || elem.ns() != NS_JINGLE {
        return None;
    }

    let creator = ContentCreator::from_str_attr(elem.attr("creator")?)?;
    let name = elem.attr("name").filter(|v| !v.is_empty())?.to_owned();
    let senders = elem.attr("senders").and_then(Senders::from_str_attr);

    let description = elem
        .children()
        .find(|child| child.name() == "description")
        .cloned();

    let transport = elem
        .children()
        .find(|child| child.name() == "transport")
        .cloned();

    Some(JingleContent {
        creator,
        name,
        senders,
        description,
        transport,
    })
}

/// Build Jingle content element.
pub fn build_jingle_content_element(content: &JingleContent) -> Element {
    let mut elem = Element::builder("content", NS_JINGLE)
        .attr("creator", content.creator.as_str())
        .attr("name", content.name.as_str())
        .build();

    if let Some(senders) = content.senders {
        elem.set_attr("senders", senders.as_str());
    }

    if let Some(ref description) = content.description {
        elem.append_child(description.clone());
    }

    if let Some(ref transport) = content.transport {
        elem.append_child(transport.clone());
    }

    elem
}

/// Parse Jingle reason element.
pub fn parse_jingle_reason_element(elem: &Element) -> Option<JingleReason> {
    if elem.name() != "reason" || elem.ns() != NS_JINGLE {
        return None;
    }

    let condition_elem = elem
        .children()
        .find(|child| child.ns() == NS_JINGLE && child.name() != "text")?;
    let condition = ReasonCondition::from_reason_element(condition_elem);
    let text = elem
        .get_child("text", NS_JINGLE)
        .map(|text_elem| text_elem.text())
        .filter(|value| !value.is_empty());

    Some(JingleReason { condition, text })
}

/// Build Jingle reason element.
pub fn build_jingle_reason_element(reason: &JingleReason) -> Element {
    let mut elem = Element::builder("reason", NS_JINGLE).build();
    elem.append_child(reason.condition.to_element());

    if let Some(ref text) = reason.text {
        elem.append_child(
            Element::builder("text", NS_JINGLE)
                .append(text.as_str())
                .build(),
        );
    }

    elem
}

/// Parse Jingle payload element.
pub fn parse_jingle_element(elem: &Element) -> Option<Jingle> {
    if !is_jingle_element(elem) {
        return None;
    }

    let action = JingleAction::from_str_attr(elem.attr("action")?)?;
    let sid = elem.attr("sid").filter(|v| !v.is_empty())?.to_owned();
    let initiator = elem
        .attr("initiator")
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let responder = elem
        .attr("responder")
        .filter(|value| !value.is_empty())
        .map(str::to_owned);

    let contents = elem
        .children()
        .filter(|child| child.name() == "content" && child.ns() == NS_JINGLE)
        .filter_map(parse_jingle_content_element)
        .collect();

    let reason = elem
        .children()
        .find(|child| child.name() == "reason" && child.ns() == NS_JINGLE)
        .and_then(parse_jingle_reason_element);

    Some(Jingle {
        action,
        sid,
        initiator,
        responder,
        contents,
        reason,
    })
}

/// Parse Jingle payload from IQ stanza.
pub fn parse_jingle_iq(iq: &Iq) -> Option<Jingle> {
    match &iq.payload {
        IqType::Set(elem) | IqType::Get(elem) => parse_jingle_element(elem),
        _ => None,
    }
}

/// Build Jingle payload element.
pub fn build_jingle_element(jingle: &Jingle) -> Element {
    let mut elem = Element::builder("jingle", NS_JINGLE)
        .attr("action", jingle.action.as_str())
        .attr("sid", jingle.sid.as_str())
        .build();

    if let Some(ref initiator) = jingle.initiator {
        elem.set_attr("initiator", initiator);
    }

    if let Some(ref responder) = jingle.responder {
        elem.set_attr("responder", responder);
    }

    for content in &jingle.contents {
        elem.append_child(build_jingle_content_element(content));
    }

    if let Some(ref reason) = jingle.reason {
        elem.append_child(build_jingle_reason_element(reason));
    }

    elem
}

/// Build IQ result response for an incoming Jingle IQ.
pub fn build_jingle_result(iq: &Iq) -> Iq {
    Iq {
        from: iq.to.clone(),
        to: iq.from.clone(),
        id: iq.id.clone(),
        payload: IqType::Result(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xep::xep0167::{build_rtp_description_element, MediaType, RtpDescription};
    use crate::xep::xep0176::{build_ice_udp_transport_element, IceUdpTransport};

    #[test]
    fn test_parse_jingle_session_initiate() {
        let xml = "<iq xmlns='jabber:client' type='set' id='j1'>\
                     <jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='sid-1' initiator='alice@example.com'>\
                       <content creator='initiator' name='audio'>\
                         <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>\
                         <transport xmlns='urn:xmpp:jingle:transports:ice-udp:1'/>\
                       </content>\
                     </jingle>\
                   </iq>";

        let iq = Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid iq");
        let parsed = parse_jingle_iq(&iq).expect("jingle should parse");

        assert_eq!(parsed.action, JingleAction::SessionInitiate);
        assert_eq!(parsed.sid, "sid-1");
        assert_eq!(parsed.contents.len(), 1);
        assert_eq!(parsed.contents[0].name, "audio");
    }

    #[test]
    fn test_parse_jingle_reason() {
        let reason = Element::builder("reason", NS_JINGLE)
            .append(Element::builder("decline", NS_JINGLE).build())
            .append(
                Element::builder("text", NS_JINGLE)
                    .append("not now")
                    .build(),
            )
            .build();

        let parsed = parse_jingle_reason_element(&reason).expect("reason should parse");
        assert_eq!(parsed.condition, ReasonCondition::Decline);
        assert_eq!(parsed.text.as_deref(), Some("not now"));
    }

    #[test]
    fn test_build_jingle_roundtrip() {
        let content = JingleContent {
            creator: ContentCreator::Initiator,
            name: "audio".to_owned(),
            senders: Some(Senders::Both),
            description: Some(build_rtp_description_element(&RtpDescription::new(
                MediaType::Audio,
            ))),
            transport: Some(build_ice_udp_transport_element(&IceUdpTransport::new())),
        };

        let jingle = Jingle::new(JingleAction::SessionInitiate, "sid-123").with_content(content);
        let elem = build_jingle_element(&jingle);
        assert!(is_jingle_element(&elem));

        let reparsed = parse_jingle_element(&elem).expect("jingle should parse");
        assert_eq!(reparsed.action, JingleAction::SessionInitiate);
        assert_eq!(reparsed.sid, "sid-123");
        assert_eq!(reparsed.contents.len(), 1);
    }

    #[test]
    fn test_parse_jingle_session_info() {
        let xml = "<iq xmlns='jabber:client' type='set' id='j2'>\
                     <jingle xmlns='urn:xmpp:jingle:1' action='session-info' sid='sid-2'/>\
                   </iq>";
        let iq = Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid iq");
        let parsed = parse_jingle_iq(&iq).expect("jingle should parse");
        assert_eq!(parsed.action, JingleAction::SessionInfo);
        assert_eq!(parsed.action.as_str(), "session-info");
    }
}
