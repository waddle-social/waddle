//! S2S routing (ADR-0020 transport, D7 and D10; gates G8, G10 and G23).
//!
//! [`S2sRouter`] is the stanza sink of inbound S2S streams and the remote
//! route of local entities when S2S is enabled:
//!
//! - [`RemotePrincipal`]: the typed identity of a remote sender, built from
//!   a stanza's `from` and the inbound stream that authenticated its
//!   domain; never a local session or account.
//! - [`reach`]: what a stanza from an S2S stream may reach (D10, D7), the
//!   code form of the inventory gate G23 requires.
//! - [`IqCorrelations`]: pending IQ requests across S2S, completed at most
//!   once with expiry, shared by every replica through the deployment's
//!   database ([`SqlIqCorrelations`]).
//! - [`LocalDelivery`]: how a correlated response or an error reaches the
//!   local resource that sent the request.
//!
//! The decisions, the handoff contract and the reachability inventory are
//! recorded in `server/docs/planning/s2s-routing-2047.md`.

mod correlation;
mod local;
mod principal;
mod reach;
mod replies;
mod router;

pub use correlation::{
    CorrelationDirection, CorrelationError, CorrelationKey, IqCorrelations, PendingRequest,
    RequestKind, SqlIqCorrelations,
};
pub use local::{LocalDelivery, LocalDeliveryOutcome};
pub use principal::{PrincipalError, RemotePrincipal};
pub use reach::{reach, InboundReach, LocalTarget, Refusal};
pub use router::{RouterLimits, RouterOptions, S2sRouter, MAX_REQUEST_ID_BYTES};
