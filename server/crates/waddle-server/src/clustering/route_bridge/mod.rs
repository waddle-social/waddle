//! ADR-0017 Phase 4 Slice 3: ordered-relay bridge for cross-node 1:1
//! full-JID delivery.
//!
//! The relay actor is spawned during clustering bring-up, before the
//! WebSocket routing graph exists. This bridge mirrors
//! [`super::resume_bridge::ResumeStealBridge`]: construct it empty at swarm
//! spawn time, then wire the narrow services it needs once
//! `create_websocket_state` has built the live registries.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use kameo::actor::ActorRef;
use libp2p::identity::{Keypair, PublicKey};
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use waddle_xmpp::ownership::{
    ClaimSnapshot, ClaimStore, Entity, EntityType, NodeIdentity, SharedNodeIdentity,
};
use waddle_xmpp::protocol::CarbonKind;
use waddle_xmpp::registry::{
    BroadcastOutcome, ConnectionEntry, ConnectionRegistry, DeliveryKind, ForceDetachOutcome,
    ForceDetachRequest, OutboundStanza, PresenceState, RegisterUserResourceIfOwnerOrAbsent,
    UnregisterUserResource, UserRegistryActor,
};
use waddle_xmpp::roster::{RosterItem, RosterVersion};
use waddle_xmpp::stream_management::InMemorySmSessionRegistry;
use waddle_xmpp::xep::xep0191::BlockingStorage;
use waddle_xmpp::Stanza;

use super::allowlist::AllowlistStore;
use super::claims::NodeLeaseStore;
use super::codec::{RemoteElement, RemoteStanza};
use super::ordered_relay::{
    OrderedRelayChannel, OrderedRelayClaim, OrderedRelayClaimRole, OrderedRelayDiversion,
    OrderedRelayDiversionReason, OrderedRelayEnvelopeClaims, OrderedRelayMucProxyKind,
    OrderedRelayNack, OrderedRelayNackReason, OrderedRelayOrigin, OrderedRelayOriginProof,
    OrderedRelayPayload, OrderedRelayRecipient, OrderedRelayReply, OrderedRelaySenderState,
    OriginInboundSequence, RemoteStanzaEnvelope,
};
use super::relay::{
    RelayAskError, RelayDeliverRemoteResourceFrame, RelayForceDetachRemoteUserResource,
    RelayForceDetachRemoteUserResourceReply, RelayHandle, RelayRegisterRemoteUserResource,
    RelayRemoteResourceForceDetachStatus, RelayRemoteResourceFrameReply,
    RelayRemoteResourceFrameStatus, RelayRemoteResourceRegistrationReply,
    RelayRemoteResourceRegistrationStatus, RelayRemoteResourceUnregisterReply,
    RelayRemoteResourceUpdateReply, RelayRemoteResourceUpdateStatus, RelayRemoteUserSideEffect,
    RelayRemoteUserSideEffectReply, RelayRemoteUserSideEffectStatus,
    RelayRouteRemoteResourceStanza, RelayRouteRemoteResourceStanzaReply, RelaySendEffect,
    RelaySendFailure, RelayUnregisterRemoteUserResource, RelayUpdateRemoteUserResource,
};
use super::trace_context::RelayTraceContext;
use super::NodeId;
use crate::config::ClusteringMessagingConfig;
use crate::server::routes::interpret::{
    FullJidDeliveryOutcome, OrderedRelayRouteOrigin, OrderedRelayRouteOriginKind,
};
use crate::server::routes::websocket::{interpret_loop::build_interpret_deps, WebSocketState};
const ORDERED_DELIVERY_MAILBOX_TIMEOUT: Duration = Duration::from_secs(2);
const ORDERED_DELIVERY_REPLY_TIMEOUT: Duration = Duration::from_secs(8);
const ORDERED_RECEIVER_DELIVERY_TIMEOUT: Duration = Duration::from_secs(6);
const ORDERED_RECEIVER_EFFECT_TIMEOUT_MARGIN: Duration = Duration::from_millis(250);
const MAX_ORDERED_RELAY_CHANNEL_LOCKS: usize = 4096;
const MAX_REMOTE_OWNER_REGISTRATION_LOCKS: usize = 4096;
const REMOTE_RESOURCE_OUTBOUND_CHANNEL_SIZE: usize = 256;

mod delivery;
mod reassert;
mod registration;
mod types;
mod validation;

pub(crate) use delivery::{MucProxyRouteDecision, OrderedRelayMucProxyOutcome};
pub use reassert::LocalMediaGrantReassertion;
use registration::*;
pub(crate) use types::RemoteResourceRegisterOutcome;
use types::*;
pub use types::{
    RemoteCarbonKind, RemotePresenceShow, RemotePresenceStateSnapshot,
    RemoteResourceOriginSnapshot, RemoteResourceOutboundFrame, RemoteResourceRegistrationId,
    RemoteResourceRouteOutcome, RemoteResourceRouteTarget, RemoteResourceSocketGeneration,
    RemoteResourceStateSnapshot, RemoteResourceStateUpdate, RemoteUserSideEffect,
};
use validation::*;

pub struct OrderedRelayDeliveryServices {
    pub claim_store: Arc<dyn ClaimStore>,
    pub allowlist_store: Arc<dyn AllowlistStore>,
    pub node_lease: Arc<dyn NodeLeaseStore>,
    pub node_identity: SharedNodeIdentity,
    pub connection_registry: Arc<ConnectionRegistry>,
    pub user_registry: ActorRef<UserRegistryActor>,
    pub sm_session_registry: Arc<InMemorySmSessionRegistry>,
    pub blocking_storage: Arc<dyn BlockingStorage>,
    pub web_socket_state: Weak<WebSocketState>,
}

pub struct OrderedRelayDeliveryBridge {
    services: OnceLock<Arc<OrderedRelayDeliveryServices>>,
    origin_signer: OnceLock<RelayOriginSigner>,
    sender_state: Mutex<OrderedRelaySenderState>,
    channel_locks: Mutex<HashMap<OrderedRelayChannel, Arc<Mutex<()>>>>,
    remote_socket_resources: Mutex<HashMap<jid::FullJid, RemoteSocketRegistration>>,
    remote_socket_generations: Mutex<HashMap<jid::FullJid, RemoteResourceSocketGeneration>>,
    remote_owner_resources: Mutex<HashMap<jid::FullJid, RemoteOwnerRegistration>>,
    remote_owner_registration_locks: Mutex<HashMap<jid::FullJid, Arc<Mutex<()>>>>,
    stop_token: CancellationToken,
    mailbox_timeout: Duration,
    reply_timeout: Duration,
}

impl OrderedRelayDeliveryBridge {
    pub fn new(stop_token: CancellationToken, messaging: &ClusteringMessagingConfig) -> Arc<Self> {
        Arc::new(Self {
            services: OnceLock::new(),
            origin_signer: OnceLock::new(),
            sender_state: Mutex::new(OrderedRelaySenderState::default()),
            channel_locks: Mutex::new(HashMap::new()),
            remote_socket_resources: Mutex::new(HashMap::new()),
            remote_socket_generations: Mutex::new(HashMap::new()),
            remote_owner_resources: Mutex::new(HashMap::new()),
            remote_owner_registration_locks: Mutex::new(HashMap::new()),
            stop_token,
            // WebSocket stanza dispatch has a 15s wedge backstop. Full-JID
            // ordered delivery must resolve before that backstop can cancel
            // the origin future, otherwise the receiver may still commit a
            // side effect after the origin has synthesized a local timeout.
            mailbox_timeout: messaging
                .mailbox_timeout
                .min(ORDERED_DELIVERY_MAILBOX_TIMEOUT),
            reply_timeout: messaging.reply_timeout.min(ORDERED_DELIVERY_REPLY_TIMEOUT),
        })
    }

    pub fn wire(&self, services: Arc<OrderedRelayDeliveryServices>) {
        if self.services.set(services).is_err() {
            tracing::error!(
                "OrderedRelayDeliveryBridge::wire called more than once; \
                 ignoring duplicate service bundle"
            );
        }
    }

    /// SFU handle for the undeliverable-IQ bounce compensation (#1444).
    /// `None` before `wire()`, when the websocket state is gone, or in
    /// deployments without an SFU — the bounce then still scrubs the
    /// credential echo, it just skips the local JTI revocation.
    fn sfu_for_bounce(&self) -> Option<Arc<dyn waddle_sfu::SfuService>> {
        self.services
            .get()?
            .web_socket_state
            .upgrade()
            .and_then(|state| state.deps.protocol.sfu.clone())
    }

    pub fn wire_origin_signer(&self, keypair: Keypair) {
        if self
            .origin_signer
            .set(RelayOriginSigner::new(keypair))
            .is_err()
        {
            tracing::error!(
                "OrderedRelayDeliveryBridge::wire_origin_signer called more than once; \
                 ignoring duplicate signer"
            );
        }
    }

    pub(crate) fn reserved_delivery_effect_timeout(&self) -> Duration {
        self.reply_timeout
            .checked_sub(ORDERED_RECEIVER_EFFECT_TIMEOUT_MARGIN)
            .filter(|timeout| !timeout.is_zero())
            .unwrap_or_else(|| self.reply_timeout / 2)
    }
}

#[cfg(test)]
mod tests;
