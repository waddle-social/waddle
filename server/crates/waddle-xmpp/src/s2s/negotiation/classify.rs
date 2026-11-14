//! Classification of received first-level elements.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use minidom::Element;

use crate::ns::{JABBER_CLIENT, JABBER_SERVER, JABBER_SERVER_DIALBACK, SASL, STREAM, TLS};
use crate::s2s::condition::{SaslFailureCondition, StreamErrorCondition};

use super::elements::EXTERNAL;

/// Stanza element names (RFC 6120 §8).
const STANZA_NAMES: [&str; 3] = ["message", "presence", "iq"];

/// A SASL mechanism named in `<auth/>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mechanism {
    External,
    /// Any other or missing mechanism; never reflected back.
    Unsupported,
}

/// SASL initial response or response data (RFC 6120 §6.4.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SaslPayload {
    /// No character data: no initial response was sent.
    Absent,
    /// `=`: a zero-length response.
    Empty,
    /// Decoded response bytes.
    Data(Vec<u8>),
}

/// A received first-level element, by protocol role.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TopLevel {
    /// `message`, `presence` or `iq` in `jabber:server`.
    Stanza(Element),
    /// A stanza qualified by `jabber:client`.
    ClientStanza,
    Features(Element),
    PeerStreamError(StreamErrorCondition),
    StartTls,
    Proceed,
    TlsFailure,
    Auth {
        mechanism: Mechanism,
        payload: Result<SaslPayload, SaslFailureCondition>,
    },
    Response(Result<SaslPayload, SaslFailureCondition>),
    Abort,
    Challenge,
    Success(Result<SaslPayload, SaslFailureCondition>),
    SaslFailure(SaslFailureCondition),
    /// Any element qualified by the dialback namespace.
    Dialback,
    /// Anything else, including unknown elements in known namespaces.
    Other,
}

/// Classify a first-level element received on an S2S stream.
///
/// `max_payload_bytes` bounds decoded SASL data; longer payloads classify as
/// `malformed-request` without being decoded.
pub(crate) fn classify(element: Element, max_payload_bytes: usize) -> TopLevel {
    let namespace = element.ns_ref();
    let name = element.name();
    if namespace == JABBER_SERVER && STANZA_NAMES.contains(&name) {
        return TopLevel::Stanza(element);
    }
    if namespace == JABBER_CLIENT && STANZA_NAMES.contains(&name) {
        return TopLevel::ClientStanza;
    }
    if namespace == STREAM {
        return match name {
            "features" => TopLevel::Features(element),
            "error" => TopLevel::PeerStreamError(StreamErrorCondition::from_peer_element(element)),
            _ => TopLevel::Other,
        };
    }
    if namespace == TLS {
        return match name {
            "starttls" => TopLevel::StartTls,
            "proceed" => TopLevel::Proceed,
            "failure" => TopLevel::TlsFailure,
            _ => TopLevel::Other,
        };
    }
    if namespace == SASL {
        return match name {
            "auth" => TopLevel::Auth {
                mechanism: match element.attr("mechanism") {
                    Some(EXTERNAL) => Mechanism::External,
                    _ => Mechanism::Unsupported,
                },
                payload: payload(&element, max_payload_bytes),
            },
            "response" => TopLevel::Response(payload(&element, max_payload_bytes)),
            "abort" => TopLevel::Abort,
            "challenge" => TopLevel::Challenge,
            "success" => TopLevel::Success(payload(&element, max_payload_bytes)),
            "failure" => TopLevel::SaslFailure(SaslFailureCondition::from_peer_element(element)),
            _ => TopLevel::Other,
        };
    }
    if namespace == JABBER_SERVER_DIALBACK {
        return TopLevel::Dialback;
    }
    TopLevel::Other
}

/// Decode SASL character data (RFC 6120 §6.3.5, §6.4.2).
fn payload(element: &Element, max_bytes: usize) -> Result<SaslPayload, SaslFailureCondition> {
    if element.children().next().is_some() {
        return Err(SaslFailureCondition::MalformedRequest);
    }
    let text = element.text();
    match text.as_str() {
        "" => Ok(SaslPayload::Absent),
        "=" => Ok(SaslPayload::Empty),
        encoded => {
            if encoded.len() > max_bytes.div_ceil(3).saturating_mul(4) {
                return Err(SaslFailureCondition::MalformedRequest);
            }
            let decoded = STANDARD
                .decode(encoded)
                .map_err(|_| SaslFailureCondition::IncorrectEncoding)?;
            if decoded.len() > max_bytes {
                return Err(SaslFailureCondition::MalformedRequest);
            }
            Ok(SaslPayload::Data(decoded))
        }
    }
}
