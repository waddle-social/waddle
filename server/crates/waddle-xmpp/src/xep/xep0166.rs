//! XEP-0166 Jingle helpers and validation.

use minidom::Element;
use thiserror::Error;
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::jingle::{Action, Jingle};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

/// Namespace for XEP-0166 Jingle.
pub const NS_JINGLE: &str = xmpp_parsers::ns::JINGLE;
/// Namespace for XEP-0166 Jingle-specific stanza errors.
pub const NS_JINGLE_ERRORS: &str = "urn:xmpp:jingle:errors:1";

/// XEP-0166 action names accepted by the parser.
pub const JINGLE_ACTIONS: &[&str] = &[
    "content-accept",
    "content-add",
    "content-modify",
    "content-reject",
    "content-remove",
    "description-info",
    "security-info",
    "session-accept",
    "session-info",
    "session-initiate",
    "session-terminate",
    "transport-accept",
    "transport-info",
    "transport-reject",
    "transport-replace",
];

/// XEP-0166 Jingle-specific error conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JingleErrorCondition {
    OutOfOrder,
    TieBreak,
    UnknownSession,
    UnsupportedInfo,
    SecurityRequired,
}

impl JingleErrorCondition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OutOfOrder => "out-of-order",
            Self::TieBreak => "tie-break",
            Self::UnknownSession => "unknown-session",
            Self::UnsupportedInfo => "unsupported-info",
            Self::SecurityRequired => "security-required",
        }
    }
}

/// Validation errors for supported Jingle WebRTC gateway payloads.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum JingleValidationError {
    #[error("not a jingle iq")]
    WrongElement,
    #[error("invalid jingle payload")]
    InvalidPayload,
    #[error("action requires at least one content")]
    MissingContent,
    #[error("content is missing required description")]
    MissingDescription,
    #[error("content is missing required transport")]
    MissingTransport,
    #[error("unsupported application format namespace: {0}")]
    UnsupportedApplication(String),
    #[error("unsupported transport namespace: {0}")]
    UnsupportedTransport(String),
    #[error("ICE-UDP candidates require pwd and ufrag")]
    MissingIceCredentials,
}

/// Check if an IQ is a Jingle IQ-set.
pub fn is_jingle_iq(iq: &Iq) -> bool {
    matches!(&iq.payload, IqType::Set(elem) if elem.name() == "jingle" && elem.ns() == NS_JINGLE)
}

/// Parse a Jingle IQ payload.
pub fn parse_jingle_iq(iq: &Iq) -> Result<Jingle, JingleValidationError> {
    let elem = match &iq.payload {
        IqType::Set(elem) if elem.name() == "jingle" && elem.ns() == NS_JINGLE => elem.clone(),
        _ => return Err(JingleValidationError::WrongElement),
    };
    Jingle::try_from(elem).map_err(|_| JingleValidationError::InvalidPayload)
}

/// Validate the subset of Jingle that Waddle's WebRTC gateway accepts.
pub fn validate_webrtc_jingle(jingle: &Jingle) -> Result<(), JingleValidationError> {
    match jingle.action {
        Action::SessionTerminate | Action::SessionInfo => return Ok(()),
        Action::TransportInfo | Action::ContentRemove => {
            if jingle.contents.is_empty() {
                return Err(JingleValidationError::MissingContent);
            }
        }
        _ => {
            if jingle.contents.is_empty() {
                return Err(JingleValidationError::MissingContent);
            }
        }
    }

    for content in &jingle.contents {
        match jingle.action {
            Action::ContentRemove => continue,
            Action::TransportInfo => {
                if content.transport.is_none() {
                    return Err(JingleValidationError::MissingTransport);
                }
            }
            _ => {
                if content.description.is_none() {
                    return Err(JingleValidationError::MissingDescription);
                }
                if content.transport.is_none() {
                    return Err(JingleValidationError::MissingTransport);
                }
            }
        }

        if let Some(description) = &content.description {
            let elem: Element = description.clone().into();
            if elem.ns() != xmpp_parsers::ns::JINGLE_RTP {
                return Err(JingleValidationError::UnsupportedApplication(elem.ns()));
            }
        }

        if let Some(transport) = &content.transport {
            let elem: Element = transport.clone().into();
            if elem.ns() != xmpp_parsers::ns::JINGLE_ICE_UDP {
                return Err(JingleValidationError::UnsupportedTransport(elem.ns()));
            }
            let has_candidates = elem.children().any(|child| {
                child.name() == "candidate" && child.ns() == xmpp_parsers::ns::JINGLE_ICE_UDP
            });
            if has_candidates
                && (elem.attr("pwd").is_none_or(str::is_empty)
                    || elem.attr("ufrag").is_none_or(str::is_empty))
            {
                return Err(JingleValidationError::MissingIceCredentials);
            }
        }
    }

    Ok(())
}

/// Build an empty result IQ acknowledging a Jingle IQ-set.
pub fn build_jingle_ack(original_iq: &Iq) -> Iq {
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(None),
    }
}

/// Build an IQ error with an optional Jingle-specific condition.
pub fn build_jingle_error(
    original_iq: &Iq,
    error_type: ErrorType,
    condition: DefinedCondition,
    jingle_condition: Option<JingleErrorCondition>,
    text: &str,
) -> Iq {
    let mut error = StanzaError::new(error_type, condition, "en", text);
    error.other = jingle_condition
        .map(|condition| Element::builder(condition.as_str(), NS_JINGLE_ERRORS).build());
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::jingle::{Content, ContentId, Creator, Description, Transport};

    fn parse_iq(xml: &str) -> Iq {
        Iq::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid iq")
    }

    #[test]
    fn parses_valid_jingle_iq() {
        let iq = parse_iq(
            "<iq xmlns='jabber:client' type='set' id='j1'><jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='s1'/></iq>",
        );
        assert!(is_jingle_iq(&iq));
        let jingle = parse_jingle_iq(&iq).expect("parsed");
        assert_eq!(jingle.action, Action::SessionInitiate);
    }

    #[test]
    fn validates_rtp_ice_content() {
        let desc = Element::builder("description", xmpp_parsers::ns::JINGLE_RTP)
            .attr("media", "audio")
            .build();
        let transport = Element::builder("transport", xmpp_parsers::ns::JINGLE_ICE_UDP)
            .attr("ufrag", "u")
            .attr("pwd", "p")
            .build();
        let jingle = Jingle::new(
            Action::SessionInitiate,
            xmpp_parsers::jingle::SessionId("s1".into()),
        )
        .add_content(
            Content::new(Creator::Initiator, ContentId("audio".into()))
                .with_description(Description::Unknown(desc))
                .with_transport(Transport::Unknown(transport)),
        );
        assert_eq!(validate_webrtc_jingle(&jingle), Ok(()));
    }

    #[test]
    fn candidates_require_ice_credentials() {
        let desc = Element::builder("description", xmpp_parsers::ns::JINGLE_RTP)
            .attr("media", "audio")
            .build();
        let transport = Element::builder("transport", xmpp_parsers::ns::JINGLE_ICE_UDP)
            .append(
                Element::builder("candidate", xmpp_parsers::ns::JINGLE_ICE_UDP)
                    .attr("component", "1")
                    .attr("foundation", "1")
                    .attr("generation", "0")
                    .attr("id", "c1")
                    .attr("ip", "192.0.2.1")
                    .attr("port", "5000")
                    .attr("priority", "1")
                    .attr("protocol", "udp")
                    .attr("type", "host")
                    .build(),
            )
            .build();
        let jingle = Jingle::new(
            Action::TransportInfo,
            xmpp_parsers::jingle::SessionId("s1".into()),
        )
        .add_content(
            Content::new(Creator::Initiator, ContentId("audio".into()))
                .with_description(Description::Unknown(desc))
                .with_transport(Transport::Unknown(transport)),
        );
        assert_eq!(
            validate_webrtc_jingle(&jingle),
            Err(JingleValidationError::MissingIceCredentials)
        );
    }
}
