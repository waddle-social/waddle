use std::time::Duration;

use minidom::Element;
use thiserror::Error;

use crate::bootstrap::{RequiredStreamFeature, SaslFailureCondition};
use crate::request::{RequestId, StanzaId};
use crate::state::{ClientState, SessionPhase};

pub type ClientResult<T> = Result<T, ClientError>;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error(transparent)]
    Core(#[from] waddle_xmpp_core::CoreError),
    #[error("invalid websocket endpoint scheme `{scheme}`; expected `ws` or `wss`")]
    InvalidTransportScheme { scheme: String },
    #[error("websocket endpoint must include a host")]
    MissingWebSocketHost,
    #[error("client resource must not be empty")]
    EmptyResource,
    #[error("stanza id must not be empty")]
    EmptyStanzaId,
    #[error("request correlation ids are exhausted")]
    RequestIdExhausted,
    #[error("request {request_id} is already pending")]
    DuplicateRequest { request_id: RequestId },
    #[error("stanza id `{stanza_id}` is already pending")]
    DuplicateStanzaCorrelation { stanza_id: StanzaId },
    #[error("request {request_id} is not pending")]
    UnknownRequest { request_id: RequestId },
    #[error("stanza id `{stanza_id}` is not pending")]
    UnknownStanzaCorrelation { stanza_id: StanzaId },
    #[error("unsupported session transition from {from:?} to {to:?}")]
    InvalidPhaseTransition {
        from: SessionPhase,
        to: SessionPhase,
    },
    #[error("unsupported client-state transition from {from:?} to {to:?}")]
    InvalidStateTransition { from: ClientState, to: ClientState },
    #[error("server did not advertise required stream feature {feature}")]
    MissingStreamFeature { feature: RequiredStreamFeature },
    #[error("received invalid stream features payload")]
    InvalidStreamFeatures,
    #[error("received invalid SASL failure payload")]
    InvalidSaslFailure,
    #[error("received invalid bind response payload")]
    InvalidBindResponse,
    #[error("server rejected SASL authentication with condition `{condition}`")]
    AuthenticationRejected { condition: SaslFailureCondition },
    #[error("websocket transport request could not be built")]
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    InvalidWebSocketRequest(#[source] tokio_tungstenite::tungstenite::http::Error),
    #[error("websocket connection timed out after {timeout:?}")]
    WebSocketConnectTimeout { timeout: Duration },
    #[error("websocket transport failed")]
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    WebSocket(#[source] Box<tokio_tungstenite::tungstenite::Error>),
    #[error("websocket transport received an empty XMPP frame")]
    EmptyTransportFrame,
    #[error("websocket transport frame exceeds the maximum size of {max} bytes")]
    TransportFrameTooLarge { max: usize },
    #[error("websocket transport received malformed XMPP framing")]
    InvalidTransportFrame,
    #[error("stream open frame contains an invalid `to` JID")]
    InvalidStreamOpenTo,
    #[error("stream open frame contains an invalid `from` JID")]
    InvalidStreamOpenFrom,
    #[error("stream open frame uses unsupported version `{version}`")]
    UnsupportedStreamVersion { version: String },
    #[error("websocket transport received an unsupported message type")]
    UnsupportedWebSocketMessage,
    #[error("websocket transport is already closed")]
    TransportClosed,
    #[error("the XMPP session is disconnected")]
    Disconnected,
    #[error("server returned a stanza error: {0}")]
    StanzaError(#[from] StanzaError),
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
impl From<tokio_tungstenite::tungstenite::Error> for ClientError {
    fn from(error: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::WebSocket(Box::new(error))
    }
}

/// XMPP stanza-level error as defined in RFC 6120 §8.3.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{error_type:?}: {condition} ({text:?})")]
pub struct StanzaError {
    /// The `type` attribute of the `<error/>` element.
    pub error_type: StanzaErrorType,
    /// The error-condition element name (e.g. `"not-found"`, `"forbidden"`).
    pub condition: String,
    /// Optional human-readable `<text/>` content.
    pub text: Option<String>,
}

/// Values of the `type` attribute on an XMPP `<error/>` element (RFC 6120 §8.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StanzaErrorType {
    Auth,
    Cancel,
    Continue,
    Modify,
    Wait,
    /// An unrecognised type string was received.
    Unknown,
}

/// Parse a stanza-level error from an IQ element returned with `type="error"`.
///
/// Looks for a child `<error/>` element, extracts `type`, the RFC 6120 condition
/// child (in the `urn:ietf:params:xml:ns:xmpp-stanzas` namespace), and optional
/// `<text/>` content.  Returns a best-effort `StanzaError` even if parts are missing.
pub fn parse_stanza_error(element: &Element) -> StanzaError {
    const NS_STANZAS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";
    const NS_CLIENT_ALT: &str = "jabber:client";

    let ns = element.ns();
    let error_el = element
        .get_child("error", ns.as_str())
        .or_else(|| element.get_child("error", NS_CLIENT_ALT))
        .or_else(|| element.get_child("error", ""));

    let error_type = error_el
        .and_then(|e| e.attr("type"))
        .map(|t| match t {
            "auth" => StanzaErrorType::Auth,
            "cancel" => StanzaErrorType::Cancel,
            "continue" => StanzaErrorType::Continue,
            "modify" => StanzaErrorType::Modify,
            "wait" => StanzaErrorType::Wait,
            _ => StanzaErrorType::Unknown,
        })
        .unwrap_or(StanzaErrorType::Unknown);

    let (condition, text) = match error_el {
        None => ("unknown".to_string(), None),
        Some(err) => {
            let condition = err
                .children()
                .find(|c| c.ns() == NS_STANZAS && c.name() != "text")
                .map(|c| c.name().to_string())
                .unwrap_or_else(|| "unknown".to_string());

            let text = err.get_child("text", NS_STANZAS).map(|t| t.text());

            (condition, text)
        }
    };

    StanzaError {
        error_type,
        condition,
        text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_iq_error(error_type: &str, condition: &str, text: Option<&str>) -> Element {
        const NS_STANZAS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";
        const NS_CLIENT: &str = "jabber:client";

        let mut error_children = Element::builder("error", NS_CLIENT)
            .attr("type", error_type)
            .append(Element::builder(condition, NS_STANZAS).build());
        if let Some(t) = text {
            error_children = Element::builder("error", NS_CLIENT)
                .attr("type", error_type)
                .append(Element::builder(condition, NS_STANZAS).build())
                .append(Element::builder("text", NS_STANZAS).append(t).build());
        }

        Element::builder("iq", NS_CLIENT)
            .attr("type", "error")
            .attr("id", "req-1")
            .append(error_children.build())
            .build()
    }

    #[test]
    fn parse_stanza_error_cancel_not_found() {
        let iq = make_iq_error("cancel", "not-found", None);
        let err = parse_stanza_error(&iq);
        assert_eq!(err.error_type, StanzaErrorType::Cancel);
        assert_eq!(err.condition, "not-found");
        assert_eq!(err.text, None);
    }

    #[test]
    fn parse_stanza_error_auth_with_text() {
        let iq = make_iq_error("auth", "forbidden", Some("Not allowed"));
        let err = parse_stanza_error(&iq);
        assert_eq!(err.error_type, StanzaErrorType::Auth);
        assert_eq!(err.condition, "forbidden");
        assert_eq!(err.text.as_deref(), Some("Not allowed"));
    }

    #[test]
    fn parse_stanza_error_unknown_type() {
        let iq = make_iq_error("bogus", "service-unavailable", None);
        let err = parse_stanza_error(&iq);
        assert_eq!(err.error_type, StanzaErrorType::Unknown);
        assert_eq!(err.condition, "service-unavailable");
    }

    #[test]
    fn parse_stanza_error_no_error_child_gives_unknown() {
        let iq = Element::builder("iq", "jabber:client")
            .attr("type", "error")
            .attr("id", "req-1")
            .build();
        let err = parse_stanza_error(&iq);
        assert_eq!(err.error_type, StanzaErrorType::Unknown);
        assert_eq!(err.condition, "unknown");
        assert_eq!(err.text, None);
    }
}
