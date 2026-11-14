//! Server-to-server (`jabber:server`) stream codec and negotiation.
//!
//! This module is I/O-agnostic: it owns no sockets, TLS sessions,
//! certificate verification or configuration loading, and nothing in the
//! server uses it yet. It provides:
//!
//! - [`StreamDecoder`]: a bounded incremental XML decoder enforcing
//!   [`CodecLimits`] while parsing, with exact stream-restart boundaries;
//! - [`StreamEncoder`]: serialisation of stream headers, first-level
//!   elements, stream errors and the closing tag;
//! - [`StreamHeader`] and [`StreamErrorCondition`]: typed stream headers and
//!   stream errors;
//! - [`Negotiation`]: the initiating and receiving negotiation state machine
//!   (STARTTLS, SASL EXTERNAL, authenticated restart), with Server Dialback
//!   decision points that refuse while [`DialbackMode::Disabled`];
//! - [`S2sStream`]: a driver combining all of the above for one connection.

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
    AuthorizationDecision, AuthorizationRequest, CloseReason, DestinationStatus, DialbackMode,
    ExchangeId, InboundAdmission, InitiatingConfig, NegotiatedStream, Negotiation,
    NegotiationInput, NegotiationPhase, ReceivingConfig, Role, Signal, Step, TlsPeer,
};
pub use stream::{Output, S2sStream};
