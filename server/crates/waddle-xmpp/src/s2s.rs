//! Server-to-server (`jabber:server`) stream codec and negotiation.
//!
//! This module is I/O-agnostic: it owns no sockets, TLS sessions,
//! certificate verification or configuration loading; the server's S2S
//! connection manager drives it. It provides:
//!
//! - [`StreamDecoder`]: a bounded incremental XML decoder enforcing
//!   [`CodecLimits`] while parsing, with exact stream-restart boundaries and
//!   a pre-authentication mode that refuses non-negotiation elements before
//!   building them;
//! - [`StreamEncoder`]: serialisation of stream headers, first-level
//!   elements, stream errors and the closing tag;
//! - [`StreamHeader`] and [`StreamErrorCondition`]: typed stream headers and
//!   stream errors;
//! - [`Negotiation`]: the initiating and receiving negotiation state machine
//!   (STARTTLS or XEP-0368 direct TLS, SASL EXTERNAL, authenticated
//!   restart), with XEP-0220 Server Dialback over TLS in the Initiating,
//!   Receiving and Authoritative Server roles when [`DialbackMode`] and the
//!   owner's admission enable them, and verification connections;
//! - [`S2sStream`]: a driver combining all of the above for one connection.
//!
//! Stream header addresses are read from the raw attribute text into the
//! identity profile's [`CanonicalDomain`](crate::identity::CanonicalDomain),
//! so every pair and peer comparison is made in canonical form; emitted
//! headers write U-labels.

mod condition;
mod decoder;
mod encoder;
mod header;
mod limits;
mod negotiation;
mod stream;

pub use condition::{SaslFailureCondition, StreamErrorCondition};
pub use decoder::{DecodeError, DiscardedInput, Inbound, StreamDecoder, TlsHandoff};
pub use encoder::{EncodeError, StreamEncoder};
pub use header::{HeaderAddress, StreamHeader, StreamId, StreamLanguage, StreamVersion};
pub use limits::{CodecLimits, LimitKind, SaslLimits};
pub use negotiation::{
    AuthorizationDecision, AuthorizationRequest, CloseReason, DestinationStatus, DialbackError,
    DialbackKey, DialbackKeys, DialbackMode, DialbackOutcome, DialbackRequest, ExchangeId,
    InboundAdmission, InitiatingConfig, NegotiatedStream, Negotiation, NegotiationInput,
    NegotiationPhase, ReceivingConfig, Role, Signal, Step, StreamAuthentication, TlsEntry, TlsPeer,
    VerifyRequest, VerifyVerdict, MAX_DIALBACK_KEY_BYTES,
};
pub use stream::{Output, S2sStream};
