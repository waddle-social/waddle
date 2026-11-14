//! Configuration, inputs and outputs of the S2S negotiation state machine.

use std::fmt;
use std::sync::Arc;

use jid::DomainPart;
use minidom::Element;

use crate::s2s::condition::{SaslFailureCondition, StreamErrorCondition};
use crate::s2s::decoder::DecodeError;
use crate::s2s::header::{StreamHeader, StreamLanguage};
use crate::s2s::limits::SaslLimits;

/// Which side of the TCP connection this stream belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Opened the TCP connection and authenticates as the source domain.
    Initiating,
    /// Accepted the TCP connection and serves the destination domain.
    Receiving,
}

/// Whether a stream header recipient is served locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestinationStatus {
    Served,
    /// Formerly served; answered with `host-gone`.
    Gone,
    /// Never served; answered with `host-unknown`.
    Unknown,
}

/// Local admission decisions consulted by the receiving role.
///
/// Admission grants transport only: a positive answer lets the peer continue
/// to TLS and SASL, it never authenticates or authorises the peer.
pub trait InboundAdmission: Send + Sync {
    /// Whether `destination` is a domain this process serves.
    fn destination(&self, destination: &DomainPart) -> DestinationStatus;

    /// Whether streams from `source` to the served `destination` may be
    /// negotiated at all.
    fn admits(&self, source: &DomainPart, destination: &DomainPart) -> bool;
}

/// Server Dialback (XEP-0220) handling.
///
/// Dialback is not implemented by this codec. The negotiation keeps explicit
/// decision points where a weaker-identity fallback would apply (no usable
/// certificate on the receiving side, no SASL EXTERNAL offer on the
/// initiating side, and dialback elements on the wire); in
/// [`Self::Disabled`] each of them closes the stream with
/// `policy-violation` (RFC 6120 §6.4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DialbackMode {
    #[default]
    Disabled,
}

/// Configuration of a receiving stream.
#[derive(Clone)]
pub struct ReceivingConfig {
    pub admission: Arc<dyn InboundAdmission>,
    /// Authoritative local domain used as the `from` of a stream header that
    /// only carries an error because the requested domain is not served
    /// (RFC 6120 §4.9.1.3).
    pub default_domain: DomainPart,
    pub language: StreamLanguage,
    pub sasl: SaslLimits,
    pub dialback: DialbackMode,
}

impl fmt::Debug for ReceivingConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReceivingConfig")
            .field("default_domain", &self.default_domain)
            .field("language", &self.language)
            .field("sasl", &self.sasl)
            .field("dialback", &self.dialback)
            .finish_non_exhaustive()
    }
}

/// Configuration of an initiating stream for one directional domain pair.
#[derive(Debug, Clone)]
pub struct InitiatingConfig {
    /// The local domain this stream authenticates as.
    pub source: DomainPart,
    /// The remote domain this stream is addressed to.
    pub destination: DomainPart,
    pub language: StreamLanguage,
    pub dialback: DialbackMode,
}

/// The peer identity established by TLS certificate verification.
///
/// The connection owner produces this after its verifier has validated the
/// peer certificate against the reference identity for this role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TlsPeer {
    /// The certificate was verified for this domain.
    Verified(DomainPart),
    /// The peer presented no certificate usable for strong authentication.
    NoCertificate,
}

/// Identifies one SASL exchange; replacing or aborting an exchange makes
/// decisions for earlier exchanges stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExchangeId(u32);

impl ExchangeId {
    pub(crate) fn first() -> Self {
        Self(1)
    }

    pub(crate) fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// A SASL EXTERNAL authorization the connection owner must decide.
///
/// The owner re-checks certificate validity and pair admission and answers
/// with [`NegotiationInput::Authorization`] carrying the same exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationRequest {
    pub exchange: ExchangeId,
    pub source: DomainPart,
    pub destination: DomainPart,
}

/// The connection owner's answer to an [`AuthorizationRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationDecision {
    Granted,
    Denied,
}

/// A stream whose TLS, SASL EXTERNAL and authenticated restart completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiatedStream {
    pub role: Role,
    /// The initiating (sending) domain.
    pub source: DomainPart,
    /// The receiving domain.
    pub destination: DomainPart,
}

/// Connection-level events that end negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// A read, negotiation or idle deadline passed.
    Timeout,
    /// The certificate, revocation data or connection age expired.
    Expired,
    /// The process is draining.
    Shutdown,
    /// A resource limit outside the codec was exhausted.
    ResourceExhausted,
}

impl Signal {
    /// The stream error sent when this signal ends a stream.
    pub fn condition(self) -> StreamErrorCondition {
        match self {
            Self::Timeout => StreamErrorCondition::ConnectionTimeout,
            Self::Expired => StreamErrorCondition::Reset,
            Self::Shutdown => StreamErrorCondition::SystemShutdown,
            Self::ResourceExhausted => StreamErrorCondition::ResourceConstraint,
        }
    }
}

/// An input to [`super::Negotiation::handle`].
#[derive(Debug, Clone, PartialEq)]
pub enum NegotiationInput {
    Header(StreamHeader),
    /// A first-level element with no further input received after it.
    Element(Element),
    /// A first-level element after which the peer had already sent more
    /// input. The receiving role refuses this for `<starttls/>`: the
    /// initiating entity must not send anything until it has received
    /// `<proceed/>` (RFC 6120 §5.4.2.3), and bytes it sent earlier are
    /// neither XML of this stream nor the TLS handshake it asked for.
    PipelinedElement(Element),
    /// The peer sent `</stream:stream>`.
    PeerEnd,
    DecodeFailed(DecodeError),
    /// Input arrived between SASL success and the post-SASL stream header.
    PrematureInput,
    TlsEstablished(TlsPeer),
    Authorization {
        exchange: ExchangeId,
        decision: AuthorizationDecision,
    },
    Signal(Signal),
}

/// Why a stream closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// This side sent the stream error.
    StreamError(StreamErrorCondition),
    /// The peer sent the stream error.
    PeerStreamError(StreamErrorCondition),
    /// The peer closed its stream.
    PeerClosed,
    /// SASL failed with this condition, sent by the receiving role or
    /// received by the initiating role.
    SaslFailure(SaslFailureCondition),
    /// The peer aborted SASL with no exchanges left.
    SaslExhausted,
    /// The receiving server refused STARTTLS.
    TlsRefused,
    /// An outbound item could not be serialised.
    LocalFailure,
}

impl CloseReason {
    /// The stream error to send before closing, if any.
    pub fn stream_error(self) -> Option<StreamErrorCondition> {
        match self {
            Self::StreamError(condition) => Some(condition),
            _ => None,
        }
    }
}

/// One instruction from the negotiation, executed in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Open a new outbound stream with this header.
    Open(StreamHeader),
    /// Send a first-level element on the open outbound stream.
    Send(Element),
    /// Stop XML processing at the element just handled and start TLS.
    StartTls,
    /// Discard both streams after SASL success and await a new header.
    RestartAfterSasl,
    /// Ask the connection owner to authorise SASL EXTERNAL.
    Authorize(AuthorizationRequest),
    /// Negotiation completed.
    Established(NegotiatedStream),
    /// Close: send the stream error (if any) and closing tag where the
    /// outbound stream is open, then close the transport.
    Close(CloseReason),
}

/// Externally visible negotiation progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NegotiationPhase {
    /// Plaintext stream: headers, features and the STARTTLS command.
    PreTls,
    /// TLS handshake in progress; no XML may be exchanged.
    TlsHandshake,
    /// Encrypted stream before SASL: headers and features.
    TlsStream,
    /// Awaiting `<auth/>` (receiving) or the SASL outcome (initiating).
    SaslAwaitAuth,
    /// Receiving role: an empty challenge was sent.
    SaslAwaitResponse,
    /// Receiving role: awaiting the connection owner's decision.
    SaslAuthorizing,
    /// Initiating role: `<auth/>` sent, awaiting success or failure.
    SaslAwaitOutcome,
    /// Encrypted, authenticated stream before the final features.
    AuthenticatedRestart,
    Established,
    Closed,
}
