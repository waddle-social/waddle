//! S2S stream negotiation state machine (RFC 6120 §4–§6, XEP-0178).

mod classify;
mod elements;
mod initiating;
mod machine;
mod receiving;
mod types;

pub use machine::Negotiation;
pub use types::{
    AuthorizationDecision, AuthorizationRequest, CloseReason, DestinationStatus, DialbackMode,
    ExchangeId, InboundAdmission, InitiatingConfig, NegotiatedStream, NegotiationInput,
    NegotiationPhase, ReceivingConfig, Role, Signal, Step, TlsPeer,
};
