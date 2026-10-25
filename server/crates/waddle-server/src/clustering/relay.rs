//! Per-node relay actor for the clustering swarm (ADR-0017 elements 5/6).
//!
//! Each node registers exactly **one** name in kademlia: its relay actor,
//! keyed by the current ownership `node_id`, updated on self-fence recovery.
//! Kademlia carries node
//! discovery only — entity→node resolution is Phase 3's Postgres claims
//! table, and per-entity DHT registration is ruled out by kameo 0.20's
//! hardcoded `MemoryStore` limits.
//!
//! The relay is **supervised**: kameo auto-unregisters an actor that stops or
//! panics (removing this node from the routing fabric while its Postgres
//! heartbeat stays fresh — a steady-state, cluster-wide degradation with no
//! self-healing path), so an owning task respawns it and **re-registers it
//! under the same name**. Sender-side no-effect stale-ref errors
//! (`ActorNotRunning`/`UnknownActor`/`BadActorType`) trigger a bounded-backoff
//! kademlia re-lookup for non-idempotent delivery paths; `ActorStopped` is
//! treated as maybe committed so callers do not duplicate user-visible work.
//!
//! Phase 4 wires DM/MUC/presence routing through this relay. Kademlia still
//! discovers node relays only; entity ownership remains in Postgres claims.

use crate::config::ClusteringMessagingConfig;
use crate::ingress::execute::RelayFrameReceiptCompletion;
use crate::server::routes::interpret::carbons::CarbonFanoutFailure;
use crate::server::routes::interpret::RecipientInventory;
use crate::server::routes::websocket::AdminListAnswer;
pub(crate) mod frame_receipts;
mod muc_cleanup;
mod remote_resource_compat;
use frame_receipts::PendingReplyReceipts;
pub use frame_receipts::RelayReplyReceiptToken;
pub use muc_cleanup::{RelayMucCleanup, RelayMucCleanupOutcome};

use super::codec::RemoteStanza;
use super::local_claims::RoomLocalClaims;
use super::metrics;
use super::ordered_relay::{
    OrderedRelayMucProxyKind, OrderedRelayNack, OrderedRelayNackReason, OrderedRelayPayload,
    OrderedRelayReceiverState, OrderedRelayReply, OrderedRelayReservation, RemoteStanzaEnvelope,
};
use super::resume_bridge::ResumeStealBridge;
use super::route_bridge::{
    OrderedRelayDeliveryBridge, RemoteResourceOutboundFrame, RemoteResourceRegistrationId,
    RemoteResourceRouteOutcome, RemoteResourceRouteTarget, RemoteResourceSocketGeneration,
    RemoteResourceStateSnapshot, RemoteResourceStateUpdate,
    RemoteResourceWriteAcceptedOutboundFrame, RemoteUserSideEffect,
};
use super::self_fence::LocallyClaimedEntities;
use super::trace_context::RelayTraceContext;
use super::NodeId;
use kameo::actor::{ActorRef, RemoteActorRef, Spawn};
use kameo::error::RemoteSendError;
use kameo::message::{Context, Message};
use kameo::{Actor, RemoteActor, Reply};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use waddle_xmpp::ownership::{ClaimEpoch, Entity, SharedNodeIdentity};

#[cfg(test)]
mod tests;

mod implementation;
#[cfg(test)]
use implementation::classify;
#[cfg(test)]
use implementation::classify_effect;
#[cfg(test)]
use implementation::finish_ordered_reservation;

use implementation::is_no_effect_stale_ref_relookup_error;
#[cfg(test)]
use implementation::is_ordered_unsupported_envelope_error;
#[cfg(test)]
use implementation::is_stale_ref_error;
#[cfg(test)]
use implementation::ordered_send_error;

#[cfg(test)]
use implementation::refresh_relay_registration;

use implementation::relay_dispatch_span;
pub use implementation::relay_name;
use implementation::send_error;
use implementation::spawn_in_dispatch_span;
pub use implementation::spawn_supervised;
pub use implementation::Demote;
pub use implementation::DemoteReply;
pub use implementation::RelayActor;
pub use implementation::RelayAskError;
pub use implementation::RelayCrash;
pub use implementation::RelayDeliverOrdered;
pub use implementation::RelayDeliverRemoteResourceFrame;
pub use implementation::RelayDeliverRemoteResourceWriteAcceptedFrame;
use implementation::RelayDispatchKind;
pub use implementation::RelayEchoReply;
pub use implementation::RelayEchoStanza;
pub use implementation::RelayExtensionRoomSend;
pub use implementation::RelayExtensionRoomSendReply;
pub use implementation::RelayFaultAck;
pub use implementation::RelayForceDetachRemoteUserResource;
pub use implementation::RelayForceDetachRemoteUserResourceReply;
pub use implementation::RelayHandle;
pub use implementation::RelayMucAdminQuery;
pub use implementation::RelayMucAdminQueryItem;
pub use implementation::RelayMucAdminQueryReply;
pub use implementation::RelayPing;
pub use implementation::RelayPong;
pub use implementation::RelayReassertMediaGrants;
pub use implementation::RelayReassertMediaGrantsReply;
pub(crate) use implementation::RelayRecipientInventory;
pub(crate) use implementation::RelayRecipientInventoryReply;
#[cfg(test)]
use implementation::RelayRegisterAttempt;
pub use implementation::RelayRegisterRemoteUserResource;
pub use implementation::RelayRegistrationTrigger;
pub use implementation::RelayRemoteResourceForceDetachStatus;
pub use implementation::RelayRemoteResourceFrameReply;
pub use implementation::RelayRemoteResourceFrameStatus;
pub use implementation::RelayRemoteResourceRegistrationReply;
pub use implementation::RelayRemoteResourceRegistrationStatus;
pub use implementation::RelayRemoteResourceUnregisterReply;
pub use implementation::RelayRemoteResourceUnregisterStatus;
pub use implementation::RelayRemoteResourceUpdateReply;
pub use implementation::RelayRemoteResourceUpdateStatus;
pub use implementation::RelayRemoteResourceWriteAcceptedReply;
pub use implementation::RelayRemoteResourceWriteAcceptedStatus;
pub use implementation::RelayRemoteUserSideEffect;
pub use implementation::RelayRemoteUserSideEffectReply;
pub use implementation::RelayRemoteUserSideEffectStatus;
pub use implementation::RelayResourcePresence;
pub use implementation::RelayResourcePresenceReply;
pub use implementation::RelayResumeSteal;
pub use implementation::RelayResumeStealReply;
pub use implementation::RelayRouteRemoteResourceStanza;
pub use implementation::RelayRouteRemoteResourceStanzaReply;
pub use implementation::RelaySendEffect;
pub use implementation::RelaySendFailure;
pub use implementation::RelaySleep;
pub use implementation::RelayUnregisterRemoteUserResource;
pub use implementation::RelayUpdateRemoteUserResource;

#[cfg(test)]
use implementation::REREGISTER_INTERVAL;
#[cfg(test)]
use implementation::RESPAWN_BACKOFF;
