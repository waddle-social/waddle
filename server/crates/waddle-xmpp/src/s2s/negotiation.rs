//! S2S stream negotiation state machine (RFC 6120 §4–§6, XEP-0178,
//! XEP-0220 and XEP-0368).

mod classify;
mod dialback;
mod elements;
mod initiating;
mod machine;
mod receiving;
mod types;

pub use machine::Negotiation;
pub use types::{
    AuthorizationDecision, AuthorizationRequest, CloseReason, DestinationStatus, DialbackError,
    DialbackKey, DialbackKeys, DialbackMode, DialbackOutcome, DialbackRequest, ExchangeId,
    InboundAdmission, InitiatingConfig, NegotiatedStream, NegotiationClock, NegotiationInput,
    NegotiationPhase, ReceivingConfig, Role, Signal, Step, StreamAuthentication, TlsEntry, TlsPeer,
    VerifyRequest, VerifyVerdict, MAX_DIALBACK_KEY_BYTES,
};
