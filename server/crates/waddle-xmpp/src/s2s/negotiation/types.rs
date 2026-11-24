//! Configuration, inputs and outputs of the S2S negotiation state machine.

use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use minidom::Element;

use crate::identity::CanonicalDomain;
use crate::s2s::condition::{SaslFailureCondition, StreamErrorCondition};
use crate::s2s::decoder::DecodeError;
use crate::s2s::header::{StreamHeader, StreamId, StreamLanguage};
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
    fn destination(&self, destination: &CanonicalDomain) -> DestinationStatus;

    /// Whether streams from `source` to the served `destination` may be
    /// negotiated at all.
    fn admits(&self, source: &CanonicalDomain, destination: &CanonicalDomain) -> bool;

    /// Whether the receiving role may authenticate `source` by dialback for
    /// streams to `destination` (Receiving Server, XEP-0220 §2.2.1). Asked
    /// once TLS has completed. Nothing but the owner's policy enables it.
    fn permits_dialback(&self, _source: &CanonicalDomain, _destination: &CanonicalDomain) -> bool {
        false
    }
}

/// Computes and checks Server Dialback keys (XEP-0185) with the deployment's
/// dialback secret.
///
/// Every replica that can answer verification requests holds the same
/// secret, so a key one replica issued verifies on any other, and no other
/// deployment holds it (ADR-0020 D2).
pub trait DialbackKeys: Send + Sync {
    /// The key the Originating Server `originating` sends to the Receiving
    /// Server `receiving` on the stream whose response header carried
    /// `stream_id`, or `None` when it cannot be computed; the stream then
    /// fails instead of authenticating.
    fn key(
        &self,
        receiving: &CanonicalDomain,
        originating: &CanonicalDomain,
        stream_id: &StreamId,
    ) -> Option<DialbackKey>;

    /// Whether `key` is the key for these inputs. Implementations compare
    /// in constant time.
    fn verify(
        &self,
        receiving: &CanonicalDomain,
        originating: &CanonicalDomain,
        stream_id: &StreamId,
        key: &DialbackKey,
    ) -> bool;
}

/// Server Dialback (XEP-0220) handling.
///
/// Dialback is only ever used over TLS, and a stream carries exactly one
/// domain pair: no sender or target multiplexing.
///
/// - [`Self::Disabled`]: every dialback decision point closes the stream
///   with `policy-violation` (RFC 6120 §6.4.5): no certificate usable for
///   SASL EXTERNAL on the receiving side, no SASL EXTERNAL offer on the
///   initiating side, and dialback elements on the wire.
/// - [`Self::Enabled`]: in the initiating role, authenticate by dialback
///   (Initiating Server) when the receiving server does not offer SASL
///   EXTERNAL or refuses it with `not-authorized` (XEP-0178 §3, step
///   11b), in either case only where it advertised dialback; in the
///   receiving role, answer `<db:verify/>` requests for keys this
///   deployment issued (Authoritative Server). Whether a receiving
///   stream may itself authenticate its peer by dialback
///   (Receiving Server) is decided per pair by
///   [`InboundAdmission::permits_dialback`], independently of this mode.
#[derive(Clone, Default)]
pub enum DialbackMode {
    #[default]
    Disabled,
    Enabled(Arc<dyn DialbackKeys>),
}

impl DialbackMode {
    /// The keys, when the dialback roles that need them are enabled.
    pub fn keys(&self) -> Option<&Arc<dyn DialbackKeys>> {
        match self {
            Self::Disabled => None,
            Self::Enabled(keys) => Some(keys),
        }
    }
}

impl fmt::Debug for DialbackMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => f.write_str("Disabled"),
            Self::Enabled(_) => f.write_str("Enabled"),
        }
    }
}

/// Longest dialback key accepted from a peer, in bytes. An XEP-0185 key is
/// 64 hexadecimal characters.
pub const MAX_DIALBACK_KEY_BYTES: usize = 1024;

/// A dialback key: the character data of `<db:result/>` or `<db:verify/>`
/// (XEP-0220 §2.1). It is opaque, between one and
/// [`MAX_DIALBACK_KEY_BYTES`] visible ASCII characters, and never written
/// to logs.
#[derive(Clone, PartialEq, Eq)]
pub struct DialbackKey(String);

impl DialbackKey {
    /// Accept a key, ignoring surrounding whitespace, or `None` when it is
    /// empty, too long or contains anything but visible ASCII characters.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim_matches(|c: char| c.is_ascii_whitespace());
        let visible = text.bytes().all(|byte| byte.is_ascii_graphic());
        if text.is_empty() || text.len() > MAX_DIALBACK_KEY_BYTES || !visible {
            return None;
        }
        Some(Self(text.to_owned()))
    }

    /// The key as sent on the wire.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DialbackKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DialbackKey(..)")
    }
}

/// A dialback key received from an Initiating Server, which the Receiving
/// Server's owner verifies with the Authoritative Server of
/// [`Self::source`] (XEP-0220 §2.2.1 and §2.1.2) and answers with
/// [`NegotiationInput::Dialback`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialbackRequest {
    /// The Sender Domain: the source of the pair the key is for.
    pub source: CanonicalDomain,
    /// The Target Domain: the destination of the pair the key is for, a
    /// local domain.
    pub destination: CanonicalDomain,
    /// The ID of the response stream header this server sent on the stream
    /// the key arrived on.
    pub stream_id: StreamId,
    pub key: DialbackKey,
}

/// A stanza error condition carried by a dialback error (XEP-0220 §2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialbackError {
    /// The Authoritative Server could not be reached.
    RemoteConnectionFailed,
    /// The Authoritative Server could not be found, or answered with an
    /// error or `host-unknown`.
    RemoteServerNotFound,
    /// The connection to the Authoritative Server failed or ended without
    /// an answer.
    RemoteServerTimeout,
    /// The asserted identity could not be established, for example because
    /// the Authoritative Server's certificate failed validation.
    NotAcceptable,
    /// This server lacks the resources to authorize the pair.
    ResourceConstraint,
    /// The pair may not use dialback here.
    NotAuthorized,
    /// The Target Domain is not a configured domain of this server
    /// (XEP-0220 §2.5).
    ItemNotFound,
    /// A policy refuses the request, for example a TLS requirement
    /// (XEP-0220 §2.5).
    PolicyViolation,
    /// The Initiating Server's key was refused for a pair on a stream that
    /// already carries verified pairs; the stream stays open (XEP-0220
    /// §2.2.1).
    Forbidden,
}

impl DialbackError {
    /// The defined condition's element name (RFC 6120 §8.3.3).
    pub fn condition(self) -> &'static str {
        match self {
            Self::RemoteConnectionFailed => "remote-connection-failed",
            Self::RemoteServerNotFound => "remote-server-not-found",
            Self::RemoteServerTimeout => "remote-server-timeout",
            Self::NotAcceptable => "not-acceptable",
            Self::ResourceConstraint => "resource-constraint",
            Self::NotAuthorized => "not-authorized",
            Self::ItemNotFound => "item-not-found",
            Self::PolicyViolation => "policy-violation",
            Self::Forbidden => "forbidden",
        }
    }
}

/// The owner's answer to a [`DialbackRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialbackOutcome {
    /// The Authoritative Server confirmed the key.
    Valid,
    /// The Authoritative Server refused the key.
    Invalid,
    /// The key could not be verified.
    Error(DialbackError),
}

/// A verification request a Receiving Server sends to an Authoritative
/// Server on a connection opened for it (XEP-0220 §2.1.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyRequest {
    /// The ID of the response stream header the Receiving Server sent to
    /// the Initiating Server.
    pub stream_id: StreamId,
    /// The key the Initiating Server sent.
    pub key: DialbackKey,
}

/// The Authoritative Server's answer on a verification connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyVerdict {
    Valid,
    Invalid,
    /// A dialback error (`type='error'`).
    Error,
}

/// How an initiating connection reaches TLS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TlsEntry {
    /// A plaintext stream header, then STARTTLS (RFC 6120 §5).
    #[default]
    StartTls,
    /// TLS from the first byte (XEP-0368): no plaintext stream and no
    /// `<starttls/>`. The connection owner sets the TLS Server Name
    /// Indication to the destination domain and offers the `xmpp-server`
    /// ALPN protocol.
    DirectTls,
}

/// How a stream's domain pair was authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamAuthentication {
    /// TLS certificate verification followed by SASL EXTERNAL.
    SaslExternal,
    /// XEP-0220 Server Dialback over TLS.
    Dialback,
}

/// Configuration of a receiving stream.
#[derive(Clone)]
pub struct ReceivingConfig {
    pub admission: Arc<dyn InboundAdmission>,
    /// Authoritative local domain used as the `from` of a stream header that
    /// only carries an error because the requested domain is not served
    /// (RFC 6120 §4.9.1.3).
    pub default_domain: CanonicalDomain,
    pub language: StreamLanguage,
    pub sasl: SaslLimits,
    pub dialback: DialbackMode,
    /// The clock the negotiation's rates are measured against.
    pub clock: NegotiationClock,
}

/// A monotonic clock for the negotiation's rates: the system clock, or one
/// a test controls.
#[derive(Clone)]
pub struct NegotiationClock(Arc<dyn Fn() -> Instant + Send + Sync>);

impl NegotiationClock {
    /// [`Instant::now`].
    pub fn system() -> Self {
        Self(Arc::new(Instant::now))
    }

    /// A clock that reads `now`.
    pub fn new(now: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
        Self(Arc::new(now))
    }

    /// The current instant.
    pub fn now(&self) -> Instant {
        (self.0)()
    }
}

impl Default for NegotiationClock {
    fn default() -> Self {
        Self::system()
    }
}

impl fmt::Debug for NegotiationClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NegotiationClock")
    }
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
    pub source: CanonicalDomain,
    /// The remote domain this stream is addressed to.
    pub destination: CanonicalDomain,
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
    Verified(CanonicalDomain),
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
    pub source: CanonicalDomain,
    pub destination: CanonicalDomain,
}

/// The connection owner's answer to an [`AuthorizationRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationDecision {
    Granted,
    Denied,
}

/// A stream whose negotiation completed over TLS: SASL EXTERNAL and the
/// authenticated restart, or Server Dialback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiatedStream {
    pub role: Role,
    /// The initiating (sending) domain.
    pub source: CanonicalDomain,
    /// The receiving domain.
    pub destination: CanonicalDomain,
    pub authentication: StreamAuthentication,
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
    /// The connection owner refuses this stream because another stream
    /// already carries its directional domain pair.
    Conflict,
    /// The connection owner refuses the stream or what it carries under
    /// local policy, such as a stanza no service accepts or a domain that is
    /// no longer admitted.
    PolicyViolation,
    /// The connection owner refuses a stanza whose address is written in a
    /// spelling the identity profile refuses (RFC 6120 §4.9.3.7).
    ImproperAddressing,
}

impl Signal {
    /// The stream error sent when this signal ends a stream.
    pub fn condition(self) -> StreamErrorCondition {
        match self {
            Self::Timeout => StreamErrorCondition::ConnectionTimeout,
            Self::Expired => StreamErrorCondition::Reset,
            Self::Shutdown => StreamErrorCondition::SystemShutdown,
            Self::ResourceExhausted => StreamErrorCondition::ResourceConstraint,
            Self::Conflict => StreamErrorCondition::Conflict,
            Self::PolicyViolation => StreamErrorCondition::PolicyViolation,
            Self::ImproperAddressing => StreamErrorCondition::ImproperAddressing,
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
    /// `<proceed/>` (RFC 6120 §5.4.3.1), and bytes it sent earlier are
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
    /// The owner's answer to a [`Step::VerifyDialback`].
    Dialback(DialbackOutcome),
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
    /// The peer's first bytes were not XML, such as a TLS ClientHello sent
    /// to the STARTTLS port; the connection closes without any XML.
    NotXml,
    /// Dialback did not authenticate the pair: the key was refused
    /// (`type='invalid'`). The stream closes without a stream error.
    DialbackRefused,
    /// Dialback did not authenticate the pair: the key could not be
    /// verified (`type='error'`). The stream closes without a stream error.
    DialbackFailed,
    /// A verification connection received its answer and closes normally.
    VerificationComplete,
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
    /// Negotiation completed for a domain pair; again for each further pair
    /// a Receiving Server verifies on a stream authenticated by dialback.
    Established(NegotiatedStream),
    /// Hand an address-checked stanza from an established receiving stream
    /// to the connection owner.
    Deliver(Element),
    /// Ask the connection owner to verify a received dialback key with the
    /// Authoritative Server and answer with [`NegotiationInput::Dialback`].
    VerifyDialback(DialbackRequest),
    /// A verification connection received the Authoritative Server's
    /// answer.
    VerifyVerdict(VerifyVerdict),
    /// The receiving role answered a `<db:verify/>` request as the
    /// Authoritative Server; `valid` is the answer.
    DialbackAnswered { valid: bool },
    /// Hand a stanza from an established receiving stream whose `to` or
    /// `from` is a valid address written in a spelling the identity profile
    /// refuses to the connection owner, which may answer it with a
    /// `jid-malformed` stanza error instead of closing the stream.
    DeliverMalformed(Element),
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
    /// Receiving role: TLS completed without a certificate usable for SASL
    /// EXTERNAL; awaiting a dialback key or verification request.
    DialbackAwaitKey,
    /// Receiving role: awaiting the owner's verification of a dialback key.
    DialbackVerifying,
    /// Initiating role: a dialback key was sent; awaiting the result.
    DialbackAwaitResult,
    /// Verification connection: the request was sent; awaiting the answer.
    VerifyAwaitResult,
    Established,
    Closed,
}
