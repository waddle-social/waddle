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
//! - [`LocalDelivery`]: how a correlated response, an error, or the traffic
//!   of a remote room reaches a local resource, and whether the recipient's
//!   blocking list applies to it ([`LocalOrigin`]).
//! - [`RemoteRoomStore`]: the remote rooms local resources occupy or are
//!   joining, whose presence and messages are exempt from D7 in both
//!   directions (XEP-0045), shared by every replica through the
//!   deployment's database ([`SqlRemoteRoomStore`]), with the cached
//!   answers on which remote JIDs are rooms.
//! - [`LocalAccounts`]: the account side of traffic between remote senders
//!   and local accounts (D7, gate G18); attaching it enables that traffic
//!   in both directions.
//!
//! The decisions, the handoff contract and the reachability inventory are
//! recorded in `server/docs/planning/s2s-routing-2047.md`, and those of
//! account traffic in `server/docs/planning/s2s-account-traffic-2047.md`.

mod accounts;
mod correlation;
mod local;
mod occupancy;
mod principal;
mod reach;
mod replies;
mod room_delivery;
mod rooms;
mod router;
mod telemetry;

pub use accounts::{
    carries_dm_pin, is_account_presence, is_outbound_account_traffic, AccountWork, LocalAccounts,
    MAX_ACCOUNT_REPLIES,
};
pub use correlation::{
    CorrelationDirection, CorrelationError, CorrelationKey, CorrelationOwner, IqCorrelations,
    PendingRequest, RequestKind, SqlIqCorrelations, MAX_CORRELATED_ADDRESS_BYTES,
    MAX_REQUEST_ID_BYTES,
};
pub use local::{LocalDelivery, LocalDeliveryOutcome, LocalOrigin};
pub use occupancy::{
    InsertOutcome, Occupancy, OccupancyPhase, RemoteRoomStore, RoomLimits, RoomStoreError,
    RoomVerdict, SqlRemoteRoomStore, MAX_JOIN_ID_BYTES, MAX_OCCUPANCY_ADDRESS_BYTES,
};
pub use principal::{PrincipalError, RemotePrincipal};
pub use reach::{reach, InboundReach, LocalTarget, Refusal};
pub use router::{RouterLimits, RouterOptions, S2sRouter};
