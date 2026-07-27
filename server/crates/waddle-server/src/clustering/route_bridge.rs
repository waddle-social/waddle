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

fn stanza_message_id(stanza: &Stanza) -> &str {
    match stanza {
        Stanza::Message(message) => message.id.as_ref().map_or("", |id| id.0.as_str()),
        Stanza::Iq(_) | Stanza::Presence(_) => "",
    }
}

type RemoteDeliveryFuture<'a> =
    Pin<Box<dyn Future<Output = Option<FullJidDeliveryOutcome>> + Send + 'a>>;

/// Narrow service bundle needed by the relay receiver to validate ownership
/// and hand an inbound full-JID stanza to the local `UserActor` delivery path.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct RemoteResourceRegistrationId(uuid::Uuid);

impl RemoteResourceRegistrationId {
    fn fresh() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct RemoteResourceSocketGeneration(u64);

impl RemoteResourceSocketGeneration {
    fn next(current: Option<Self>) -> Self {
        Self(
            current
                .map(|generation| generation.0)
                .unwrap_or(0)
                .saturating_add(1),
        )
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RemotePresenceStateSnapshot {
    pub show: Option<String>,
    pub status: Option<String>,
    pub priority: i8,
    pub payloads: Vec<RemoteElement>,
}

impl From<PresenceState> for RemotePresenceStateSnapshot {
    fn from(state: PresenceState) -> Self {
        Self {
            show: state.show,
            status: state.status,
            priority: state.priority,
            payloads: state.payloads.into_iter().map(RemoteElement).collect(),
        }
    }
}

impl From<RemotePresenceStateSnapshot> for PresenceState {
    fn from(state: RemotePresenceStateSnapshot) -> Self {
        Self {
            show: state.show,
            status: state.status,
            priority: state.priority,
            payloads: state
                .payloads
                .into_iter()
                .map(|element| element.0)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RemoteResourceStateSnapshot {
    pub carbons_enabled: bool,
    pub roster_interested: bool,
    pub blocklist_interested: bool,
    pub presence_available: bool,
    pub presence_priority: i8,
    pub presence_state: Option<RemotePresenceStateSnapshot>,
}

impl RemoteResourceStateSnapshot {
    fn from_entry(entry: &ConnectionEntry, presence_state: Option<PresenceState>) -> Self {
        Self {
            carbons_enabled: entry.carbons_enabled.load(Ordering::Relaxed),
            roster_interested: entry.roster_interested.load(Ordering::Relaxed),
            blocklist_interested: entry.blocklist_interested.load(Ordering::Relaxed),
            presence_available: entry.presence_available.load(Ordering::Relaxed),
            presence_priority: entry.presence_priority.load(Ordering::Relaxed),
            presence_state: presence_state.map(RemotePresenceStateSnapshot::from),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum RemoteResourceStateUpdate {
    Presence {
        available: bool,
        priority: i8,
        state: Option<RemotePresenceStateSnapshot>,
    },
    Carbons {
        enabled: bool,
    },
    RosterInterested,
    BlocklistInterested,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub enum RemoteCarbonKind {
    Sent,
    Received,
}

impl From<CarbonKind> for RemoteCarbonKind {
    fn from(kind: CarbonKind) -> Self {
        match kind {
            CarbonKind::Sent => Self::Sent,
            CarbonKind::Received => Self::Received,
        }
    }
}

impl From<RemoteCarbonKind> for CarbonKind {
    fn from(kind: RemoteCarbonKind) -> Self {
        match kind {
            RemoteCarbonKind::Sent => Self::Sent,
            RemoteCarbonKind::Received => Self::Received,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum RemoteUserSideEffect {
    Carbons {
        owner: jid::BareJid,
        message: RemoteStanza,
        kind: RemoteCarbonKind,
        exclude: Vec<jid::FullJid>,
    },
    RosterPush {
        user_jid: jid::BareJid,
        source_jid: jid::FullJid,
        item: RosterItem,
        version: RosterVersion,
    },
    BlocklistPush {
        user_bare: jid::BareJid,
        blocked: bool,
        jids: Vec<jid::Jid>,
    },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RemoteResourceOutboundFrame {
    pub jid: jid::FullJid,
    pub registration_id: RemoteResourceRegistrationId,
    pub stanza: RemoteStanza,
    pub kind: DeliveryKind,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum RemoteResourceRouteTarget {
    FullJid {
        target: jid::FullJid,
        stanza: RemoteStanza,
    },
    BareJid {
        target: jid::BareJid,
        stanza: RemoteStanza,
    },
    MucProxy {
        room_jid: jid::BareJid,
        kind: OrderedRelayMucProxyKind,
        stanza: RemoteStanza,
    },
}

fn route_target_stanza_is_iq(target: &RemoteResourceRouteTarget) -> bool {
    match target {
        RemoteResourceRouteTarget::FullJid { stanza, .. }
        | RemoteResourceRouteTarget::BareJid { stanza, .. }
        | RemoteResourceRouteTarget::MucProxy { stanza, .. } => matches!(stanza.0, Stanza::Iq(_)),
    }
}

/// The few small fields the delivery-outcome log needs, extracted
/// before the route target (and its full stanza) is moved into the
/// routing future — logging must never force a stanza deep-clone.
struct RouteOutcomeLog {
    kind: &'static str,
    entity: String,
    message_id: String,
}

fn route_outcome_log(target: &RemoteResourceRouteTarget) -> RouteOutcomeLog {
    match target {
        RemoteResourceRouteTarget::FullJid { target, stanza } => RouteOutcomeLog {
            kind: "full-JID",
            entity: target.to_string(),
            message_id: stanza_message_id(&stanza.0).to_owned(),
        },
        RemoteResourceRouteTarget::BareJid { target, stanza } => RouteOutcomeLog {
            kind: "bare-JID",
            entity: target.to_string(),
            message_id: stanza_message_id(&stanza.0).to_owned(),
        },
        RemoteResourceRouteTarget::MucProxy {
            room_jid, stanza, ..
        } => RouteOutcomeLog {
            kind: "MUC proxy",
            entity: room_jid.to_string(),
            message_id: stanza_message_id(&stanza.0).to_owned(),
        },
    }
}

fn log_remote_resource_route_outcome(context: &RouteOutcomeLog, outcome: FullJidDeliveryOutcome) {
    tracing::debug!(
        kind = context.kind,
        entity = %context.entity,
        message_id = %context.message_id,
        ?outcome,
        "ordered-relay delivery outcome"
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RemoteResourceRouteOutcome {
    Delivered,
    QueuedDetached,
    Unavailable,
    Dropped,
    StaleRegistration,
    MaybeCommitted,
    JoinMaybeCommitted,
}

impl From<FullJidDeliveryOutcome> for RemoteResourceRouteOutcome {
    fn from(outcome: FullJidDeliveryOutcome) -> Self {
        match outcome {
            FullJidDeliveryOutcome::Delivered => Self::Delivered,
            FullJidDeliveryOutcome::QueuedDetached => Self::QueuedDetached,
            FullJidDeliveryOutcome::Unavailable => Self::Unavailable,
            FullJidDeliveryOutcome::Dropped => Self::Dropped,
            FullJidDeliveryOutcome::MaybeCommitted => Self::MaybeCommitted,
        }
    }
}

impl From<RemoteResourceRouteOutcome> for FullJidDeliveryOutcome {
    fn from(outcome: RemoteResourceRouteOutcome) -> Self {
        match outcome {
            RemoteResourceRouteOutcome::Delivered => Self::Delivered,
            RemoteResourceRouteOutcome::QueuedDetached => Self::QueuedDetached,
            RemoteResourceRouteOutcome::Unavailable
            | RemoteResourceRouteOutcome::StaleRegistration => Self::Unavailable,
            RemoteResourceRouteOutcome::Dropped => Self::Dropped,
            RemoteResourceRouteOutcome::MaybeCommitted
            | RemoteResourceRouteOutcome::JoinMaybeCommitted => Self::MaybeCommitted,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteResourceRegisterOutcome {
    Registered,
    NotRemote,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteResourceOriginRefresh {
    Remote(RemoteResourceOriginSnapshot),
    LocalOwner,
    Failed,
}

#[derive(Debug, Clone)]
struct RemoteSocketRegistration {
    registration_id: RemoteResourceRegistrationId,
    socket_generation: RemoteResourceSocketGeneration,
    owner: Arc<AtomicBool>,
    user_owner: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteResourceOriginSnapshot {
    pub jid: jid::FullJid,
    pub registration_id: RemoteResourceRegistrationId,
    pub socket_generation: RemoteResourceSocketGeneration,
    pub user_owner: NodeId,
}

#[derive(Debug, Clone)]
struct RemoteOwnerRegistration {
    registration_id: RemoteResourceRegistrationId,
    socket_node: NodeId,
    socket_generation: RemoteResourceSocketGeneration,
    owner: Arc<AtomicBool>,
}

struct RelayOriginSigner {
    keypair: Keypair,
    public_key: Vec<u8>,
}

impl RelayOriginSigner {
    fn new(keypair: Keypair) -> Self {
        let public_key = keypair.public().encode_protobuf();
        Self {
            keypair,
            public_key,
        }
    }
}

struct PreparedRemoteDelivery {
    services: Arc<OrderedRelayDeliveryServices>,
    target_entity: Entity,
    previous_owner: NodeIdentity,
    channel: OrderedRelayChannel,
    envelope: RemoteStanzaEnvelope,
    target: jid::Jid,
    stanza: Stanza,
    is_iq: bool,
}

struct RemoteDeliveryOutcome {
    delivery: FullJidDeliveryOutcome,
    client_replies: Vec<Stanza>,
    maybe_committed: bool,
    join_repair_allowed: bool,
}

fn caller_delivery_outcome(outcome: RemoteDeliveryOutcome) -> FullJidDeliveryOutcome {
    if outcome.maybe_committed {
        FullJidDeliveryOutcome::MaybeCommitted
    } else {
        outcome.delivery
    }
}

struct RemoteDeliverySeed {
    services: Arc<OrderedRelayDeliveryServices>,
    target_entity: Entity,
    previous_owner: NodeIdentity,
    channel: OrderedRelayChannel,
    asserted_origin_node: NodeId,
    origin_inbound_sequence: OriginInboundSequence,
    origin_claim: OrderedRelayClaim,
    sender_claim: OrderedRelayClaim,
    target_claim: OrderedRelayClaim,
    payload: OrderedRelayPayload,
    target: jid::Jid,
    stanza: Stanza,
    is_iq: bool,
}

/// Construction-order bridge plus shared sender sequencing state.
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

#[derive(Debug, Clone)]
pub(crate) enum OrderedRelayMucProxyOutcome {
    Delivered(Vec<Stanza>),
    Unavailable,
    Dropped,
    MaybeCommitted,
    JoinMaybeCommitted,
}

/// Typed routing decision for a MUC proxy attempt (#1249). The old
/// `Option<OrderedRelayMucProxyOutcome>` API collapsed six distinct
/// "no relay attempted" conditions into one `None`, forcing the
/// disconnect-cleanup caller to treat the benign "room claim is locally
/// owned" case (the local room loop handles the leave moments later)
/// exactly like the harmful "origin `UserActor` claim held by another
/// node" case (which recurs whenever the disconnecting user has a
/// second device on another node and previously ghosted the occupant
/// forever). Callers that only need the legacy semantics keep using
/// [`OrderedRelayDeliveryBridge::try_proxy_muc_remote`]; the cleanup
/// path consumes this decision directly.
#[derive(Debug, Clone)]
pub(crate) enum MucProxyRouteDecision {
    /// An ordered-relay send was attempted; the payload is its result.
    Attempted(OrderedRelayMucProxyOutcome),
    /// The room claim is owned by THIS node — the local room path is
    /// authoritative and handles the stanza. Benign for cleanup: the
    /// local `LeaveByRealJid` loop converges the occupancy.
    LocalRoom,
    /// Definitive: no claim row exists for the room, so no node holds a
    /// live `RoomActor` (occupancy is in-memory on the claim owner).
    /// There is no remote occupancy left to clean up.
    RoomUnclaimed,
    /// The room claim could not be used right now: the claim lookup
    /// errored, the owner's lease is stale (owner crash / renewal lag),
    /// or the bridge services are not wired. Retryable.
    RoomClaimUnavailable,
    /// The origin/sender claim needed to sequence the relay is not
    /// usable from this node (typically: the origin `UserActor` claim
    /// is held by the node hosting the user's other device). Retryable;
    /// disconnect cleanup avoids this case up-front by preferring the
    /// remote-resource origin when the socket was registered against a
    /// foreign `UserActor` owner.
    OriginUnavailable,
}

impl MucProxyRouteDecision {
    /// Legacy adapter: `Some(outcome)` iff a relay send was attempted;
    /// `None` means "keep the existing local path" (all non-attempt
    /// variants), exactly matching the pre-#1249 `Option` contract.
    fn into_attempted(self) -> Option<OrderedRelayMucProxyOutcome> {
        match self {
            MucProxyRouteDecision::Attempted(outcome) => Some(outcome),
            MucProxyRouteDecision::LocalRoom
            | MucProxyRouteDecision::RoomUnclaimed
            | MucProxyRouteDecision::RoomClaimUnavailable
            | MucProxyRouteDecision::OriginUnavailable => None,
        }
    }
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

    pub(crate) async fn remote_resource_origin_if_owner(
        &self,
        jid: &jid::FullJid,
        owner: &Arc<AtomicBool>,
    ) -> Option<RemoteResourceOriginSnapshot> {
        let registrations = self.remote_socket_resources.lock().await;
        let registration = registrations
            .get(jid)
            .filter(|registration| Arc::ptr_eq(&registration.owner, owner))?;
        Some(RemoteResourceOriginSnapshot {
            jid: jid.clone(),
            registration_id: registration.registration_id,
            socket_generation: registration.socket_generation,
            user_owner: registration.user_owner.clone(),
        })
    }

    pub(crate) async fn try_deliver_registered_remote_resource(
        &self,
        target: &jid::FullJid,
        stanza: &Stanza,
        kind: DeliveryKind,
    ) -> Option<FullJidDeliveryOutcome> {
        let registration = {
            let registrations = self.remote_owner_resources.lock().await;
            registrations.get(target).cloned()
        }?;
        self.deliver_registered_remote_resource_with_registration(
            target,
            stanza,
            kind,
            &registration,
        )
        .await
    }

    /// Return `Some` only when this exact full-JID target is currently owned
    /// by a fresh foreign `UserActor` claim and an ordered-relay send was
    /// attempted. `None` means the caller must keep the existing local path.
    pub(crate) fn try_deliver_full_jid_remote<'a>(
        self: &'a Arc<Self>,
        target: &'a jid::FullJid,
        stanza: &'a Stanza,
        origin: &'a OrderedRelayRouteOrigin,
    ) -> RemoteDeliveryFuture<'a> {
        Box::pin(async move {
            if let Some(remote_origin) = remote_resource_origin(origin) {
                return Arc::clone(self)
                    .route_remote_resource_origin(
                        remote_origin,
                        RemoteResourceRouteTarget::FullJid {
                            target: target.clone(),
                            stanza: RemoteStanza(stanza.clone()),
                        },
                        stanza,
                        origin,
                    )
                    .await;
            }
            let services = self.services.get()?.clone();
            let target_entity = user_entity(&target.to_bare());
            let target_snapshot = current_claim(&services, &target_entity).await?;
            if !target_snapshot.owner_lease_fresh {
                return None;
            }
            let me = services.node_identity.current();
            if target_snapshot.owner == me {
                return None;
            }

            let (origin_entity, channel_origin) = route_origin_claim(&origin.kind);
            let origin_snapshot = current_claim(&services, &origin_entity).await?;
            if !origin_snapshot.owner_lease_fresh || origin_snapshot.owner != me {
                tracing::debug!(
                    target = %target,
                    origin_entity = %origin_entity,
                    "ordered relay: origin entity is not currently owned locally; \
                     keeping local fallback path"
                );
                return None;
            }
            let sender_claim =
                current_fresh_local_relay_claim(&services, &origin.sender_entity, &me, "sender")
                    .await?;

            let payload = payload_for_recipient(jid::Jid::from(target.clone()), stanza)?;
            let is_iq = matches!(stanza, Stanza::Iq(_));
            let channel = OrderedRelayChannel {
                origin: channel_origin,
                recipient: OrderedRelayRecipient::FullJid(target.clone()),
                target_epoch: target_snapshot.claim_epoch,
            };
            let origin_claim = OrderedRelayClaim {
                entity: origin_entity,
                epoch: origin_snapshot.claim_epoch,
            };
            let target_claim = OrderedRelayClaim {
                entity: target_entity.clone(),
                epoch: target_snapshot.claim_epoch,
            };
            let seed = RemoteDeliverySeed {
                services: services.clone(),
                target_entity: target_entity.clone(),
                previous_owner: target_snapshot.owner.clone(),
                channel,
                asserted_origin_node: NodeId::new(me.node_id.clone()),
                origin_inbound_sequence: OriginInboundSequence(origin.inbound_sequence),
                origin_claim,
                sender_claim,
                target_claim,
                payload,
                target: jid::Jid::from(target.clone()),
                stanza: stanza.clone(),
                is_iq,
            };

            if let Some(handoff) = origin.handoff.clone() {
                if handoff.mark_deferred() {
                    let bridge = Arc::clone(self);
                    let origin_stanza = stanza.clone();
                    let outcome_target = target.clone();
                    let outcome_message_id = match stanza {
                        Stanza::Message(message) => message.id.clone(),
                        Stanza::Iq(_) | Stanza::Presence(_) => None,
                    };
                    tokio::spawn(async move {
                        let delivery_outcome = bridge
                            .deliver_seeded_remote(seed, true)
                            .await
                            .map(caller_delivery_outcome);
                        if let Some(outcome) = delivery_outcome {
                            tracing::debug!(
                                jid = %outcome_target,
                                message_id = outcome_message_id
                                    .as_ref()
                                    .map_or("", |id| id.0.as_str()),
                                ?outcome,
                                "ordered-relay deferred full-JID delivery outcome"
                            );
                        }
                        let replies = delivery_outcome
                            .map(|outcome| replies_for_origin_handoff(&origin_stanza, outcome))
                            .unwrap_or_default();
                        handoff.complete(replies);
                    });
                    return Some(FullJidDeliveryOutcome::Delivered);
                }
            }

            let outcome =
                caller_delivery_outcome(Arc::clone(self).deliver_seeded_remote(seed, true).await?);
            tracing::debug!(
                jid = %target,
                message_id = stanza_message_id(stanza),
                ?outcome,
                "ordered-relay full-JID delivery outcome"
            );
            Some(outcome)
        })
    }

    /// Return `Some` only when this bare-JID target is currently owned by a
    /// fresh foreign `UserActor` claim and an ordered-relay send was attempted.
    /// `None` means the caller must keep the existing local path.
    pub(crate) fn try_deliver_bare_jid_remote<'a>(
        self: &'a Arc<Self>,
        target: &'a jid::BareJid,
        stanza: &'a Stanza,
        origin: &'a OrderedRelayRouteOrigin,
    ) -> RemoteDeliveryFuture<'a> {
        Box::pin(async move {
            if let Some(remote_origin) = remote_resource_origin(origin) {
                return Arc::clone(self)
                    .route_remote_resource_origin(
                        remote_origin,
                        RemoteResourceRouteTarget::BareJid {
                            target: target.clone(),
                            stanza: RemoteStanza(stanza.clone()),
                        },
                        stanza,
                        origin,
                    )
                    .await;
            }
            let services = self.services.get()?.clone();
            let target_entity = user_entity(target);
            let target_snapshot = current_claim(&services, &target_entity).await?;
            if !target_snapshot.owner_lease_fresh {
                return None;
            }
            let me = services.node_identity.current();
            if target_snapshot.owner == me {
                return None;
            }

            let (origin_entity, channel_origin) = route_origin_claim(&origin.kind);
            let origin_snapshot = current_claim(&services, &origin_entity).await?;
            if !origin_snapshot.owner_lease_fresh || origin_snapshot.owner != me {
                tracing::debug!(
                    target = %target,
                    origin_entity = %origin_entity,
                    "ordered relay: origin entity is not currently owned locally; \
                     keeping local fallback path"
                );
                return None;
            }
            let sender_claim =
                current_fresh_local_relay_claim(&services, &origin.sender_entity, &me, "sender")
                    .await?;

            let payload = payload_for_recipient(jid::Jid::from(target.clone()), stanza)?;
            let is_iq = matches!(stanza, Stanza::Iq(_));
            let channel = OrderedRelayChannel {
                origin: channel_origin,
                recipient: OrderedRelayRecipient::BareJid(target.clone()),
                target_epoch: target_snapshot.claim_epoch,
            };
            let origin_claim = OrderedRelayClaim {
                entity: origin_entity,
                epoch: origin_snapshot.claim_epoch,
            };
            let target_claim = OrderedRelayClaim {
                entity: target_entity.clone(),
                epoch: target_snapshot.claim_epoch,
            };
            let seed = RemoteDeliverySeed {
                services,
                target_entity,
                previous_owner: target_snapshot.owner,
                channel,
                asserted_origin_node: NodeId::new(me.node_id.clone()),
                origin_inbound_sequence: OriginInboundSequence(origin.inbound_sequence),
                origin_claim,
                sender_claim,
                target_claim,
                payload,
                target: jid::Jid::from(target.clone()),
                stanza: stanza.clone(),
                is_iq,
            };

            if let Some(handoff) = origin.handoff.clone() {
                if handoff.mark_deferred() {
                    let bridge = Arc::clone(self);
                    let origin_stanza = stanza.clone();
                    tokio::spawn(async move {
                        let replies = bridge
                            .deliver_seeded_remote(seed, true)
                            .await
                            .map(|outcome| {
                                replies_for_origin_handoff(
                                    &origin_stanza,
                                    caller_delivery_outcome(outcome),
                                )
                            })
                            .unwrap_or_default();
                        handoff.complete(replies);
                    });
                    return Some(FullJidDeliveryOutcome::Delivered);
                }
            }

            Some(caller_delivery_outcome(
                Arc::clone(self).deliver_seeded_remote(seed, true).await?,
            ))
        })
    }

    /// Return `Some` only when this room is currently owned by a fresh
    /// foreign `RoomActor` claim and an ordered-relay MUC proxy send was
    /// attempted. `None` means the caller must keep the existing local room
    /// path.
    pub(crate) async fn try_proxy_muc_remote(
        self: &Arc<Self>,
        room_jid: &jid::BareJid,
        stanza: &Stanza,
        kind: OrderedRelayMucProxyKind,
        origin: &OrderedRelayRouteOrigin,
    ) -> Option<OrderedRelayMucProxyOutcome> {
        self.try_proxy_muc_remote_decision(room_jid, stanza, kind, origin)
            .await
            .into_attempted()
    }

    /// Typed variant of [`Self::try_proxy_muc_remote`] (#1249): reports
    /// WHY no relay was attempted instead of a flat `None`, so the
    /// disconnect-cleanup path can converge (forget vs retry) per case.
    pub(crate) async fn try_proxy_muc_remote_decision(
        self: &Arc<Self>,
        room_jid: &jid::BareJid,
        stanza: &Stanza,
        kind: OrderedRelayMucProxyKind,
        origin: &OrderedRelayRouteOrigin,
    ) -> MucProxyRouteDecision {
        if let Some(remote_origin) = remote_resource_origin(origin) {
            return match Arc::clone(self)
                .route_remote_resource_origin_muc(
                    remote_origin,
                    RemoteResourceRouteTarget::MucProxy {
                        room_jid: room_jid.clone(),
                        kind,
                        stanza: RemoteStanza(stanza.clone()),
                    },
                    stanza,
                    origin,
                )
                .await
            {
                Some(outcome) => MucProxyRouteDecision::Attempted(outcome),
                // Only `services.get()` misses produce `None` on the
                // remote-resource path — the bridge is not wired yet.
                None => MucProxyRouteDecision::RoomClaimUnavailable,
            };
        }
        self.try_proxy_muc_remote_from_local_origin_decision(room_jid, stanza, kind, origin)
            .await
    }

    async fn try_proxy_muc_remote_from_local_origin(
        self: &Arc<Self>,
        room_jid: &jid::BareJid,
        stanza: &Stanza,
        kind: OrderedRelayMucProxyKind,
        origin: &OrderedRelayRouteOrigin,
    ) -> Option<OrderedRelayMucProxyOutcome> {
        self.try_proxy_muc_remote_from_local_origin_decision(room_jid, stanza, kind, origin)
            .await
            .into_attempted()
    }

    async fn try_proxy_muc_remote_from_local_origin_decision(
        self: &Arc<Self>,
        room_jid: &jid::BareJid,
        stanza: &Stanza,
        kind: OrderedRelayMucProxyKind,
        origin: &OrderedRelayRouteOrigin,
    ) -> MucProxyRouteDecision {
        let Some(services) = self.services.get().cloned() else {
            return MucProxyRouteDecision::RoomClaimUnavailable;
        };
        let target_entity = room_entity(room_jid);
        // Distinguish "no claim row exists" (definitive: no live
        // RoomActor anywhere, nothing to relay to) from "claim lookup
        // errored" (retryable) — `current_claim` conflates them (#1249).
        let target_snapshot = match services.claim_store.current_claim(&target_entity).await {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => return MucProxyRouteDecision::RoomUnclaimed,
            Err(error) => {
                tracing::warn!(
                    entity = %target_entity,
                    %error,
                    "ordered relay: claim lookup failed"
                );
                return MucProxyRouteDecision::RoomClaimUnavailable;
            }
        };
        if !target_snapshot.owner_lease_fresh {
            return MucProxyRouteDecision::RoomClaimUnavailable;
        }
        let me = services.node_identity.current();
        if target_snapshot.owner == me {
            return MucProxyRouteDecision::LocalRoom;
        }

        let (origin_entity, channel_origin) = route_origin_claim(&origin.kind);
        let Some(origin_snapshot) = current_claim(&services, &origin_entity).await else {
            return MucProxyRouteDecision::OriginUnavailable;
        };
        if !origin_snapshot.owner_lease_fresh || origin_snapshot.owner != me {
            tracing::debug!(
                room = %room_jid,
                origin_entity = %origin_entity,
                "ordered relay: MUC origin entity is not currently owned locally; \
                 keeping local fallback path"
            );
            return MucProxyRouteDecision::OriginUnavailable;
        }
        let Some(sender_claim) =
            current_fresh_local_relay_claim(&services, &origin.sender_entity, &me, "sender").await
        else {
            return MucProxyRouteDecision::OriginUnavailable;
        };

        let payload = OrderedRelayPayload::MucProxy {
            room_jid: room_jid.clone(),
            kind,
            stanza: RemoteStanza(stanza.clone()),
        };
        let channel = OrderedRelayChannel {
            origin: channel_origin,
            recipient: OrderedRelayRecipient::Room(room_jid.clone()),
            target_epoch: target_snapshot.claim_epoch,
        };
        let retry_channel = channel.clone();
        let previous_owner = target_snapshot.owner.clone();
        let origin_claim = OrderedRelayClaim {
            entity: origin_entity,
            epoch: origin_snapshot.claim_epoch,
        };
        let target_claim = OrderedRelayClaim {
            entity: room_entity(room_jid),
            epoch: target_snapshot.claim_epoch,
        };
        let seed = RemoteDeliverySeed {
            services: services.clone(),
            target_entity: target_entity.clone(),
            previous_owner: previous_owner.clone(),
            channel,
            asserted_origin_node: NodeId::new(me.node_id.clone()),
            origin_inbound_sequence: OriginInboundSequence(origin.inbound_sequence),
            origin_claim: origin_claim.clone(),
            sender_claim: sender_claim.clone(),
            target_claim: target_claim.clone(),
            payload: payload.clone(),
            target: jid::Jid::from(room_jid.clone()),
            stanza: stanza.clone(),
            is_iq: matches!(stanza, Stanza::Iq(_)),
        };

        let Some(outcome) = Arc::clone(self).deliver_seeded_remote(seed, true).await else {
            // `deliver_seeded_remote` yields `None` for more than the
            // benign "target claim refreshed to local ownership" case —
            // notably a `RelayAskError::NotFound` after a transient
            // owner-lookup miss while the room claim lease is still
            // fresh (the envelope was rolled back, provably never
            // delivered). Classify RETRYABLE (race review P2 on PR
            // #1277): the cleanup janitor re-drives it, and if the room
            // truly became local the next attempt classifies
            // `LocalRoom` and the local sweep converges the occupancy.
            return MucProxyRouteDecision::RoomClaimUnavailable;
        };
        if outcome.maybe_committed {
            if kind == OrderedRelayMucProxyKind::JoinPresence && outcome.join_repair_allowed {
                self.forget_channel(&retry_channel).await;
                let retry = Arc::clone(self)
                    .deliver_seeded_remote(
                        RemoteDeliverySeed {
                            services: services.clone(),
                            target_entity: target_entity.clone(),
                            previous_owner: previous_owner.clone(),
                            channel: retry_channel,
                            asserted_origin_node: NodeId::new(me.node_id.clone()),
                            origin_inbound_sequence: OriginInboundSequence(origin.inbound_sequence),
                            origin_claim: origin_claim.clone(),
                            sender_claim: sender_claim.clone(),
                            target_claim: target_claim.clone(),
                            payload: payload.clone(),
                            target: jid::Jid::from(room_jid.clone()),
                            stanza: stanza.clone(),
                            is_iq: false,
                        },
                        false,
                    )
                    .await;
                if let Some(retry) = retry.filter(|retry| !retry.maybe_committed) {
                    match retry.delivery {
                        FullJidDeliveryOutcome::Delivered
                        | FullJidDeliveryOutcome::QueuedDetached => {
                            return MucProxyRouteDecision::Attempted(
                                OrderedRelayMucProxyOutcome::Delivered(retry.client_replies),
                            );
                        }
                        FullJidDeliveryOutcome::Unavailable
                        | FullJidDeliveryOutcome::Dropped
                        | FullJidDeliveryOutcome::MaybeCommitted => {}
                    }
                }
                if let Some(repair) = Arc::clone(self)
                    .try_proxy_muc_join_repair(room_jid, stanza, origin)
                    .await
                {
                    if !repair.maybe_committed {
                        match repair.delivery {
                            FullJidDeliveryOutcome::Delivered
                            | FullJidDeliveryOutcome::QueuedDetached => {
                                return MucProxyRouteDecision::Attempted(
                                    OrderedRelayMucProxyOutcome::Delivered(repair.client_replies),
                                );
                            }
                            FullJidDeliveryOutcome::Unavailable
                            | FullJidDeliveryOutcome::Dropped
                            | FullJidDeliveryOutcome::MaybeCommitted => {}
                        }
                    }
                }
                return MucProxyRouteDecision::Attempted(
                    OrderedRelayMucProxyOutcome::JoinMaybeCommitted,
                );
            }
            // A `MujiJingleIq` maybe-committed deliberately gets the
            // same treatment as every other kind: the channel keeps
            // its diversion. An earlier attempt to `forget_channel`
            // here was WRONG and is recorded so it is not retried —
            // `OrderedRelaySenderState::forget_channel` drops
            // `next_by_channel`, resetting the SENDER's sequence to
            // FIRST while the receiver's `next_expected` is untouched.
            // The next envelope on the channel (the user's next
            // groupchat message or leave presence) would then arrive
            // with a stale sequence and NACK as an ordering gap,
            // converting a bounded failure into a diversion attached
            // to unrelated MUC traffic. Only the `JoinPresence` arm
            // above can forget safely, because it immediately re-sends
            // and has `try_proxy_muc_join_repair` as a backstop.
            return MucProxyRouteDecision::Attempted(OrderedRelayMucProxyOutcome::MaybeCommitted);
        }

        MucProxyRouteDecision::Attempted(match outcome.delivery {
            FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::QueuedDetached => {
                OrderedRelayMucProxyOutcome::Delivered(outcome.client_replies)
            }
            FullJidDeliveryOutcome::Unavailable => OrderedRelayMucProxyOutcome::Unavailable,
            FullJidDeliveryOutcome::Dropped => OrderedRelayMucProxyOutcome::Dropped,
            FullJidDeliveryOutcome::MaybeCommitted => {
                if kind == OrderedRelayMucProxyKind::JoinPresence {
                    OrderedRelayMucProxyOutcome::JoinMaybeCommitted
                } else {
                    OrderedRelayMucProxyOutcome::MaybeCommitted
                }
            }
        })
    }

    pub(crate) async fn try_register_remote_user_resource(
        self: &Arc<Self>,
        jid: &jid::FullJid,
        entry: ConnectionEntry,
        owner: Arc<AtomicBool>,
    ) -> RemoteResourceRegisterOutcome {
        let Some(services) = self.services.get().cloned() else {
            return RemoteResourceRegisterOutcome::Failed;
        };
        let target_entity = user_entity(&jid.to_bare());
        let Some(target_snapshot) = current_claim(&services, &target_entity).await else {
            return RemoteResourceRegisterOutcome::NotRemote;
        };
        if !target_snapshot.owner_lease_fresh {
            return RemoteResourceRegisterOutcome::NotRemote;
        }
        let me = services.node_identity.current();
        if target_snapshot.owner == me {
            return RemoteResourceRegisterOutcome::NotRemote;
        }

        let registration_id = RemoteResourceRegistrationId::fresh();
        let socket_generation = {
            let mut generations = self.remote_socket_generations.lock().await;
            let next = RemoteResourceSocketGeneration::next(generations.get(jid).copied());
            generations.insert(jid.clone(), next);
            next
        };
        let socket_node = NodeId::new(me.node_id.clone());
        let user_owner = NodeId::new(target_snapshot.owner.node_id.clone());
        let state = RemoteResourceStateSnapshot::from_entry(
            &entry,
            services.connection_registry.get_presence_state(jid),
        );
        let mut handle = RelayHandle::new(user_owner.clone(), self.stop_token.clone())
            .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        let reply = match handle
            .register_remote_user_resource(RelayRegisterRemoteUserResource {
                jid: jid.clone(),
                registration_id,
                socket_generation,
                socket_node,
                state,
                trace: RelayTraceContext::default(),
            })
            .await
        {
            Ok(reply) => reply,
            Err(error) => {
                tracing::warn!(
                    jid = %jid,
                    owner_node = %user_owner.as_str(),
                    %error,
                    "clustered remote-resource register ask failed"
                );
                return RemoteResourceRegisterOutcome::Failed;
            }
        };
        match reply.status {
            RelayRemoteResourceRegistrationStatus::Registered => {
                if services
                    .connection_registry
                    .entry_if_owner(jid, &owner)
                    .is_none()
                {
                    let _ = handle
                        .unregister_remote_user_resource(RelayUnregisterRemoteUserResource {
                            jid: jid.clone(),
                            registration_id,
                            socket_generation,
                            trace: RelayTraceContext::default(),
                        })
                        .await;
                    return RemoteResourceRegisterOutcome::Failed;
                }
                self.remote_socket_resources.lock().await.insert(
                    jid.clone(),
                    RemoteSocketRegistration {
                        registration_id,
                        socket_generation,
                        owner,
                        user_owner,
                    },
                );
                RemoteResourceRegisterOutcome::Registered
            }
            RelayRemoteResourceRegistrationStatus::NotOwner => {
                RemoteResourceRegisterOutcome::NotRemote
            }
            RelayRemoteResourceRegistrationStatus::StaleRegistration
            | RelayRemoteResourceRegistrationStatus::Unavailable => {
                RemoteResourceRegisterOutcome::Failed
            }
        }
    }

    pub(crate) async fn unregister_remote_user_resource_if_owner(
        &self,
        jid: &jid::FullJid,
        owner: &Arc<AtomicBool>,
    ) {
        let registration = {
            let mut registrations = self.remote_socket_resources.lock().await;
            match registrations.get(jid) {
                Some(registration) if Arc::ptr_eq(&registration.owner, owner) => {
                    registrations.remove(jid)
                }
                _ => None,
            }
        };
        let Some(registration) = registration else {
            return;
        };
        let mut handle = RelayHandle::new(registration.user_owner.clone(), self.stop_token.clone())
            .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        if let Err(error) = handle
            .unregister_remote_user_resource(RelayUnregisterRemoteUserResource {
                jid: jid.clone(),
                registration_id: registration.registration_id,
                socket_generation: registration.socket_generation,
                trace: RelayTraceContext::default(),
            })
            .await
        {
            tracing::warn!(
                jid = %jid,
                %error,
                "clustered remote-resource unregister ask failed; owner-side stale \
                 entry will self-heal on closed-channel delivery"
            );
        }
    }

    pub(crate) async fn update_remote_user_resource_if_owner(
        &self,
        jid: &jid::FullJid,
        owner: &Arc<AtomicBool>,
        update: RemoteResourceStateUpdate,
    ) {
        let registration = {
            let registrations = self.remote_socket_resources.lock().await;
            registrations
                .get(jid)
                .filter(|registration| Arc::ptr_eq(&registration.owner, owner))
                .cloned()
        };
        let Some(registration) = registration else {
            return;
        };
        let mut handle = RelayHandle::new(registration.user_owner.clone(), self.stop_token.clone())
            .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        match handle
            .update_remote_user_resource(RelayUpdateRemoteUserResource {
                jid: jid.clone(),
                registration_id: registration.registration_id,
                socket_generation: registration.socket_generation,
                update,
                trace: RelayTraceContext::default(),
            })
            .await
        {
            Ok(RelayRemoteResourceUpdateReply {
                status: RelayRemoteResourceUpdateStatus::Updated,
            }) => {}
            Ok(RelayRemoteResourceUpdateReply { status }) => {
                tracing::warn!(
                    jid = %jid,
                    status = ?status,
                    "clustered remote-resource state update failed closed; detaching socket"
                );
                self.detach_stale_remote_socket_resource(jid, &registration)
                    .await;
            }
            Err(error) => {
                tracing::warn!(
                    jid = %jid,
                    %error,
                    "clustered remote-resource state update ask failed; detaching socket"
                );
                self.detach_stale_remote_socket_resource(jid, &registration)
                    .await;
            }
        }
    }

    pub(crate) async fn try_fanout_remote_user_carbons(
        &self,
        source_jid: &jid::FullJid,
        owner: &jid::BareJid,
        message: &xmpp_parsers::message::Message,
        kind: CarbonKind,
        exclude: Vec<jid::FullJid>,
    ) -> bool {
        self.try_remote_user_side_effect(
            source_jid,
            RemoteUserSideEffect::Carbons {
                owner: owner.clone(),
                message: RemoteStanza(Stanza::Message(message.clone())),
                kind: kind.into(),
                exclude,
            },
        )
        .await
    }

    pub(crate) async fn try_fanout_remote_user_roster_push(
        &self,
        source_jid: &jid::FullJid,
        user_jid: &jid::BareJid,
        item: &RosterItem,
        version: &RosterVersion,
    ) -> bool {
        self.try_remote_user_side_effect(
            source_jid,
            RemoteUserSideEffect::RosterPush {
                user_jid: user_jid.clone(),
                source_jid: source_jid.clone(),
                item: item.clone(),
                version: version.clone(),
            },
        )
        .await
    }

    pub(crate) async fn try_fanout_remote_user_blocklist_push(
        &self,
        source_jid: &jid::FullJid,
        user_bare: &jid::BareJid,
        blocked: bool,
        jids: &[jid::Jid],
    ) -> bool {
        self.try_remote_user_side_effect(
            source_jid,
            RemoteUserSideEffect::BlocklistPush {
                user_bare: user_bare.clone(),
                blocked,
                jids: jids.to_vec(),
            },
        )
        .await
    }

    async fn try_remote_user_side_effect(
        &self,
        source_jid: &jid::FullJid,
        effect: RemoteUserSideEffect,
    ) -> bool {
        let Some(registration) = self.remote_socket_registration_if_current(source_jid).await
        else {
            return false;
        };
        let mut handle = RelayHandle::new(registration.user_owner.clone(), self.stop_token.clone())
            .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        match handle
            .remote_user_side_effect(RelayRemoteUserSideEffect {
                source_jid: source_jid.clone(),
                registration_id: registration.registration_id,
                socket_generation: registration.socket_generation,
                effect,
                trace: RelayTraceContext::default(),
            })
            .await
        {
            Ok(RelayRemoteUserSideEffectReply {
                status: RelayRemoteUserSideEffectStatus::Applied,
            }) => true,
            Ok(RelayRemoteUserSideEffectReply {
                status: RelayRemoteUserSideEffectStatus::StaleRegistration,
            }) => {
                self.remove_remote_socket_registration_if_current(source_jid, &registration)
                    .await;
                false
            }
            Ok(RelayRemoteUserSideEffectReply {
                status: RelayRemoteUserSideEffectStatus::Unavailable,
            }) => false,
            Err(RelayAskError::Send {
                effect: RelaySendEffect::MaybeCommitted,
                message,
                ..
            }) => {
                tracing::warn!(
                    jid = %source_jid,
                    %message,
                    "clustered remote-user side-effect relay may have committed; suppressing local fallback"
                );
                true
            }
            Err(error) => {
                tracing::warn!(
                    jid = %source_jid,
                    %error,
                    "clustered remote-user side-effect relay ask failed"
                );
                false
            }
        }
    }

    async fn remote_socket_registration_if_current(
        &self,
        jid: &jid::FullJid,
    ) -> Option<RemoteSocketRegistration> {
        let registration = self
            .remote_socket_resources
            .lock()
            .await
            .get(jid)
            .cloned()?;
        let services = self.services.get()?;
        services
            .connection_registry
            .entry_if_owner(jid, &registration.owner)
            .map(|_| registration)
    }

    async fn remove_remote_socket_registration_if_current(
        &self,
        jid: &jid::FullJid,
        registration: &RemoteSocketRegistration,
    ) {
        let mut registrations = self.remote_socket_resources.lock().await;
        if registrations.get(jid).is_some_and(|current| {
            current.registration_id == registration.registration_id
                && current.socket_generation == registration.socket_generation
                && current.user_owner == registration.user_owner
                && Arc::ptr_eq(&current.owner, &registration.owner)
        }) {
            registrations.remove(jid);
        }
    }

    async fn remove_remote_owner_registration_if_current(
        &self,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
    ) {
        let mut registrations = self.remote_owner_resources.lock().await;
        if registrations
            .get(jid)
            .is_some_and(|current| remote_owner_registration_matches(current, registration))
        {
            registrations.remove(jid);
        }
    }

    pub(crate) async fn register_remote_user_resource_on_owner(
        self: &Arc<Self>,
        msg: RelayRegisterRemoteUserResource,
    ) -> RelayRemoteResourceRegistrationReply {
        let jid = msg.jid.clone();
        let Some(lock) = self.lock_for_remote_owner_registration(&jid).await else {
            return RelayRemoteResourceRegistrationReply {
                status: RelayRemoteResourceRegistrationStatus::Unavailable,
            };
        };
        let guard = lock.lock().await;
        let reply = self
            .register_remote_user_resource_on_owner_locked(msg)
            .await;
        drop(guard);
        self.remove_remote_owner_registration_lock_if_unused(&jid, &lock)
            .await;
        reply
    }

    async fn register_remote_user_resource_on_owner_locked(
        self: &Arc<Self>,
        msg: RelayRegisterRemoteUserResource,
    ) -> RelayRemoteResourceRegistrationReply {
        let Some(services) = self.services.get().cloned() else {
            return RelayRemoteResourceRegistrationReply {
                status: RelayRemoteResourceRegistrationStatus::Unavailable,
            };
        };
        let target_entity = user_entity(&msg.jid.to_bare());
        let Some(snapshot) = current_claim(&services, &target_entity).await else {
            return RelayRemoteResourceRegistrationReply {
                status: RelayRemoteResourceRegistrationStatus::NotOwner,
            };
        };
        let me = services.node_identity.current();
        if !snapshot.owner_lease_fresh || snapshot.owner != me {
            return RelayRemoteResourceRegistrationReply {
                status: RelayRemoteResourceRegistrationStatus::NotOwner,
            };
        }

        if let Some(displaced) = self
            .remote_owner_resources
            .lock()
            .await
            .get(&msg.jid)
            .cloned()
        {
            if displaced.registration_id == msg.registration_id
                && displaced.socket_node == msg.socket_node
                && displaced.socket_generation == msg.socket_generation
            {
                match remote_owner_registration_is_current(&services, &msg.jid, &displaced).await {
                    Ok(()) => {
                        return RelayRemoteResourceRegistrationReply {
                            status: RelayRemoteResourceRegistrationStatus::Registered,
                        };
                    }
                    Err(RelayRemoteResourceRegistrationStatus::StaleRegistration) => {
                        self.remove_remote_owner_registration_if_current(&msg.jid, &displaced)
                            .await;
                    }
                    Err(status) => return RelayRemoteResourceRegistrationReply { status },
                }
            } else if displaced.socket_node == msg.socket_node
                && displaced.socket_generation >= msg.socket_generation
            {
                return RelayRemoteResourceRegistrationReply {
                    status: RelayRemoteResourceRegistrationStatus::StaleRegistration,
                };
            } else if displaced.socket_node != msg.socket_node {
                match remote_owner_registration_is_current(&services, &msg.jid, &displaced).await {
                    Ok(()) => {
                        if !self
                            .retire_remote_owner_registration(&services, &msg.jid, &displaced)
                            .await
                        {
                            return RelayRemoteResourceRegistrationReply {
                                status: RelayRemoteResourceRegistrationStatus::Unavailable,
                            };
                        }
                        let mut registrations = self.remote_owner_resources.lock().await;
                        match registrations.get(&msg.jid) {
                            Some(current)
                                if remote_owner_registration_matches(current, &displaced) =>
                            {
                                registrations.remove(&msg.jid);
                            }
                            Some(_) => {
                                return RelayRemoteResourceRegistrationReply {
                                    status:
                                        RelayRemoteResourceRegistrationStatus::StaleRegistration,
                                };
                            }
                            None => {}
                        }
                    }
                    Err(RelayRemoteResourceRegistrationStatus::StaleRegistration) => {
                        self.remove_remote_owner_registration_if_current(&msg.jid, &displaced)
                            .await;
                    }
                    Err(status) => return RelayRemoteResourceRegistrationReply { status },
                }
            } else {
                if !self
                    .retire_remote_owner_registration(&services, &msg.jid, &displaced)
                    .await
                {
                    return RelayRemoteResourceRegistrationReply {
                        status: RelayRemoteResourceRegistrationStatus::Unavailable,
                    };
                }
                let mut registrations = self.remote_owner_resources.lock().await;
                match registrations.get(&msg.jid) {
                    Some(current) if remote_owner_registration_matches(current, &displaced) => {
                        registrations.remove(&msg.jid);
                    }
                    Some(_) => {
                        return RelayRemoteResourceRegistrationReply {
                            status: RelayRemoteResourceRegistrationStatus::StaleRegistration,
                        };
                    }
                    None => {}
                }
            }
            if self
                .remote_owner_resources
                .lock()
                .await
                .contains_key(&msg.jid)
            {
                return RelayRemoteResourceRegistrationReply {
                    status: RelayRemoteResourceRegistrationStatus::StaleRegistration,
                };
            }
        }

        let (tx, rx) = mpsc::channel(REMOTE_RESOURCE_OUTBOUND_CHANNEL_SIZE);
        let entry = ConnectionEntry::new(tx);
        apply_remote_resource_state(&entry, &msg.state);
        let owner = entry.carbons_handle();
        let force_detach_rx = entry.take_force_detach_rx();
        match services
            .user_registry
            .ask(RegisterUserResourceIfOwnerOrAbsent {
                jid: msg.jid.clone(),
                entry: entry.clone(),
                owner: owner.clone(),
            })
            .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .await
        {
            Ok(true) => {
                let registration = RemoteOwnerRegistration {
                    registration_id: msg.registration_id,
                    socket_node: msg.socket_node.clone(),
                    socket_generation: msg.socket_generation,
                    owner: owner.clone(),
                };
                if !services
                    .connection_registry
                    .register_entry_if_owner_or_absent(msg.jid.clone(), entry.clone(), &owner)
                {
                    if !unregister_remote_owner_actor_entry(&services, &msg.jid, &owner).await {
                        return RelayRemoteResourceRegistrationReply {
                            status: RelayRemoteResourceRegistrationStatus::Unavailable,
                        };
                    }
                    return RelayRemoteResourceRegistrationReply {
                        status: RelayRemoteResourceRegistrationStatus::StaleRegistration,
                    };
                }
                match remote_owner_registration_is_current(&services, &msg.jid, &registration).await
                {
                    Ok(()) => {}
                    Err(status) => {
                        if !unregister_remote_owner_actor_entry(&services, &msg.jid, &owner).await {
                            return RelayRemoteResourceRegistrationReply {
                                status: RelayRemoteResourceRegistrationStatus::Unavailable,
                            };
                        }
                        services
                            .connection_registry
                            .unregister_if_owner(&msg.jid, &owner);
                        return RelayRemoteResourceRegistrationReply { status };
                    }
                }
                apply_remote_resource_presence_to_registry(
                    &services.connection_registry,
                    &msg.jid,
                    &owner,
                    msg.state.presence_available,
                    msg.state.presence_priority,
                    msg.state.presence_state.clone(),
                );
                self.remote_owner_resources
                    .lock()
                    .await
                    .insert(msg.jid.clone(), registration);
                self.spawn_remote_resource_forwarder(
                    msg.jid,
                    msg.registration_id,
                    msg.socket_node,
                    rx,
                    force_detach_rx,
                );
                RelayRemoteResourceRegistrationReply {
                    status: RelayRemoteResourceRegistrationStatus::Registered,
                }
            }
            Ok(false) => RelayRemoteResourceRegistrationReply {
                status: RelayRemoteResourceRegistrationStatus::StaleRegistration,
            },
            Err(error) => {
                tracing::warn!(
                    jid = %msg.jid,
                    %error,
                    "clustered remote-resource owner registration failed"
                );
                RelayRemoteResourceRegistrationReply {
                    status: RelayRemoteResourceRegistrationStatus::Unavailable,
                }
            }
        }
    }

    pub(crate) async fn unregister_remote_user_resource_on_owner(
        &self,
        msg: RelayUnregisterRemoteUserResource,
    ) -> RelayRemoteResourceUnregisterReply {
        let Some(services) = self.services.get().cloned() else {
            return RelayRemoteResourceUnregisterReply { removed: false };
        };
        let registration = self
            .remote_owner_resources
            .lock()
            .await
            .get(&msg.jid)
            .filter(|registration| {
                registration.registration_id == msg.registration_id
                    && registration.socket_generation == msg.socket_generation
            })
            .cloned();
        let Some(registration) = registration else {
            return RelayRemoteResourceUnregisterReply { removed: false };
        };
        let actor_removed = services
            .user_registry
            .ask(UnregisterUserResource {
                jid: msg.jid.clone(),
                owner: Some(registration.owner.clone()),
            })
            .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .await
            .is_ok();
        if !actor_removed {
            return RelayRemoteResourceUnregisterReply { removed: false };
        }
        let registry_removed = services
            .connection_registry
            .unregister_if_owner(&msg.jid, &registration.owner)
            .is_some();
        let mut registrations = self.remote_owner_resources.lock().await;
        if registrations.get(&msg.jid).is_some_and(|registration| {
            registration.registration_id == msg.registration_id
                && registration.socket_generation == msg.socket_generation
        }) {
            registrations.remove(&msg.jid);
        }
        RelayRemoteResourceUnregisterReply {
            removed: registry_removed,
        }
    }

    pub(crate) async fn update_remote_user_resource_on_owner(
        &self,
        msg: RelayUpdateRemoteUserResource,
    ) -> RelayRemoteResourceUpdateReply {
        let Some(services) = self.services.get().cloned() else {
            return RelayRemoteResourceUpdateReply {
                status: RelayRemoteResourceUpdateStatus::Unavailable,
            };
        };
        let registration = self
            .remote_owner_resources
            .lock()
            .await
            .get(&msg.jid)
            .filter(|registration| {
                registration.registration_id == msg.registration_id
                    && registration.socket_generation == msg.socket_generation
            })
            .cloned();
        let Some(registration) = registration else {
            return RelayRemoteResourceUpdateReply {
                status: RelayRemoteResourceUpdateStatus::StaleRegistration,
            };
        };
        let actor = match services
            .user_registry
            .ask(waddle_xmpp::registry::GetUser {
                bare_jid: msg.jid.to_bare(),
            })
            .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .await
        {
            Ok(Some(actor)) => actor,
            Ok(None) => {
                return RelayRemoteResourceUpdateReply {
                    status: RelayRemoteResourceUpdateStatus::StaleRegistration,
                };
            }
            Err(error) => {
                tracing::warn!(
                    jid = %msg.jid,
                    %error,
                    "clustered remote-resource state update could not resolve owner UserActor"
                );
                return RelayRemoteResourceUpdateReply {
                    status: RelayRemoteResourceUpdateStatus::Unavailable,
                };
            }
        };
        let jid = msg.jid;
        let status = match msg.update {
            RemoteResourceStateUpdate::Presence {
                available,
                priority,
                state,
            } => {
                match owner_remote_entry_if_current(
                    &actor,
                    &services.connection_registry,
                    &jid,
                    &registration.owner,
                )
                .await
                {
                    Ok(_) => {
                        if apply_remote_resource_presence_to_registry(
                            &services.connection_registry,
                            &jid,
                            &registration.owner,
                            available,
                            priority,
                            state,
                        ) {
                            RelayRemoteResourceUpdateStatus::Updated
                        } else {
                            RelayRemoteResourceUpdateStatus::StaleRegistration
                        }
                    }
                    Err(status) => status,
                }
            }
            RemoteResourceStateUpdate::Carbons { enabled } => {
                match owner_remote_entry_if_current(
                    &actor,
                    &services.connection_registry,
                    &jid,
                    &registration.owner,
                )
                .await
                {
                    Ok(entry) => {
                        entry.carbons_enabled.store(enabled, Ordering::Relaxed);
                        RelayRemoteResourceUpdateStatus::Updated
                    }
                    Err(status) => status,
                }
            }
            RemoteResourceStateUpdate::RosterInterested => match owner_remote_entry_if_current(
                &actor,
                &services.connection_registry,
                &jid,
                &registration.owner,
            )
            .await
            {
                Ok(entry) => {
                    entry.roster_interested.store(true, Ordering::Relaxed);
                    RelayRemoteResourceUpdateStatus::Updated
                }
                Err(status) => status,
            },
            RemoteResourceStateUpdate::BlocklistInterested => {
                match owner_remote_entry_if_current(
                    &actor,
                    &services.connection_registry,
                    &jid,
                    &registration.owner,
                )
                .await
                {
                    Ok(entry) => {
                        entry.blocklist_interested.store(true, Ordering::Relaxed);
                        RelayRemoteResourceUpdateStatus::Updated
                    }
                    Err(status) => status,
                }
            }
        };
        RelayRemoteResourceUpdateReply { status }
    }

    pub(crate) async fn apply_remote_user_side_effect_on_owner(
        &self,
        msg: RelayRemoteUserSideEffect,
    ) -> RelayRemoteUserSideEffectReply {
        let Some(services) = self.services.get().cloned() else {
            return RelayRemoteUserSideEffectReply {
                status: RelayRemoteUserSideEffectStatus::Unavailable,
            };
        };
        let registration = self
            .remote_owner_resources
            .lock()
            .await
            .get(&msg.source_jid)
            .filter(|registration| {
                registration.registration_id == msg.registration_id
                    && registration.socket_generation == msg.socket_generation
            })
            .cloned();
        let Some(registration) = registration else {
            return RelayRemoteUserSideEffectReply {
                status: RelayRemoteUserSideEffectStatus::StaleRegistration,
            };
        };
        let actor = match services
            .user_registry
            .ask(waddle_xmpp::registry::GetUser {
                bare_jid: msg.source_jid.to_bare(),
            })
            .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .await
        {
            Ok(Some(actor)) => actor,
            Ok(None) => {
                return RelayRemoteUserSideEffectReply {
                    status: RelayRemoteUserSideEffectStatus::StaleRegistration,
                };
            }
            Err(error) => {
                tracing::warn!(
                    jid = %msg.source_jid,
                    %error,
                    "clustered remote-user side effect could not resolve owner UserActor"
                );
                return RelayRemoteUserSideEffectReply {
                    status: RelayRemoteUserSideEffectStatus::Unavailable,
                };
            }
        };
        if let Err(status) = owner_remote_entry_if_current(
            &actor,
            &services.connection_registry,
            &msg.source_jid,
            &registration.owner,
        )
        .await
        {
            return RelayRemoteUserSideEffectReply {
                status: match status {
                    RelayRemoteResourceUpdateStatus::Updated => {
                        RelayRemoteUserSideEffectStatus::Applied
                    }
                    RelayRemoteResourceUpdateStatus::StaleRegistration => {
                        RelayRemoteUserSideEffectStatus::StaleRegistration
                    }
                    RelayRemoteResourceUpdateStatus::Unavailable => {
                        RelayRemoteUserSideEffectStatus::Unavailable
                    }
                },
            };
        }

        let status = match msg.effect {
            RemoteUserSideEffect::Carbons {
                owner,
                message,
                kind,
                exclude,
            } => match message.0 {
                Stanza::Message(message) => {
                    let web_socket_state = services.web_socket_state.upgrade();
                    crate::server::routes::interpret::carbons::send_carbons_to_registry(
                        &services.connection_registry,
                        Some(&services.sm_session_registry),
                        web_socket_state.as_deref(),
                        owner,
                        Box::new(message),
                        kind.into(),
                        exclude,
                    )
                    .await;
                    RelayRemoteUserSideEffectStatus::Applied
                }
                _ => RelayRemoteUserSideEffectStatus::StaleRegistration,
            },
            RemoteUserSideEffect::RosterPush {
                user_jid,
                source_jid,
                item,
                version,
            } => {
                let Some(state) = services.web_socket_state.upgrade() else {
                    return RelayRemoteUserSideEffectReply {
                        status: RelayRemoteUserSideEffectStatus::Unavailable,
                    };
                };
                crate::server::routes::websocket::handlers::iq::roster::push::send_roster_push_to_sibling_resources(
                    &state,
                    &user_jid,
                    &source_jid,
                    &item,
                    &version,
                )
                .await;
                RelayRemoteUserSideEffectStatus::Applied
            }
            RemoteUserSideEffect::BlocklistPush {
                user_bare,
                blocked,
                jids,
            } => {
                let Some(state) = services.web_socket_state.upgrade() else {
                    return RelayRemoteUserSideEffectReply {
                        status: RelayRemoteUserSideEffectStatus::Unavailable,
                    };
                };
                crate::server::routes::websocket::handlers::iq::blocking::send_blocking_pushes(
                    &state, &user_bare, blocked, &jids,
                )
                .await;
                RelayRemoteUserSideEffectStatus::Applied
            }
        };
        RelayRemoteUserSideEffectReply { status }
    }

    async fn route_remote_resource_origin(
        self: Arc<Self>,
        remote_origin: RemoteResourceOriginSnapshot,
        target: RemoteResourceRouteTarget,
        origin_stanza: &Stanza,
        origin: &OrderedRelayRouteOrigin,
    ) -> Option<FullJidDeliveryOutcome> {
        let outcome_log = route_outcome_log(&target);
        if let Some(handoff) = origin.handoff.clone() {
            if handoff.mark_deferred() {
                let bridge = Arc::clone(&self);
                let origin_stanza = origin_stanza.clone();
                tokio::spawn(async move {
                    let outcome = bridge
                        .route_remote_resource_origin_once(remote_origin, target)
                        .await
                        .unwrap_or(FullJidDeliveryOutcome::Dropped);
                    log_remote_resource_route_outcome(&outcome_log, outcome);
                    handoff.complete(replies_for_origin_handoff(&origin_stanza, outcome));
                });
                return Some(FullJidDeliveryOutcome::Delivered);
            }
        }
        let outcome = self
            .route_remote_resource_origin_once(remote_origin, target)
            .await;
        if let Some(outcome) = outcome {
            log_remote_resource_route_outcome(&outcome_log, outcome);
        }
        outcome
    }

    async fn route_remote_resource_origin_once(
        self: &Arc<Self>,
        remote_origin: RemoteResourceOriginSnapshot,
        target: RemoteResourceRouteTarget,
    ) -> Option<FullJidDeliveryOutcome> {
        let target_is_iq = route_target_stanza_is_iq(&target);
        let reply = self
            .ask_remote_resource_origin(&remote_origin, target.clone())
            .await;
        match reply {
            Ok(reply) if reply.outcome == RemoteResourceRouteOutcome::StaleRegistration => {
                match self.refresh_remote_resource_origin(&remote_origin).await {
                    RemoteResourceOriginRefresh::Remote(refreshed) => {
                        match self.ask_remote_resource_origin(&refreshed, target).await {
                            Ok(reply) => Some(reply.outcome.into()),
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    "clustered remote-resource origin route retry failed"
                                );
                                outcome_for_ask_error(&error, target_is_iq)
                                    .or(Some(FullJidDeliveryOutcome::Dropped))
                            }
                        }
                    }
                    RemoteResourceOriginRefresh::LocalOwner => {
                        self.route_remote_resource_target_from_local_origin(&remote_origin, target)
                            .await
                    }
                    RemoteResourceOriginRefresh::Failed => {
                        Some(FullJidDeliveryOutcome::Unavailable)
                    }
                }
            }
            Ok(reply) => Some(reply.outcome.into()),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "clustered remote-resource origin route ask failed"
                );
                if ask_error_allows_target_refresh(&error) {
                    match self.refresh_remote_resource_origin(&remote_origin).await {
                        RemoteResourceOriginRefresh::Remote(refreshed) => {
                            return match self.ask_remote_resource_origin(&refreshed, target).await {
                                Ok(reply) => Some(reply.outcome.into()),
                                Err(error) => {
                                    tracing::warn!(
                                        %error,
                                        "clustered remote-resource origin route retry failed"
                                    );
                                    outcome_for_ask_error(&error, target_is_iq)
                                        .or(Some(FullJidDeliveryOutcome::Dropped))
                                }
                            };
                        }
                        RemoteResourceOriginRefresh::LocalOwner => {
                            return self
                                .route_remote_resource_target_from_local_origin(
                                    &remote_origin,
                                    target,
                                )
                                .await;
                        }
                        RemoteResourceOriginRefresh::Failed => {}
                    }
                }
                outcome_for_ask_error(&error, target_is_iq)
                    .or(Some(FullJidDeliveryOutcome::Dropped))
            }
        }
    }

    async fn route_remote_resource_origin_muc(
        self: Arc<Self>,
        remote_origin: RemoteResourceOriginSnapshot,
        target: RemoteResourceRouteTarget,
        _origin_stanza: &Stanza,
        _origin: &OrderedRelayRouteOrigin,
    ) -> Option<OrderedRelayMucProxyOutcome> {
        // MUC joins mutate local membership state after this call returns, so
        // the socket node must observe the owner node's real result instead of
        // deferring through the SM handoff and reporting provisional success.
        self.route_remote_resource_origin_muc_once(remote_origin, target)
            .await
    }

    async fn route_remote_resource_origin_muc_once(
        self: &Arc<Self>,
        remote_origin: RemoteResourceOriginSnapshot,
        target: RemoteResourceRouteTarget,
    ) -> Option<OrderedRelayMucProxyOutcome> {
        let reply = self
            .ask_remote_resource_origin(&remote_origin, target.clone())
            .await;
        match reply {
            Ok(reply) if reply.outcome == RemoteResourceRouteOutcome::StaleRegistration => {
                match self.refresh_remote_resource_origin(&remote_origin).await {
                    RemoteResourceOriginRefresh::Remote(refreshed) => {
                        match self
                            .ask_remote_resource_origin(&refreshed, target.clone())
                            .await
                        {
                            Ok(reply) => Some(remote_resource_muc_outcome(reply)),
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    "clustered remote-resource MUC origin route retry failed"
                                );
                                Some(remote_resource_muc_ask_error_outcome(&target, &error))
                            }
                        }
                    }
                    RemoteResourceOriginRefresh::LocalOwner => {
                        self.route_remote_resource_muc_target_from_local_origin(
                            &remote_origin,
                            target,
                        )
                        .await
                    }
                    RemoteResourceOriginRefresh::Failed => {
                        Some(OrderedRelayMucProxyOutcome::Unavailable)
                    }
                }
            }
            Ok(reply) => Some(remote_resource_muc_outcome(reply)),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "clustered remote-resource MUC origin route ask failed"
                );
                if ask_error_allows_target_refresh(&error) {
                    match self.refresh_remote_resource_origin(&remote_origin).await {
                        RemoteResourceOriginRefresh::Remote(refreshed) => {
                            return match self
                                .ask_remote_resource_origin(&refreshed, target.clone())
                                .await
                            {
                                Ok(reply) => Some(remote_resource_muc_outcome(reply)),
                                Err(error) => {
                                    tracing::warn!(
                                        %error,
                                        "clustered remote-resource MUC origin route retry failed"
                                    );
                                    Some(remote_resource_muc_ask_error_outcome(&target, &error))
                                }
                            };
                        }
                        RemoteResourceOriginRefresh::LocalOwner => {
                            return self
                                .route_remote_resource_muc_target_from_local_origin(
                                    &remote_origin,
                                    target,
                                )
                                .await;
                        }
                        RemoteResourceOriginRefresh::Failed => {}
                    }
                }
                Some(remote_resource_muc_ask_error_outcome(&target, &error))
            }
        }
    }

    async fn ask_remote_resource_origin(
        &self,
        remote_origin: &RemoteResourceOriginSnapshot,
        target: RemoteResourceRouteTarget,
    ) -> Result<RelayRouteRemoteResourceStanzaReply, RelayAskError> {
        let mut handle =
            RelayHandle::new(remote_origin.user_owner.clone(), self.stop_token.clone())
                .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        handle
            .route_remote_resource_stanza(RelayRouteRemoteResourceStanza {
                source_jid: remote_origin.jid.clone(),
                registration_id: remote_origin.registration_id,
                socket_generation: remote_origin.socket_generation,
                target,
                trace: RelayTraceContext::default(),
            })
            .await
    }

    async fn route_remote_resource_target_from_local_origin(
        self: &Arc<Self>,
        remote_origin: &RemoteResourceOriginSnapshot,
        target: RemoteResourceRouteTarget,
    ) -> Option<FullJidDeliveryOutcome> {
        let services = self.services.get().cloned()?;
        let origin = local_origin_for_remote_resource(remote_origin);
        match target {
            RemoteResourceRouteTarget::FullJid { target, stanza } => {
                if let Some(remote) = self
                    .try_deliver_full_jid_remote(&target, &stanza.0, &origin)
                    .await
                {
                    Some(remote)
                } else {
                    Some(
                        deliver_local_full_jid_after_target_refresh(&services, &target, &stanza.0)
                            .await,
                    )
                }
            }
            RemoteResourceRouteTarget::BareJid { target, stanza } => {
                match route_local_bare_jid_with_timeout(&services, &target, &stanza.0, Some(origin))
                    .await
                {
                    Ok(replies) if replies.is_empty() => Some(FullJidDeliveryOutcome::Delivered),
                    Ok(_) => Some(FullJidDeliveryOutcome::Unavailable),
                    Err(OrderedRelayNackReason::TargetUnavailable) => {
                        Some(FullJidDeliveryOutcome::Unavailable)
                    }
                    Err(_) => Some(FullJidDeliveryOutcome::Dropped),
                }
            }
            RemoteResourceRouteTarget::MucProxy { .. } => Some(FullJidDeliveryOutcome::Dropped),
        }
    }

    async fn route_remote_resource_muc_target_from_local_origin(
        self: &Arc<Self>,
        remote_origin: &RemoteResourceOriginSnapshot,
        target: RemoteResourceRouteTarget,
    ) -> Option<OrderedRelayMucProxyOutcome> {
        let services = self.services.get().cloned()?;
        let RemoteResourceRouteTarget::MucProxy {
            room_jid,
            kind,
            stanza,
        } = target
        else {
            return Some(OrderedRelayMucProxyOutcome::Dropped);
        };
        let origin = local_origin_for_remote_resource(remote_origin);
        if let Some(remote) = self
            .try_proxy_muc_remote_from_local_origin(&room_jid, &stanza.0, kind, &origin)
            .await
        {
            Some(remote)
        } else {
            Some(muc_proxy_result_to_ordered_outcome(
                kind,
                Box::pin(deliver_reserved_muc_proxy(
                    &services, &room_jid, kind, &stanza.0,
                ))
                .await,
            ))
        }
    }

    async fn refresh_remote_resource_origin(
        self: &Arc<Self>,
        remote_origin: &RemoteResourceOriginSnapshot,
    ) -> RemoteResourceOriginRefresh {
        let Some(services) = self.services.get().cloned() else {
            return RemoteResourceOriginRefresh::Failed;
        };
        let owner = {
            let registrations = self.remote_socket_resources.lock().await;
            let Some(owner) = registrations
                .get(&remote_origin.jid)
                .filter(|registration| {
                    registration.registration_id == remote_origin.registration_id
                        && registration.socket_generation == remote_origin.socket_generation
                })
                .map(|registration| registration.owner.clone())
            else {
                return RemoteResourceOriginRefresh::Failed;
            };
            owner
        };
        let Some(entry) = services
            .connection_registry
            .entry_if_owner(&remote_origin.jid, &owner)
        else {
            return RemoteResourceOriginRefresh::Failed;
        };
        let target_entity = user_entity(&remote_origin.jid.to_bare());
        let Some(snapshot) = current_claim(&services, &target_entity).await else {
            return RemoteResourceOriginRefresh::Failed;
        };
        let me = services.node_identity.current();
        if snapshot.owner_lease_fresh && snapshot.owner == me {
            match crate::server::dual_registration::mirror_register_outcome(
                &services.user_registry,
                remote_origin.jid.clone(),
                entry,
            )
            .await
            {
                crate::server::dual_registration::MirrorRegisterOutcome::Registered => {}
                crate::server::dual_registration::MirrorRegisterOutcome::ForeignOwner
                | crate::server::dual_registration::MirrorRegisterOutcome::Failed => {
                    return RemoteResourceOriginRefresh::Failed;
                }
            }
            self.remove_remote_socket_registration_if_snapshot(remote_origin, &owner)
                .await;
            return RemoteResourceOriginRefresh::LocalOwner;
        }
        if !snapshot.owner_lease_fresh {
            return RemoteResourceOriginRefresh::Failed;
        }
        match self
            .try_register_remote_user_resource(&remote_origin.jid, entry, owner.clone())
            .await
        {
            RemoteResourceRegisterOutcome::Registered => self
                .remote_resource_origin_if_owner(&remote_origin.jid, &owner)
                .await
                .map(RemoteResourceOriginRefresh::Remote)
                .unwrap_or(RemoteResourceOriginRefresh::Failed),
            RemoteResourceRegisterOutcome::NotRemote => RemoteResourceOriginRefresh::Failed,
            RemoteResourceRegisterOutcome::Failed => RemoteResourceOriginRefresh::Failed,
        }
    }

    async fn remove_remote_socket_registration_if_snapshot(
        &self,
        remote_origin: &RemoteResourceOriginSnapshot,
        owner: &Arc<AtomicBool>,
    ) {
        let mut registrations = self.remote_socket_resources.lock().await;
        if registrations
            .get(&remote_origin.jid)
            .is_some_and(|registration| {
                registration.registration_id == remote_origin.registration_id
                    && registration.socket_generation == remote_origin.socket_generation
                    && Arc::ptr_eq(&registration.owner, owner)
            })
        {
            registrations.remove(&remote_origin.jid);
        }
    }

    pub(crate) async fn route_remote_resource_stanza_on_owner(
        self: &Arc<Self>,
        msg: RelayRouteRemoteResourceStanza,
    ) -> RelayRouteRemoteResourceStanzaReply {
        let Some(services) = self.services.get().cloned() else {
            return remote_resource_route_reply(RemoteResourceRouteOutcome::Dropped);
        };
        let Some(registration) = self
            .remote_owner_resources
            .lock()
            .await
            .get(&msg.source_jid)
            .filter(|registration| {
                registration.registration_id == msg.registration_id
                    && registration.socket_generation == msg.socket_generation
            })
            .cloned()
        else {
            return remote_resource_route_reply(RemoteResourceRouteOutcome::StaleRegistration);
        };
        let actor = match services
            .user_registry
            .ask(waddle_xmpp::registry::GetUser {
                bare_jid: msg.source_jid.to_bare(),
            })
            .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .await
        {
            Ok(Some(actor)) => actor,
            Ok(None) => {
                return remote_resource_route_reply(RemoteResourceRouteOutcome::StaleRegistration);
            }
            Err(error) => {
                tracing::warn!(
                    jid = %msg.source_jid,
                    %error,
                    "clustered remote-resource origin route could not resolve owner UserActor"
                );
                return remote_resource_route_reply(RemoteResourceRouteOutcome::Dropped);
            }
        };
        if let Err(status) = owner_remote_entry_if_current(
            &actor,
            &services.connection_registry,
            &msg.source_jid,
            &registration.owner,
        )
        .await
        {
            return remote_resource_route_reply(match status {
                RelayRemoteResourceUpdateStatus::Updated => RemoteResourceRouteOutcome::Delivered,
                RelayRemoteResourceUpdateStatus::StaleRegistration => {
                    RemoteResourceRouteOutcome::StaleRegistration
                }
                RelayRemoteResourceUpdateStatus::Unavailable => RemoteResourceRouteOutcome::Dropped,
            });
        }

        let sender_entity = user_entity(&msg.source_jid.to_bare());
        let origin = OrderedRelayRouteOrigin {
            kind: OrderedRelayRouteOriginKind::Entity(sender_entity.clone()),
            sender_entity,
            inbound_sequence: 0,
            handoff: None,
        };

        match msg.target {
            RemoteResourceRouteTarget::FullJid { target, stanza } => {
                let outcome = if let Some(remote) = self
                    .try_deliver_full_jid_remote(&target, &stanza.0, &origin)
                    .await
                {
                    remote
                } else if let Some(registered) = self
                    .try_deliver_registered_remote_resource(
                        &target,
                        &stanza.0,
                        DeliveryKind::PeerStanza,
                    )
                    .await
                {
                    registered
                } else {
                    deliver_local_full_jid_after_target_refresh(&services, &target, &stanza.0).await
                };
                remote_resource_route_reply(outcome.into())
            }
            RemoteResourceRouteTarget::BareJid { target, stanza } => {
                match route_local_bare_jid_with_timeout(&services, &target, &stanza.0, Some(origin))
                    .await
                {
                    Ok(replies) if replies.is_empty() => {
                        remote_resource_route_reply(RemoteResourceRouteOutcome::Delivered)
                    }
                    Ok(_) => remote_resource_route_reply(RemoteResourceRouteOutcome::Unavailable),
                    Err(OrderedRelayNackReason::TargetUnavailable) => {
                        remote_resource_route_reply(RemoteResourceRouteOutcome::Unavailable)
                    }
                    Err(_) => remote_resource_route_reply(RemoteResourceRouteOutcome::Dropped),
                }
            }
            RemoteResourceRouteTarget::MucProxy {
                room_jid,
                kind,
                stanza,
            } => {
                // This path can execute the payload locally WITHOUT the
                // envelope validation (`envelope_is_consistent` /
                // `validate_claims`) that the ordered-relay receiver
                // applies, so nothing here has bound the stanza's
                // `from` to the sending session. That is tolerable for
                // the presence/message kinds, but `MujiJingleIq` mints
                // a media credential from whatever `from` says (#1445),
                // so bind it to the registration this call already
                // authenticated (`msg.source_jid`) before dispatch.
                let stanza = if kind == OrderedRelayMucProxyKind::MujiJingleIq {
                    RemoteStanza(rebind_stanza_sender(&stanza.0, &msg.source_jid))
                } else {
                    stanza
                };
                let outcome = if let Some(remote) = self
                    .try_proxy_muc_remote(&room_jid, &stanza.0, kind, &origin)
                    .await
                {
                    remote
                } else {
                    muc_proxy_result_to_ordered_outcome(
                        kind,
                        deliver_reserved_muc_proxy(&services, &room_jid, kind, &stanza.0).await,
                    )
                };
                match outcome {
                    OrderedRelayMucProxyOutcome::Delivered(replies) => {
                        RelayRouteRemoteResourceStanzaReply {
                            outcome: RemoteResourceRouteOutcome::Delivered,
                            replies: replies.into_iter().map(RemoteStanza).collect(),
                        }
                    }
                    OrderedRelayMucProxyOutcome::Unavailable => {
                        remote_resource_route_reply(RemoteResourceRouteOutcome::Unavailable)
                    }
                    OrderedRelayMucProxyOutcome::Dropped => {
                        remote_resource_route_reply(RemoteResourceRouteOutcome::Dropped)
                    }
                    OrderedRelayMucProxyOutcome::MaybeCommitted => {
                        remote_resource_route_reply(RemoteResourceRouteOutcome::MaybeCommitted)
                    }
                    OrderedRelayMucProxyOutcome::JoinMaybeCommitted => {
                        remote_resource_route_reply(RemoteResourceRouteOutcome::JoinMaybeCommitted)
                    }
                }
            }
        }
    }

    pub(crate) async fn deliver_remote_resource_frame_on_socket(
        &self,
        msg: RelayDeliverRemoteResourceFrame,
    ) -> RelayRemoteResourceFrameReply {
        let Some(services) = self.services.get().cloned() else {
            return RelayRemoteResourceFrameReply {
                status: RelayRemoteResourceFrameStatus::Unavailable,
            };
        };
        let registration = {
            let registrations = self.remote_socket_resources.lock().await;
            registrations
                .get(&msg.frame.jid)
                .filter(|registration| registration.registration_id == msg.frame.registration_id)
                .cloned()
        };
        let Some(registration) = registration else {
            return RelayRemoteResourceFrameReply {
                status: RelayRemoteResourceFrameStatus::Unavailable,
            };
        };
        let outbound = OutboundStanza {
            stanza: msg.frame.stanza.0,
            kind: msg.frame.kind,
            pending_row_id: None,
            pending_row_original_receipt_at: None,
        };
        let outcome = services.connection_registry.try_send_outbound_if_owner(
            &msg.frame.jid,
            &registration.owner,
            outbound,
        );
        let status = match outcome {
            BroadcastOutcome::Delivered => RelayRemoteResourceFrameStatus::Delivered,
            BroadcastOutcome::DroppedFull => RelayRemoteResourceFrameStatus::Backpressure,
            BroadcastOutcome::NotConnected | BroadcastOutcome::DroppedClosed => {
                RelayRemoteResourceFrameStatus::Unavailable
            }
        };
        RelayRemoteResourceFrameReply { status }
    }

    async fn deliver_registered_remote_resource_with_registration(
        &self,
        target: &jid::FullJid,
        stanza: &Stanza,
        kind: DeliveryKind,
        registration: &RemoteOwnerRegistration,
    ) -> Option<FullJidDeliveryOutcome> {
        let mut handle =
            RelayHandle::new(registration.socket_node.clone(), self.stop_token.clone())
                .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        match handle
            .deliver_remote_resource_frame(RelayDeliverRemoteResourceFrame {
                frame: RemoteResourceOutboundFrame {
                    jid: target.clone(),
                    registration_id: registration.registration_id,
                    stanza: RemoteStanza(stanza.clone()),
                    kind,
                },
                trace: RelayTraceContext::default(),
            })
            .await
        {
            Ok(RelayRemoteResourceFrameReply {
                status: RelayRemoteResourceFrameStatus::Delivered,
            }) => {
                // The direct remote-resource path bypasses the counted
                // owner-node delivery channels AND the deliberately
                // uncounted socket endpoint, so the flagship delivered-
                // message counter must be bumped here — once, on the
                // owner node, upon the socket node's acknowledgment.
                if let Some(message_kind) =
                    waddle_xmpp::telemetry::messages::delivered_message_kind(stanza)
                {
                    waddle_xmpp::telemetry::messages::record_delivered_message(message_kind);
                }
                tracing::debug!(
                    jid = %target,
                    message_id = stanza_message_id(stanza),
                    outcome = ?FullJidDeliveryOutcome::Delivered,
                    "clustered remote-resource delivery outcome"
                );
                Some(FullJidDeliveryOutcome::Delivered)
            }
            Ok(RelayRemoteResourceFrameReply {
                status: RelayRemoteResourceFrameStatus::Backpressure,
            }) => {
                tracing::debug!(
                    jid = %target,
                    message_id = stanza_message_id(stanza),
                    outcome = ?FullJidDeliveryOutcome::Dropped,
                    "clustered remote-resource delivery outcome"
                );
                Some(FullJidDeliveryOutcome::Dropped)
            }
            Ok(RelayRemoteResourceFrameReply {
                status: RelayRemoteResourceFrameStatus::Unavailable,
            }) => {
                tracing::debug!(
                    jid = %target,
                    message_id = stanza_message_id(stanza),
                    outcome = ?FullJidDeliveryOutcome::Unavailable,
                    "clustered remote-resource delivery outcome"
                );
                self.cleanup_remote_owner_resource_if_registration(
                    target,
                    registration.registration_id,
                )
                .await;
                None
            }
            Err(error) => {
                tracing::warn!(
                    jid = %target,
                    message_id = stanza_message_id(stanza),
                    %error,
                    "clustered remote-resource acked delivery relay ask failed"
                );
                if ask_error_proves_remote_resource_ref_stale(&error) {
                    self.cleanup_remote_owner_resource_if_registration(
                        target,
                        registration.registration_id,
                    )
                    .await;
                    None
                } else {
                    Some(FullJidDeliveryOutcome::MaybeCommitted)
                }
            }
        }
    }

    pub(crate) async fn force_detach_remote_user_resource_on_socket(
        &self,
        msg: RelayForceDetachRemoteUserResource,
    ) -> RelayForceDetachRemoteUserResourceReply {
        let Some(services) = self.services.get().cloned() else {
            return RelayForceDetachRemoteUserResourceReply {
                outcome: ForceDetachOutcome::NotPersisted,
                status: RelayRemoteResourceForceDetachStatus::Unknown,
            };
        };
        let registration = {
            let registrations = self.remote_socket_resources.lock().await;
            registrations
                .get(&msg.jid)
                .filter(|registration| registration.registration_id == msg.registration_id)
                .cloned()
        };
        let Some(registration) = registration else {
            return RelayForceDetachRemoteUserResourceReply {
                outcome: ForceDetachOutcome::NotPersisted,
                status: RelayRemoteResourceForceDetachStatus::NotLive,
            };
        };
        let Some(entry) = services
            .connection_registry
            .entry_if_owner(&msg.jid, &registration.owner)
        else {
            return RelayForceDetachRemoteUserResourceReply {
                outcome: ForceDetachOutcome::NotPersisted,
                status: RelayRemoteResourceForceDetachStatus::NotLive,
            };
        };
        let (ack, ack_rx) = tokio::sync::oneshot::channel();
        let request = ForceDetachRequest {
            requester_bare_jid: msg.requester_bare_jid,
            ack,
        };
        if entry.force_detach_sender().try_send(request).is_err() {
            return RelayForceDetachRemoteUserResourceReply {
                outcome: ForceDetachOutcome::NotPersisted,
                status: RelayRemoteResourceForceDetachStatus::Unknown,
            };
        }
        let (outcome, status) =
            match tokio::time::timeout(ORDERED_DELIVERY_REPLY_TIMEOUT, ack_rx).await {
                Ok(Ok(ForceDetachOutcome::Detached)) => (
                    ForceDetachOutcome::Detached,
                    RelayRemoteResourceForceDetachStatus::Detached,
                ),
                Ok(Ok(ForceDetachOutcome::NotPersisted)) => (
                    ForceDetachOutcome::NotPersisted,
                    RelayRemoteResourceForceDetachStatus::Detached,
                ),
                Ok(Ok(ForceDetachOutcome::IdentityMismatch)) => (
                    ForceDetachOutcome::IdentityMismatch,
                    RelayRemoteResourceForceDetachStatus::Refused,
                ),
                Ok(Err(_)) | Err(_) => (
                    ForceDetachOutcome::NotPersisted,
                    RelayRemoteResourceForceDetachStatus::Unknown,
                ),
            };
        RelayForceDetachRemoteUserResourceReply { outcome, status }
    }

    async fn detach_stale_remote_socket_resource(
        &self,
        jid: &jid::FullJid,
        registration: &RemoteSocketRegistration,
    ) {
        let Some(services) = self.services.get().cloned() else {
            return;
        };
        {
            let mut registrations = self.remote_socket_resources.lock().await;
            if registrations.get(jid).is_some_and(|current| {
                current.registration_id == registration.registration_id
                    && current.socket_generation == registration.socket_generation
            }) {
                registrations.remove(jid);
            }
        }
        let Some(entry) = services
            .connection_registry
            .entry_if_owner(jid, &registration.owner)
        else {
            return;
        };
        let (ack, _ack_rx) = tokio::sync::oneshot::channel();
        let _ = entry.force_detach_sender().try_send(ForceDetachRequest {
            requester_bare_jid: jid.to_bare(),
            ack,
        });
    }

    async fn retire_remote_owner_registration(
        &self,
        services: &OrderedRelayDeliveryServices,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
    ) -> bool {
        let mut handle =
            RelayHandle::new(registration.socket_node.clone(), self.stop_token.clone())
                .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        let detach = handle
            .force_detach_remote_user_resource(RelayForceDetachRemoteUserResource {
                jid: jid.clone(),
                registration_id: registration.registration_id,
                requester_bare_jid: jid.to_bare(),
                trace: RelayTraceContext::default(),
            })
            .await;
        self.finish_remote_owner_registration_retire(services, jid, registration, detach)
            .await
    }

    async fn finish_remote_owner_registration_retire(
        &self,
        services: &OrderedRelayDeliveryServices,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
        detach: Result<RelayForceDetachRemoteUserResourceReply, RelayAskError>,
    ) -> bool {
        let reply = match detach {
            Ok(reply) => reply,
            Err(error) if ask_error_proves_remote_resource_ref_stale(&error) => {
                tracing::info!(
                    jid = %jid,
                    ?error,
                    "clustered remote-resource replacement cleaning stale old-socket mirror"
                );
                if !unregister_remote_owner_actor_entry(services, jid, &registration.owner).await {
                    return false;
                }
                services
                    .connection_registry
                    .unregister_if_owner(jid, &registration.owner);
                return true;
            }
            Err(error) => {
                tracing::warn!(
                    jid = %jid,
                    ?error,
                    "clustered remote-resource replacement refused uncertain old-socket detach"
                );
                return false;
            }
        };
        if !matches!(
            reply.status,
            RelayRemoteResourceForceDetachStatus::Detached
                | RelayRemoteResourceForceDetachStatus::NotLive
        ) {
            tracing::warn!(
                jid = %jid,
                status = ?reply.status,
                "clustered remote-resource replacement refused uncertain old-socket detach"
            );
            return false;
        }
        if !unregister_remote_owner_actor_entry(services, jid, &registration.owner).await {
            return false;
        }
        services
            .connection_registry
            .unregister_if_owner(jid, &registration.owner);
        true
    }

    async fn cleanup_remote_owner_resource_if_registration(
        &self,
        jid: &jid::FullJid,
        registration_id: RemoteResourceRegistrationId,
    ) {
        let Some(services) = self.services.get().cloned() else {
            return;
        };
        let registration = self
            .remote_owner_resources
            .lock()
            .await
            .get(jid)
            .filter(|registration| registration.registration_id == registration_id)
            .cloned();
        let Some(registration) = registration else {
            return;
        };
        let actor_removed = services
            .user_registry
            .ask(UnregisterUserResource {
                jid: jid.clone(),
                owner: Some(registration.owner.clone()),
            })
            .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .await
            .is_ok();
        if !actor_removed {
            return;
        }
        services
            .connection_registry
            .unregister_if_owner(jid, &registration.owner);
        let mut registrations = self.remote_owner_resources.lock().await;
        if registrations
            .get(jid)
            .is_some_and(|registration| registration.registration_id == registration_id)
        {
            registrations.remove(jid);
        }
    }

    fn spawn_remote_resource_forwarder(
        self: &Arc<Self>,
        jid: jid::FullJid,
        registration_id: RemoteResourceRegistrationId,
        socket_node: NodeId,
        mut rx: mpsc::Receiver<OutboundStanza>,
        force_detach_rx: Option<mpsc::Receiver<ForceDetachRequest>>,
    ) {
        let outbound_bridge = Arc::clone(self);
        let outbound_jid = jid.clone();
        let outbound_socket_node = socket_node.clone();
        tokio::spawn(async move {
            while let Some(outbound) = rx.recv().await {
                forward_remote_resource_outbound(
                    &outbound_bridge,
                    &outbound_jid,
                    registration_id,
                    &outbound_socket_node,
                    outbound,
                )
                .await;
            }
        });
        if let Some(mut force_detach_rx) = force_detach_rx {
            let control_bridge = Arc::clone(self);
            tokio::spawn(async move {
                while let Some(request) = force_detach_rx.recv().await {
                    forward_remote_resource_force_detach(
                        &control_bridge,
                        &jid,
                        registration_id,
                        &socket_node,
                        request,
                    )
                    .await;
                }
            });
        }
    }

    async fn try_proxy_muc_join_repair(
        self: Arc<Self>,
        room_jid: &jid::BareJid,
        stanza: &Stanza,
        original_origin: &OrderedRelayRouteOrigin,
    ) -> Option<RemoteDeliveryOutcome> {
        let Stanza::Presence(presence) = stanza else {
            return None;
        };
        let sender_jid = presence
            .from
            .as_ref()
            .and_then(|jid| jid.clone().try_into_full().ok())?;
        let services = self.services.get()?.clone();
        let target_entity = room_entity(room_jid);
        let target_snapshot = current_claim(&services, &target_entity).await?;
        if !target_snapshot.owner_lease_fresh {
            return None;
        }
        let me = services.node_identity.current();
        if target_snapshot.owner == me {
            return None;
        }

        let repair_origin_entity = user_entity(&sender_jid.to_bare());
        let (original_origin_entity, _) = route_origin_claim(&original_origin.kind);
        if repair_origin_entity == original_origin_entity {
            return None;
        }
        let origin_snapshot = current_claim(&services, &repair_origin_entity).await?;
        if !origin_snapshot.owner_lease_fresh || origin_snapshot.owner != me {
            tracing::warn!(
                room = %room_jid,
                sender = %sender_jid,
                origin_entity = %repair_origin_entity,
                "ordered relay: cannot repair maybe-committed MUC join because \
                 UserActor origin is not owned locally"
            );
            return None;
        }
        let sender_claim = OrderedRelayClaim {
            entity: repair_origin_entity.clone(),
            epoch: origin_snapshot.claim_epoch,
        };

        tracing::warn!(
            room = %room_jid,
            sender = %sender_jid,
            "ordered relay: retrying maybe-committed MUC join on UserActor repair channel"
        );
        let payload = OrderedRelayPayload::MucProxy {
            room_jid: room_jid.clone(),
            kind: OrderedRelayMucProxyKind::JoinPresence,
            stanza: RemoteStanza(stanza.clone()),
        };
        let channel = OrderedRelayChannel {
            origin: OrderedRelayOrigin::Entity(repair_origin_entity.clone()),
            recipient: OrderedRelayRecipient::Room(room_jid.clone()),
            target_epoch: target_snapshot.claim_epoch,
        };
        let seed = RemoteDeliverySeed {
            services,
            target_entity,
            previous_owner: target_snapshot.owner,
            channel,
            asserted_origin_node: NodeId::new(me.node_id.clone()),
            origin_inbound_sequence: OriginInboundSequence(original_origin.inbound_sequence),
            origin_claim: OrderedRelayClaim {
                entity: repair_origin_entity,
                epoch: origin_snapshot.claim_epoch,
            },
            sender_claim,
            target_claim: OrderedRelayClaim {
                entity: room_entity(room_jid),
                epoch: target_snapshot.claim_epoch,
            },
            payload,
            target: jid::Jid::from(room_jid.clone()),
            stanza: stanza.clone(),
            is_iq: false,
        };
        self.deliver_seeded_remote(seed, true).await
    }

    async fn deliver_seeded_remote(
        self: Arc<Self>,
        seed: RemoteDeliverySeed,
        allow_target_refresh_retry: bool,
    ) -> Option<RemoteDeliveryOutcome> {
        let channel = seed.channel.clone();
        let Some(lock) = self.lock_for_channel(&channel).await else {
            self.divert_channel(channel, OrderedRelayDiversionReason::Backpressure)
                .await;
            return Some(no_client_reply_outcome(definite_no_effect_outcome(
                seed.is_iq,
            )));
        };
        let outcome = {
            let _guard = lock.lock().await;
            match self.prepare_remote_delivery(seed).await {
                Ok(prepared) => {
                    Arc::clone(&self)
                        .deliver_prepared_remote(prepared, allow_target_refresh_retry)
                        .await
                }
                Err(outcome) => Some(no_client_reply_outcome(outcome)),
            }
        };
        self.remove_channel_lock_if_unused(&channel, &lock).await;
        outcome
    }

    async fn prepare_remote_delivery(
        &self,
        seed: RemoteDeliverySeed,
    ) -> Result<PreparedRemoteDelivery, FullJidDeliveryOutcome> {
        let mut envelope = {
            let mut sender = self.sender_state.lock().await;
            match sender.next_envelope(
                seed.asserted_origin_node,
                seed.channel.clone(),
                seed.origin_inbound_sequence,
                OrderedRelayEnvelopeClaims::new(
                    seed.origin_claim,
                    seed.sender_claim,
                    seed.target_claim,
                ),
                seed.payload,
            ) {
                Ok(envelope) => envelope,
                Err(diversion) => {
                    tracing::warn!(
                        target = %seed.target,
                        reason = ?diversion.reason,
                        "ordered relay: sender channel diverted; dropping to avoid \
                         reordering"
                    );
                    return Err(definite_no_effect_outcome(seed.is_iq));
                }
            }
        };
        let channel = envelope.channel.clone();
        if self.sign_envelope(&mut envelope).is_err() {
            self.divert_channel(channel, OrderedRelayDiversionReason::Unreachable)
                .await;
            return Err(definite_no_effect_outcome(seed.is_iq));
        }
        Ok(PreparedRemoteDelivery {
            services: seed.services,
            target_entity: seed.target_entity,
            previous_owner: seed.previous_owner,
            channel,
            envelope,
            target: seed.target,
            stanza: seed.stanza,
            is_iq: seed.is_iq,
        })
    }

    async fn lock_for_channel(&self, channel: &OrderedRelayChannel) -> Option<Arc<Mutex<()>>> {
        let mut locks = self.channel_locks.lock().await;
        if !locks.contains_key(channel) && locks.len() >= MAX_ORDERED_RELAY_CHANNEL_LOCKS {
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        }
        if !locks.contains_key(channel) && locks.len() >= MAX_ORDERED_RELAY_CHANNEL_LOCKS {
            tracing::warn!(
                limit = MAX_ORDERED_RELAY_CHANNEL_LOCKS,
                "ordered relay: channel lock map is full"
            );
            return None;
        }
        Some(
            locks
                .entry(channel.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone(),
        )
    }

    async fn remove_channel_lock_if_unused(
        &self,
        channel: &OrderedRelayChannel,
        lock: &Arc<Mutex<()>>,
    ) {
        let mut locks = self.channel_locks.lock().await;
        if locks
            .get(channel)
            .is_some_and(|existing| Arc::ptr_eq(existing, lock) && Arc::strong_count(lock) == 2)
        {
            locks.remove(channel);
        }
    }

    async fn lock_for_remote_owner_registration(
        &self,
        jid: &jid::FullJid,
    ) -> Option<Arc<Mutex<()>>> {
        let mut locks = self.remote_owner_registration_locks.lock().await;
        if !locks.contains_key(jid) && locks.len() >= MAX_REMOTE_OWNER_REGISTRATION_LOCKS {
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        }
        if !locks.contains_key(jid) && locks.len() >= MAX_REMOTE_OWNER_REGISTRATION_LOCKS {
            tracing::warn!(
                limit = MAX_REMOTE_OWNER_REGISTRATION_LOCKS,
                "clustered remote-resource registration lock map is full"
            );
            return None;
        }
        Some(
            locks
                .entry(jid.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone(),
        )
    }

    async fn remove_remote_owner_registration_lock_if_unused(
        &self,
        jid: &jid::FullJid,
        lock: &Arc<Mutex<()>>,
    ) {
        let mut locks = self.remote_owner_registration_locks.lock().await;
        if locks
            .get(jid)
            .is_some_and(|existing| Arc::ptr_eq(existing, lock) && Arc::strong_count(lock) == 2)
        {
            locks.remove(jid);
        }
    }

    async fn deliver_prepared_remote(
        self: Arc<Self>,
        prepared: PreparedRemoteDelivery,
        allow_target_refresh_retry: bool,
    ) -> Option<RemoteDeliveryOutcome> {
        let result = self
            .send_prepared_to_owner(&prepared.previous_owner, prepared.envelope.clone())
            .await;
        if allow_target_refresh_retry
            && matches!(
                &result,
                Ok(OrderedRelayReply::Nack(OrderedRelayNack {
                    reason: OrderedRelayNackReason::NotOwner {
                        role: OrderedRelayClaimRole::Target
                    },
                    ..
                }))
            )
        {
            if let Some(outcome) = Arc::clone(&self)
                .retry_after_target_owner_refresh(&prepared)
                .await
            {
                return Some(outcome);
            }
        }
        if allow_target_refresh_retry
            && matches!(&result, Err(error) if ask_error_allows_target_refresh(error))
        {
            if let Some(outcome) = Arc::clone(&self)
                .retry_after_target_owner_refresh(&prepared)
                .await
            {
                return Some(outcome);
            }
        }

        self.finish_prepared_delivery_result(prepared, result).await
    }

    async fn send_prepared_to_owner(
        &self,
        owner: &NodeIdentity,
        envelope: RemoteStanzaEnvelope,
    ) -> Result<OrderedRelayReply, RelayAskError> {
        let mut handle =
            RelayHandle::new(NodeId::new(owner.node_id.clone()), self.stop_token.clone())
                .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        handle.deliver_ordered(envelope).await
    }

    async fn finish_prepared_delivery_result(
        self: Arc<Self>,
        prepared: PreparedRemoteDelivery,
        result: Result<OrderedRelayReply, RelayAskError>,
    ) -> Option<RemoteDeliveryOutcome> {
        match result {
            Ok(OrderedRelayReply::Ack(ack)) => Some(RemoteDeliveryOutcome {
                delivery: FullJidDeliveryOutcome::Delivered,
                client_replies: ack
                    .client_replies
                    .into_iter()
                    .map(|remote| remote.0)
                    .collect(),
                maybe_committed: false,
                join_repair_allowed: false,
            }),
            Ok(OrderedRelayReply::Nack(nack)) => {
                let (outcome, channel_action, maybe_committed) = outcome_for_nack(
                    &prepared.services,
                    &prepared.target_entity,
                    &prepared.previous_owner,
                    &nack,
                    prepared.is_iq,
                )
                .await;
                self.apply_nack_channel_action(prepared.channel, channel_action)
                    .await;
                let join_repair_allowed =
                    maybe_committed && !matches!(nack.reason, OrderedRelayNackReason::InFlight);
                match outcome {
                    Some(outcome) => {
                        Some(no_client_reply_outcome_with_commit_state_and_join_repair(
                            outcome,
                            maybe_committed,
                            join_repair_allowed,
                        ))
                    }
                    None => Some(
                        deliver_local_after_target_refresh_outcome(
                            &prepared.services,
                            &prepared.target,
                            &prepared.stanza,
                            &prepared.envelope.payload,
                        )
                        .await,
                    ),
                }
            }
            Err(error) => {
                if matches!(error, RelayAskError::NotFound { .. }) {
                    self.sender_state
                        .lock()
                        .await
                        .rollback_unseen_envelope(&prepared.envelope);
                }
                if let Some(reason) = channel_diversion_for_ask_error(&error) {
                    self.divert_channel(prepared.channel, reason).await;
                }
                outcome_for_ask_error(&error, prepared.is_iq).map(|outcome| {
                    no_client_reply_outcome_with_commit_state(
                        outcome,
                        ask_error_maybe_committed(&error),
                    )
                })
            }
        }
    }

    async fn retry_after_target_owner_refresh(
        self: Arc<Self>,
        prepared: &PreparedRemoteDelivery,
    ) -> Option<RemoteDeliveryOutcome> {
        let snapshot = current_claim(&prepared.services, &prepared.target_entity).await?;
        if !snapshot.owner_lease_fresh {
            return None;
        }

        let me = prepared.services.node_identity.current();
        if snapshot.owner == me {
            self.forget_channel(&prepared.envelope.channel).await;
            return Some(
                deliver_local_after_target_refresh_outcome(
                    &prepared.services,
                    &prepared.target,
                    &prepared.stanza,
                    &prepared.envelope.payload,
                )
                .await,
            );
        }

        let new_channel = OrderedRelayChannel {
            origin: prepared.envelope.channel.origin.clone(),
            recipient: prepared.envelope.channel.recipient.clone(),
            target_epoch: snapshot.claim_epoch,
        };
        if new_channel == prepared.envelope.channel {
            return None;
        }

        if snapshot.owner == prepared.previous_owner
            && snapshot.claim_epoch == prepared.envelope.target_claim.epoch
        {
            return None;
        }

        self.forget_channel(&prepared.envelope.channel).await;

        tracing::debug!(
            entity_id = %prepared.target_entity.id,
            previous_owner = %prepared.previous_owner.node_id,
            refreshed_owner = %snapshot.owner.node_id,
            previous_epoch = prepared.envelope.target_claim.epoch.0,
            refreshed_epoch = snapshot.claim_epoch.0,
            "ordered relay: retrying target-owner NACK on refreshed ordered channel"
        );

        let seed = RemoteDeliverySeed {
            services: prepared.services.clone(),
            target_entity: prepared.target_entity.clone(),
            previous_owner: snapshot.owner,
            channel: new_channel.clone(),
            asserted_origin_node: prepared.envelope.asserted_origin_node.clone(),
            origin_inbound_sequence: prepared.envelope.origin_inbound_sequence,
            origin_claim: prepared.envelope.origin_claim.clone(),
            sender_claim: prepared.envelope.sender_claim.clone(),
            target_claim: OrderedRelayClaim {
                entity: prepared.target_entity.clone(),
                epoch: snapshot.claim_epoch,
            },
            payload: prepared.envelope.payload.clone(),
            target: prepared.target.clone(),
            stanza: prepared.stanza.clone(),
            is_iq: prepared.is_iq,
        };
        let Some(lock) = self.lock_for_channel(&new_channel).await else {
            self.divert_channel(new_channel, OrderedRelayDiversionReason::Backpressure)
                .await;
            return Some(no_client_reply_outcome(definite_no_effect_outcome(
                prepared.is_iq,
            )));
        };
        let outcome = {
            let _guard = lock.lock().await;
            match self.prepare_remote_delivery(seed).await {
                Ok(retry) => {
                    let result = self
                        .send_prepared_to_owner(&retry.previous_owner, retry.envelope.clone())
                        .await;
                    Arc::clone(&self)
                        .finish_prepared_delivery_result(retry, result)
                        .await
                }
                Err(outcome) => Some(no_client_reply_outcome(outcome)),
            }
        };
        self.remove_channel_lock_if_unused(&new_channel, &lock)
            .await;
        outcome
    }

    /// Receiver-side effect for one already-reserved envelope. The caller
    /// commits the reservation only when this returns `Ok(())`.
    pub async fn deliver_reserved(
        &self,
        envelope: &RemoteStanzaEnvelope,
    ) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
        let Some(services) = self.services.get().cloned() else {
            return Err(OrderedRelayNackReason::Unreachable);
        };
        validate_claims(&services, envelope).await?;
        match relay_payload_target(envelope)? {
            RelayPayloadTarget::Full(target, stanza) => self
                .deliver_reserved_full_jid(&services, target, stanza)
                .await
                .map(|()| Vec::new()),
            RelayPayloadTarget::Bare(target, stanza) => {
                deliver_reserved_bare_jid(&services, &target, stanza)
                    .await
                    .map(|()| Vec::new())
            }
            RelayPayloadTarget::Muc(room, kind, stanza) => {
                deliver_reserved_muc_proxy(&services, room, kind, stanza).await
            }
        }
    }

    fn sign_envelope(&self, envelope: &mut RemoteStanzaEnvelope) -> Result<(), ()> {
        let Some(signer) = self.origin_signer.get() else {
            tracing::warn!("ordered relay: origin signer is not wired; dropping envelope");
            return Err(());
        };
        let signing_bytes = envelope.signing_bytes().map_err(|error| {
            tracing::warn!(
                %error,
                "ordered relay: failed to serialize envelope signing bytes"
            );
        })?;
        let signature = signer.keypair.sign(&signing_bytes).map_err(|error| {
            tracing::warn!(
                %error,
                "ordered relay: failed to sign envelope"
            );
        })?;
        envelope.origin_proof = Some(OrderedRelayOriginProof {
            public_key: signer.public_key.clone(),
            signature,
        });
        Ok(())
    }

    async fn divert_channel(
        &self,
        channel: OrderedRelayChannel,
        reason: OrderedRelayDiversionReason,
    ) {
        self.sender_state
            .lock()
            .await
            .divert(OrderedRelayDiversion { channel, reason });
    }

    async fn forget_channel(&self, channel: &OrderedRelayChannel) {
        self.sender_state.lock().await.forget_channel(channel);
    }

    async fn apply_nack_channel_action(
        &self,
        channel: OrderedRelayChannel,
        action: NackChannelAction,
    ) {
        match action {
            NackChannelAction::Divert(reason) => self.divert_channel(channel, reason).await,
            NackChannelAction::Forget => self.forget_channel(&channel).await,
            NackChannelAction::Keep => {}
        }
    }
}

fn apply_remote_resource_presence_to_registry(
    registry: &ConnectionRegistry,
    jid: &jid::FullJid,
    owner: &Arc<AtomicBool>,
    available: bool,
    priority: i8,
    state: Option<RemotePresenceStateSnapshot>,
) -> bool {
    if !registry.update_presence_if_owner(jid, owner, available, priority) {
        return false;
    }
    if available {
        let state = state.map(PresenceState::from).unwrap_or(PresenceState {
            show: None,
            status: None,
            priority,
            payloads: Vec::new(),
        });
        registry.update_presence_state_if_owner(
            jid,
            owner,
            state.show,
            state.status,
            state.priority,
            state.payloads,
        )
    } else {
        registry.clear_presence_state_if_owner(jid, owner)
    }
}

async fn owner_remote_entry_if_current(
    actor: &ActorRef<waddle_xmpp::registry::user_actor::UserActor>,
    registry: &ConnectionRegistry,
    jid: &jid::FullJid,
    owner: &Arc<AtomicBool>,
) -> Result<ConnectionEntry, RelayRemoteResourceUpdateStatus> {
    let actor_entry = match actor
        .ask(waddle_xmpp::registry::GetConnectionEntry { jid: jid.clone() })
        .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .await
    {
        Ok(Some(entry)) => entry,
        Ok(None) => return Err(RelayRemoteResourceUpdateStatus::StaleRegistration),
        Err(_) => return Err(RelayRemoteResourceUpdateStatus::Unavailable),
    };
    if !Arc::ptr_eq(&actor_entry.carbons_enabled, owner) {
        return Err(RelayRemoteResourceUpdateStatus::StaleRegistration);
    }
    let Some(registry_entry) = registry.entry_if_owner(jid, owner) else {
        return Err(RelayRemoteResourceUpdateStatus::StaleRegistration);
    };
    if !Arc::ptr_eq(
        &registry_entry.carbons_enabled,
        &actor_entry.carbons_enabled,
    ) {
        return Err(RelayRemoteResourceUpdateStatus::StaleRegistration);
    }
    Ok(registry_entry)
}

async fn remote_owner_registration_is_current(
    services: &OrderedRelayDeliveryServices,
    jid: &jid::FullJid,
    registration: &RemoteOwnerRegistration,
) -> Result<(), RelayRemoteResourceRegistrationStatus> {
    let actor = match services
        .user_registry
        .ask(waddle_xmpp::registry::GetUser {
            bare_jid: jid.to_bare(),
        })
        .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .await
    {
        Ok(Some(actor)) => actor,
        Ok(None) => return Err(RelayRemoteResourceRegistrationStatus::StaleRegistration),
        Err(_) => return Err(RelayRemoteResourceRegistrationStatus::Unavailable),
    };
    owner_remote_entry_if_current(
        &actor,
        &services.connection_registry,
        jid,
        &registration.owner,
    )
    .await
    .map(|_| ())
    .map_err(|status| match status {
        RelayRemoteResourceUpdateStatus::Updated => {
            RelayRemoteResourceRegistrationStatus::Registered
        }
        RelayRemoteResourceUpdateStatus::StaleRegistration => {
            RelayRemoteResourceRegistrationStatus::StaleRegistration
        }
        RelayRemoteResourceUpdateStatus::Unavailable => {
            RelayRemoteResourceRegistrationStatus::Unavailable
        }
    })
}

async fn unregister_remote_owner_actor_entry(
    services: &OrderedRelayDeliveryServices,
    jid: &jid::FullJid,
    owner: &Arc<AtomicBool>,
) -> bool {
    match services
        .user_registry
        .ask(UnregisterUserResource {
            jid: jid.clone(),
            owner: Some(owner.clone()),
        })
        .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .await
    {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(
                jid = %jid,
                %error,
                "clustered remote-resource owner actor unregister failed"
            );
            false
        }
    }
}

fn remote_owner_registration_matches(
    left: &RemoteOwnerRegistration,
    right: &RemoteOwnerRegistration,
) -> bool {
    left.registration_id == right.registration_id
        && left.socket_node == right.socket_node
        && left.socket_generation == right.socket_generation
        && Arc::ptr_eq(&left.owner, &right.owner)
}

async fn forward_remote_resource_outbound(
    bridge: &Arc<OrderedRelayDeliveryBridge>,
    jid: &jid::FullJid,
    registration_id: RemoteResourceRegistrationId,
    socket_node: &NodeId,
    outbound: OutboundStanza,
) {
    if outbound.pending_row_id.is_some() {
        tracing::warn!(
            jid = %jid,
            "clustered remote-resource forwarder received pending-delivery \
             flush frame; dropping to avoid breaking SM row ack accounting"
        );
        return;
    }
    let kind = outbound.kind;
    let frame = RemoteResourceOutboundFrame {
        jid: jid.clone(),
        registration_id,
        stanza: RemoteStanza(outbound.stanza),
        kind,
    };
    let mut handle = RelayHandle::new(socket_node.clone(), bridge.stop_token.clone())
        .with_ask_timeouts(bridge.mailbox_timeout, bridge.reply_timeout);
    match handle
        .deliver_remote_resource_frame(RelayDeliverRemoteResourceFrame {
            frame,
            trace: RelayTraceContext::default(),
        })
        .await
    {
        Ok(RelayRemoteResourceFrameReply {
            status: RelayRemoteResourceFrameStatus::Delivered,
        }) => {}
        Ok(RelayRemoteResourceFrameReply {
            status: RelayRemoteResourceFrameStatus::Unavailable,
        }) => {
            tracing::debug!(
                jid = %jid,
                "clustered remote-resource socket registration unavailable; cleaning owner mirror"
            );
            bridge
                .cleanup_remote_owner_resource_if_registration(jid, registration_id)
                .await;
        }
        Ok(reply) => {
            tracing::debug!(
                jid = %jid,
                status = ?reply.status,
                "clustered remote-resource forwarder did not deliver frame"
            );
        }
        Err(error) => {
            tracing::warn!(
                jid = %jid,
                %error,
                "clustered remote-resource forwarder relay ask failed"
            );
            if ask_error_proves_remote_resource_ref_stale(&error) {
                bridge
                    .cleanup_remote_owner_resource_if_registration(jid, registration_id)
                    .await;
            }
        }
    }
}

async fn forward_remote_resource_force_detach(
    bridge: &Arc<OrderedRelayDeliveryBridge>,
    jid: &jid::FullJid,
    registration_id: RemoteResourceRegistrationId,
    socket_node: &NodeId,
    request: ForceDetachRequest,
) {
    let mut handle = RelayHandle::new(socket_node.clone(), bridge.stop_token.clone())
        .with_ask_timeouts(bridge.mailbox_timeout, bridge.reply_timeout);
    let outcome = match handle
        .force_detach_remote_user_resource(RelayForceDetachRemoteUserResource {
            jid: jid.clone(),
            registration_id,
            requester_bare_jid: request.requester_bare_jid,
            trace: RelayTraceContext::default(),
        })
        .await
    {
        Ok(reply) => reply.outcome,
        Err(error) => {
            tracing::warn!(
                jid = %jid,
                %error,
                "clustered remote-resource force-detach relay ask failed"
            );
            ForceDetachOutcome::NotPersisted
        }
    };
    let _ = request.ack.send(outcome);
}

impl OrderedRelayDeliveryBridge {
    async fn deliver_reserved_full_jid(
        &self,
        services: &OrderedRelayDeliveryServices,
        target: &jid::FullJid,
        stanza: &Stanza,
    ) -> Result<(), OrderedRelayNackReason> {
        if let Some(outcome) = self
            .try_deliver_registered_remote_resource(target, stanza, DeliveryKind::PeerStanza)
            .await
        {
            return match outcome {
                FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::QueuedDetached => {
                    Ok(())
                }
                FullJidDeliveryOutcome::Dropped => Err(OrderedRelayNackReason::Backpressure),
                FullJidDeliveryOutcome::MaybeCommitted => {
                    Err(OrderedRelayNackReason::MaybeCommitted)
                }
                FullJidDeliveryOutcome::Unavailable => {
                    Err(OrderedRelayNackReason::TargetUnavailable)
                }
            };
        }
        if matches!(stanza, Stanza::Iq(_)) {
            return deliver_reserved_full_jid_peer_live_only(services, target, stanza).await;
        }
        match crate::server::routes::interpret::deliver_peer_to_full(
            Some(&services.user_registry),
            Some(&services.sm_session_registry),
            target,
            stanza,
        )
        .await
        {
            FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::QueuedDetached => Ok(()),
            FullJidDeliveryOutcome::Dropped => Err(OrderedRelayNackReason::Backpressure),
            FullJidDeliveryOutcome::MaybeCommitted => Err(OrderedRelayNackReason::MaybeCommitted),
            FullJidDeliveryOutcome::Unavailable => Err(OrderedRelayNackReason::TargetUnavailable),
        }
    }
}

async fn deliver_reserved_full_jid_peer_live_only(
    services: &OrderedRelayDeliveryServices,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> Result<(), OrderedRelayNackReason> {
    let user_actor = services
        .user_registry
        .ask(waddle_xmpp::registry::GetUser {
            bare_jid: target.to_bare(),
        })
        .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .await
        .map_err(|error| {
            tracing::warn!(
                target = %target,
                %error,
                "ordered relay: failed to resolve target UserActor for full-JID IQ"
            );
            OrderedRelayNackReason::Unreachable
        })?;
    let Some(user_actor) = user_actor else {
        return Err(OrderedRelayNackReason::TargetUnavailable);
    };

    match user_actor
        .ask(waddle_xmpp::registry::TrySendPeer {
            jid: target.clone(),
            stanza: stanza.clone(),
        })
        .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .await
    {
        Ok(BroadcastOutcome::Delivered) => Ok(()),
        Ok(BroadcastOutcome::NotConnected | BroadcastOutcome::DroppedClosed) => {
            Err(OrderedRelayNackReason::TargetUnavailable)
        }
        Ok(BroadcastOutcome::DroppedFull) => Err(OrderedRelayNackReason::Backpressure),
        Err(error) => {
            tracing::warn!(
                target = %target,
                %error,
                "ordered relay: live-only full-JID IQ peer delivery failed"
            );
            Err(OrderedRelayNackReason::InFlight)
        }
    }
}

async fn deliver_reserved_bare_jid(
    services: &OrderedRelayDeliveryServices,
    target: &jid::BareJid,
    stanza: &Stanza,
) -> Result<(), OrderedRelayNackReason> {
    if matches!(
        stanza,
        Stanza::Presence(presence) if !is_server_handled_presence_request(presence)
    ) {
        return deliver_reserved_bare_presence_direct(services, target, stanza).await;
    }

    let sender_entity = user_entity(target);
    let origin = OrderedRelayRouteOrigin {
        kind: OrderedRelayRouteOriginKind::Entity(sender_entity.clone()),
        sender_entity,
        inbound_sequence: 0,
        handoff: None,
    };
    let replies = route_local_bare_jid_with_timeout(services, target, stanza, Some(origin)).await?;
    if !replies.is_empty() {
        tracing::warn!(
            bare_jid = %target,
            reply_count = replies.len(),
            "ordered relay: receiver-side bare-JID delivery produced local fallback replies"
        );
        return Err(OrderedRelayNackReason::TargetUnavailable);
    }
    Ok(())
}

async fn deliver_reserved_bare_presence_direct(
    services: &OrderedRelayDeliveryServices,
    target: &jid::BareJid,
    stanza: &Stanza,
) -> Result<(), OrderedRelayNackReason> {
    if remote_presence_blocked_for_recipient(services, target, stanza).await? {
        tracing::debug!(
            bare_jid = %target,
            "ordered relay: dropping bare-JID presence from blocked sender"
        );
        return Ok(());
    }

    let live_targets =
        waddle_xmpp::registry::available_resources_for_user(&services.user_registry, target).await;
    let live_set: std::collections::HashSet<jid::FullJid> =
        live_targets.iter().map(|(jid, _)| jid.clone()).collect();
    let mut landed = false;
    for resource in live_targets.into_iter().map(|(jid, _)| jid) {
        match deliver_direct_or_registered_remote_resource(services, &resource, stanza).await {
            FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::QueuedDetached => {
                landed = true;
            }
            FullJidDeliveryOutcome::Unavailable | FullJidDeliveryOutcome::Dropped => {}
            FullJidDeliveryOutcome::MaybeCommitted => {
                landed = true;
            }
        }
    }

    match services
        .sm_session_registry
        .available_detached_resources_for_user(target)
        .await
    {
        Ok(detached) => {
            for resource in detached {
                if live_set.contains(&resource) {
                    continue;
                }
                match services
                    .sm_session_registry
                    .record_stanza_for_detached_resource(&resource, stanza, chrono::Utc::now())
                    .await
                {
                    Ok(true) => {
                        landed = true;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        tracing::warn!(
                            resource = %resource,
                            %error,
                            "ordered relay: failed to record bare-JID presence for detached resource"
                        );
                    }
                }
            }
        }
        Err(error) => {
            tracing::warn!(
                bare_jid = %target,
                %error,
                "ordered relay: failed to enumerate detached resources for bare-JID presence"
            );
        }
    }

    if landed {
        Ok(())
    } else {
        Err(OrderedRelayNackReason::TargetUnavailable)
    }
}

async fn deliver_direct_or_registered_remote_resource(
    services: &OrderedRelayDeliveryServices,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> FullJidDeliveryOutcome {
    if let Some(state) = services.web_socket_state.upgrade() {
        if let Some(bridge) = state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .as_ref()
        {
            if let Some(outcome) = bridge
                .try_deliver_registered_remote_resource(target, stanza, DeliveryKind::DirectFrame)
                .await
            {
                return outcome;
            }
        }
    }
    crate::server::routes::interpret::deliver_direct_to_full(
        Some(&services.user_registry),
        Some(&services.sm_session_registry),
        target,
        stanza,
    )
    .await
}

async fn remote_presence_blocked_for_recipient(
    services: &OrderedRelayDeliveryServices,
    target: &jid::BareJid,
    stanza: &Stanza,
) -> Result<bool, OrderedRelayNackReason> {
    let Stanza::Presence(presence) = stanza else {
        return Ok(false);
    };
    let Some(sender) = presence.from.as_ref() else {
        return Ok(false);
    };
    let entries = services
        .blocking_storage
        .list_blocked_jid_entries(target)
        .await
        .map_err(|error| {
            tracing::warn!(
                bare_jid = %target,
                sender = %sender,
                %error,
                "ordered relay: failed to load recipient blocklist for remote presence"
            );
            OrderedRelayNackReason::InFlight
        })?;
    Ok(waddle_xmpp::protocol::Blocklist::new(entries).contains_jid(sender))
}

async fn route_local_bare_jid_with_timeout(
    services: &OrderedRelayDeliveryServices,
    target: &jid::BareJid,
    stanza: &Stanza,
    origin: Option<OrderedRelayRouteOrigin>,
) -> Result<Vec<Stanza>, OrderedRelayNackReason> {
    let Some(state) = services.web_socket_state.upgrade() else {
        tracing::warn!(
            bare_jid = %target,
            "ordered relay: WebSocket state is gone; cannot deliver bare-JID relay payload"
        );
        return Err(OrderedRelayNackReason::Unreachable);
    };
    if let (Stanza::Presence(presence), Some(origin)) = (stanza, origin.clone()) {
        if is_server_handled_presence_request(presence) {
            return match tokio::time::timeout(
                ORDERED_RECEIVER_DELIVERY_TIMEOUT,
                crate::server::routes::websocket::handlers::presence::handle_ordered_relay_presence_request(
                    state.as_ref(),
                    target,
                    presence.clone(),
                    origin,
                ),
            )
            .await
            {
                Ok(Ok(())) => Ok(Vec::new()),
                Ok(Err(())) => Err(OrderedRelayNackReason::ParseFailure),
                Err(_) => {
                    tracing::warn!(
                        bare_jid = %target,
                        timeout_ms = ORDERED_RECEIVER_DELIVERY_TIMEOUT.as_millis(),
                        "ordered relay: local presence request handling timed out"
                    );
                    Err(OrderedRelayNackReason::MaybeCommitted)
                }
            };
        }
    }
    let deps = build_interpret_deps(state.as_ref(), None).with_ordered_relay_origin(origin);
    match tokio::time::timeout(
        ORDERED_RECEIVER_DELIVERY_TIMEOUT,
        crate::server::routes::interpret::route_to_connection(
            &deps,
            jid::Jid::from(target.clone()),
            Box::new(stanza.clone()),
            0,
        ),
    )
    .await
    {
        Ok(replies) => Ok(replies),
        Err(_) => {
            tracing::warn!(
                bare_jid = %target,
                timeout_ms = ORDERED_RECEIVER_DELIVERY_TIMEOUT.as_millis(),
                "ordered relay: local bare-JID delivery timed out"
            );
            Err(OrderedRelayNackReason::MaybeCommitted)
        }
    }
}

fn is_server_handled_presence_request(presence: &xmpp_parsers::presence::Presence) -> bool {
    matches!(
        presence.type_,
        xmpp_parsers::presence::Type::Probe
            | xmpp_parsers::presence::Type::Subscribe
            | xmpp_parsers::presence::Type::Subscribed
            | xmpp_parsers::presence::Type::Unsubscribe
            | xmpp_parsers::presence::Type::Unsubscribed
    )
}

async fn current_claim(
    services: &OrderedRelayDeliveryServices,
    entity: &Entity,
) -> Option<ClaimSnapshot> {
    match services.claim_store.current_claim(entity).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::warn!(
                entity = %entity,
                %error,
                "ordered relay: claim lookup failed"
            );
            None
        }
    }
}

fn user_entity(bare: &jid::BareJid) -> Entity {
    Entity::new(EntityType::UserActor, bare.to_string())
}

fn room_entity(room: &jid::BareJid) -> Entity {
    Entity::new(EntityType::RoomActor, room.to_string())
}

fn no_client_reply_outcome(delivery: FullJidDeliveryOutcome) -> RemoteDeliveryOutcome {
    no_client_reply_outcome_with_commit_state(delivery, false)
}

fn remote_resource_route_reply(
    outcome: RemoteResourceRouteOutcome,
) -> RelayRouteRemoteResourceStanzaReply {
    RelayRouteRemoteResourceStanzaReply {
        outcome,
        replies: Vec::new(),
    }
}

fn remote_resource_muc_outcome(
    reply: RelayRouteRemoteResourceStanzaReply,
) -> OrderedRelayMucProxyOutcome {
    match reply.outcome {
        RemoteResourceRouteOutcome::Delivered | RemoteResourceRouteOutcome::QueuedDetached => {
            OrderedRelayMucProxyOutcome::Delivered(
                reply.replies.into_iter().map(|reply| reply.0).collect(),
            )
        }
        RemoteResourceRouteOutcome::Unavailable | RemoteResourceRouteOutcome::StaleRegistration => {
            OrderedRelayMucProxyOutcome::Unavailable
        }
        RemoteResourceRouteOutcome::Dropped => OrderedRelayMucProxyOutcome::Dropped,
        RemoteResourceRouteOutcome::MaybeCommitted => OrderedRelayMucProxyOutcome::MaybeCommitted,
        RemoteResourceRouteOutcome::JoinMaybeCommitted => {
            OrderedRelayMucProxyOutcome::JoinMaybeCommitted
        }
    }
}

fn remote_resource_muc_ask_error_outcome(
    target: &RemoteResourceRouteTarget,
    error: &RelayAskError,
) -> OrderedRelayMucProxyOutcome {
    if !ask_error_maybe_committed(error) {
        return OrderedRelayMucProxyOutcome::Dropped;
    }
    match target {
        RemoteResourceRouteTarget::MucProxy {
            kind: OrderedRelayMucProxyKind::JoinPresence,
            ..
        } => OrderedRelayMucProxyOutcome::JoinMaybeCommitted,
        RemoteResourceRouteTarget::MucProxy { .. } => OrderedRelayMucProxyOutcome::MaybeCommitted,
        RemoteResourceRouteTarget::FullJid { .. } | RemoteResourceRouteTarget::BareJid { .. } => {
            OrderedRelayMucProxyOutcome::Dropped
        }
    }
}

fn no_client_reply_outcome_with_commit_state(
    delivery: FullJidDeliveryOutcome,
    maybe_committed: bool,
) -> RemoteDeliveryOutcome {
    no_client_reply_outcome_with_commit_state_and_join_repair(
        delivery,
        maybe_committed,
        maybe_committed,
    )
}

fn no_client_reply_outcome_with_commit_state_and_join_repair(
    delivery: FullJidDeliveryOutcome,
    maybe_committed: bool,
    join_repair_allowed: bool,
) -> RemoteDeliveryOutcome {
    RemoteDeliveryOutcome {
        delivery,
        client_replies: Vec::new(),
        maybe_committed,
        join_repair_allowed,
    }
}

fn route_origin_claim(kind: &OrderedRelayRouteOriginKind) -> (Entity, OrderedRelayOrigin) {
    match kind {
        OrderedRelayRouteOriginKind::SmSession(stream_id) => (
            Entity::new(EntityType::SmSession, stream_id.to_string()),
            OrderedRelayOrigin::SmSession(stream_id.clone()),
        ),
        OrderedRelayRouteOriginKind::Entity(entity) => {
            (entity.clone(), OrderedRelayOrigin::Entity(entity.clone()))
        }
        OrderedRelayRouteOriginKind::RemoteResource(remote) => {
            let entity = user_entity(&remote.jid.to_bare());
            (entity.clone(), OrderedRelayOrigin::Entity(entity))
        }
    }
}

fn remote_resource_origin(
    origin: &OrderedRelayRouteOrigin,
) -> Option<RemoteResourceOriginSnapshot> {
    match &origin.kind {
        OrderedRelayRouteOriginKind::RemoteResource(remote) => Some(remote.clone()),
        OrderedRelayRouteOriginKind::SmSession(_) | OrderedRelayRouteOriginKind::Entity(_) => None,
    }
}

fn local_origin_for_remote_resource(
    remote_origin: &RemoteResourceOriginSnapshot,
) -> OrderedRelayRouteOrigin {
    let sender_entity = user_entity(&remote_origin.jid.to_bare());
    OrderedRelayRouteOrigin {
        kind: OrderedRelayRouteOriginKind::Entity(sender_entity.clone()),
        sender_entity,
        inbound_sequence: 0,
        handoff: None,
    }
}

async fn current_fresh_local_relay_claim(
    services: &OrderedRelayDeliveryServices,
    entity: &Entity,
    me: &NodeIdentity,
    role: &'static str,
) -> Option<OrderedRelayClaim> {
    let snapshot = current_claim(services, entity).await?;
    if !snapshot.owner_lease_fresh || snapshot.owner != *me {
        tracing::debug!(
            entity = %entity,
            role,
            "ordered relay: entity is not currently owned locally; keeping local fallback path"
        );
        return None;
    }
    Some(OrderedRelayClaim {
        entity: entity.clone(),
        epoch: snapshot.claim_epoch,
    })
}

fn payload_for_recipient(recipient: jid::Jid, stanza: &Stanza) -> Option<OrderedRelayPayload> {
    match stanza {
        Stanza::Message(message)
            if message.type_ == xmpp_parsers::message::MessageType::Groupchat =>
        {
            None
        }
        Stanza::Message(_) => Some(OrderedRelayPayload::Message {
            recipient,
            stanza: RemoteStanza(stanza.clone()),
        }),
        Stanza::Iq(_) => Some(OrderedRelayPayload::Iq {
            recipient,
            stanza: RemoteStanza(stanza.clone()),
        }),
        Stanza::Presence(_) => Some(OrderedRelayPayload::Presence {
            recipient,
            stanza: RemoteStanza(stanza.clone()),
        }),
    }
}

fn apply_remote_resource_state(entry: &ConnectionEntry, state: &RemoteResourceStateSnapshot) {
    entry
        .carbons_enabled
        .store(state.carbons_enabled, Ordering::Relaxed);
    entry
        .roster_interested
        .store(state.roster_interested, Ordering::Relaxed);
    entry
        .blocklist_interested
        .store(state.blocklist_interested, Ordering::Relaxed);
    entry
        .presence_available
        .store(state.presence_available, Ordering::Relaxed);
    entry
        .presence_priority
        .store(state.presence_priority, Ordering::Relaxed);
}

enum RelayPayloadTarget<'a> {
    Full(&'a jid::FullJid, &'a Stanza),
    Bare(jid::BareJid, &'a Stanza),
    Muc(&'a jid::BareJid, OrderedRelayMucProxyKind, &'a Stanza),
}

fn relay_payload_target(
    envelope: &RemoteStanzaEnvelope,
) -> Result<RelayPayloadTarget<'_>, OrderedRelayNackReason> {
    let (recipient, stanza) = match &envelope.payload {
        OrderedRelayPayload::Message { recipient, stanza }
        | OrderedRelayPayload::Iq { recipient, stanza }
        | OrderedRelayPayload::Presence { recipient, stanza } => Ok((recipient, &stanza.0)),
        OrderedRelayPayload::MucProxy {
            room_jid,
            kind,
            stanza,
        } => return Ok(RelayPayloadTarget::Muc(room_jid, *kind, &stanza.0)),
    }?;
    match &envelope.channel.recipient {
        OrderedRelayRecipient::FullJid(full) if recipient == &jid::Jid::from(full.clone()) => {
            Ok(RelayPayloadTarget::Full(full, stanza))
        }
        OrderedRelayRecipient::BareJid(bare) if recipient == &jid::Jid::from(bare.clone()) => {
            Ok(RelayPayloadTarget::Bare(bare.clone(), stanza))
        }
        OrderedRelayRecipient::FullJid(_) | OrderedRelayRecipient::BareJid(_) => {
            Err(OrderedRelayNackReason::ParseFailure)
        }
        OrderedRelayRecipient::Room(_) => Err(OrderedRelayNackReason::ParseFailure),
    }
}

/// Return `stanza` with its `from` replaced by `sender`.
///
/// Used where a locally-executed relay payload must be attributed to
/// the session the caller already authenticated rather than to
/// whatever the serialized stanza claims (#1445).
fn rebind_stanza_sender(stanza: &Stanza, sender: &jid::FullJid) -> Stanza {
    let mut rebound = stanza.clone();
    let from = Some(jid::Jid::from(sender.clone()));
    match &mut rebound {
        Stanza::Iq(iq) => match iq.as_mut() {
            xmpp_parsers::iq::Iq::Get { from: f, .. }
            | xmpp_parsers::iq::Iq::Set { from: f, .. }
            | xmpp_parsers::iq::Iq::Result { from: f, .. }
            | xmpp_parsers::iq::Iq::Error { from: f, .. } => *f = from,
        },
        Stanza::Message(message) => message.from = from,
        Stanza::Presence(presence) => presence.from = from,
    }
    rebound
}

async fn deliver_reserved_muc_proxy(
    services: &OrderedRelayDeliveryServices,
    room_jid: &jid::BareJid,
    kind: OrderedRelayMucProxyKind,
    stanza: &Stanza,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    let Some(state) = services.web_socket_state.upgrade() else {
        tracing::warn!(
            room = %room_jid,
            "ordered relay: WebSocket state is gone; cannot deliver MUC relay payload"
        );
        return Err(OrderedRelayNackReason::Unreachable);
    };
    match (kind, stanza) {
        (OrderedRelayMucProxyKind::JoinPresence, Stanza::Presence(presence)) => {
            deliver_reserved_muc_join(state.as_ref(), room_jid, presence).await
        }
        (OrderedRelayMucProxyKind::GroupchatMessage, Stanza::Message(message)) => {
            deliver_reserved_muc_groupchat(state.as_ref(), room_jid, message).await
        }
        (OrderedRelayMucProxyKind::OccupantPresence, Stanza::Presence(presence)) => {
            deliver_reserved_muc_occupant_presence(state.as_ref(), room_jid, presence).await
        }
        (OrderedRelayMucProxyKind::MujiJingleIq, Stanza::Iq(iq)) => {
            deliver_reserved_muji_iq(state.as_ref(), room_jid, iq).await
        }
        _ => Err(OrderedRelayNackReason::ParseFailure),
    }
}

/// #1445: execute a relayed Muji `session-initiate` on this node — the
/// room-claim owner — and carry the reply frames (IQ ack +
/// server-initiated `session-accept` with the LiveKit token) back to
/// the origin node as client replies. Envelope validation has already
/// bound the payload's `<muji room>` to `room_jid`, so the room
/// binding needs no re-check here; the executor re-runs the membership
/// gate against the local room actor and is terminal (a denial comes
/// back as a delivered IQ-error frame, never a NACK, so no relay
/// loop can form).
async fn deliver_reserved_muji_iq(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    iq: &xmpp_parsers::iq::Iq,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    let frames = tokio::time::timeout(
        ORDERED_RECEIVER_DELIVERY_TIMEOUT,
        crate::server::routes::websocket::handlers::iq::jingle_muji_relay::handle_relayed_muji_initiate(
            state, iq,
        ),
    )
    .await
    .map_err(|_| {
        tracing::warn!(
            room = %room_jid,
            timeout_ms = ORDERED_RECEIVER_DELIVERY_TIMEOUT.as_millis(),
            "ordered relay: Muji Jingle execution timed out"
        );
        OrderedRelayNackReason::MaybeCommitted
    })?
    .ok_or(OrderedRelayNackReason::ParseFailure)?;
    remote_replies_from_frames(frames)
}

async fn deliver_reserved_muc_occupant_presence(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    presence: &xmpp_parsers::presence::Presence,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    if presence.type_ == xmpp_parsers::presence::Type::Unavailable {
        return deliver_reserved_muc_leave(state, room_jid, presence).await;
    }
    deliver_reserved_muc_update(state, room_jid, presence).await
}

async fn deliver_reserved_muc_join(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    presence: &xmpp_parsers::presence::Presence,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    let Some(sender_jid) = presence
        .from
        .as_ref()
        .and_then(|jid| jid.clone().try_into_full().ok())
    else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    let Some(to) = presence.to.as_ref() else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    let Some(nick) = to.resource().map(|resource| resource.as_str()) else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    if to.to_bare() != *room_jid {
        return Err(OrderedRelayNackReason::ParseFailure);
    }
    let presence_show = presence
        .show
        .clone()
        .map(crate::notification_activity::NotificationPresenceShow::from_xep0045);
    let synthetic_session = synthetic_session_for_full_jid(&sender_jid);
    let frames = crate::server::routes::websocket::handlers::presence::handle_muc_join(
        state,
        state.deps.auth_state.xmpp_domain.as_str(),
        room_jid,
        &sender_jid,
        nick,
        presence_show,
        &Some(synthetic_session),
    )
    .await;
    remote_replies_from_frames(frames)
}

async fn deliver_reserved_muc_update(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    presence: &xmpp_parsers::presence::Presence,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    let Some(sender_jid) = presence
        .from
        .as_ref()
        .and_then(|jid| jid.clone().try_into_full().ok())
    else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    let Some(to) = presence.to.as_ref() else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    let Some(nick) = to.resource().map(|resource| resource.as_str()) else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    if to.to_bare() != *room_jid {
        return Err(OrderedRelayNackReason::ParseFailure);
    }

    match crate::server::routes::websocket::handlers::presence::try_handle_muc_presence_update(
        state,
        room_jid,
        &sender_jid,
        nick,
        presence,
    )
    .await
    {
        Some(frames) => remote_replies_from_frames(frames),
        None => Err(OrderedRelayNackReason::TargetUnavailable),
    }
}

async fn deliver_reserved_muc_leave(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    presence: &xmpp_parsers::presence::Presence,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    let Some(sender_jid) = presence
        .from
        .as_ref()
        .and_then(|jid| jid.clone().try_into_full().ok())
    else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    let Some(to) = presence.to.as_ref() else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    let Some(nick) = to.resource().map(|resource| resource.as_str()) else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    if to.to_bare() != *room_jid {
        return Err(OrderedRelayNackReason::ParseFailure);
    }

    let frames = crate::server::routes::websocket::handlers::presence::handle_muc_leave(
        state,
        room_jid,
        &sender_jid,
        nick,
        None,
    )
    .await;
    remote_replies_from_frames(frames)
}

async fn deliver_reserved_muc_groupchat(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    message: &xmpp_parsers::message::Message,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    let synthetic_session = message
        .from
        .as_ref()
        .and_then(|jid| jid.clone().try_into_full().ok())
        .map(|sender| synthetic_session_for_full_jid(&sender));
    let sender_entity = room_entity(room_jid);
    let deps = build_interpret_deps(state, synthetic_session.as_ref()).with_ordered_relay_origin(
        Some(OrderedRelayRouteOrigin {
            kind: OrderedRelayRouteOriginKind::Entity(sender_entity.clone()),
            sender_entity,
            inbound_sequence: 0,
            handoff: None,
        }),
    );
    let outcome = tokio::time::timeout(
        ORDERED_RECEIVER_DELIVERY_TIMEOUT,
        crate::server::routes::interpret::dispatch_muc_to_room_for_relay(
            &deps,
            room_jid.clone(),
            message.clone(),
        ),
    )
    .await
    .map_err(|_| OrderedRelayNackReason::MaybeCommitted)?;
    Ok(outcome
        .frames
        .into_iter()
        .filter_map(|frame| match super::codec::decode_stanza(frame.as_str()) {
            Ok(stanza) => Some(RemoteStanza(stanza)),
            Err(error) => {
                tracing::warn!(
                    room = %room_jid,
                    %error,
                    "ordered relay: MUC groupchat reply frame was not a stanza"
                );
                None
            }
        })
        .collect())
}

fn remote_replies_from_frames(
    frames: Vec<String>,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    frames
        .into_iter()
        .map(|frame| super::codec::decode_stanza(frame.as_str()).map(RemoteStanza))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            tracing::warn!(
                %error,
                "ordered relay: MUC proxy reply frame was not a stanza"
            );
            OrderedRelayNackReason::ParseFailure
        })
}

fn synthetic_session_for_full_jid(sender_jid: &jid::FullJid) -> crate::auth::Session {
    let sender_bare = sender_jid.to_bare();
    let localpart = sender_bare
        .node()
        .map(|node| node.to_string())
        .unwrap_or_else(|| sender_bare.to_string());
    let sender_bare_string = sender_bare.to_string();
    crate::auth::Session::new(
        sender_bare_string.as_str(),
        localpart.as_str(),
        localpart.as_str(),
    )
}

async fn deliver_local_after_target_refresh_outcome(
    services: &OrderedRelayDeliveryServices,
    target: &jid::Jid,
    stanza: &Stanza,
    payload: &OrderedRelayPayload,
) -> RemoteDeliveryOutcome {
    match payload {
        OrderedRelayPayload::MucProxy {
            room_jid,
            kind,
            stanza,
        } => muc_proxy_result_to_outcome(
            Box::pin(deliver_reserved_muc_proxy(
                services, room_jid, *kind, &stanza.0,
            ))
            .await,
        ),
        OrderedRelayPayload::Message { .. }
        | OrderedRelayPayload::Iq { .. }
        | OrderedRelayPayload::Presence { .. } => no_client_reply_outcome(
            deliver_local_after_target_refresh(services, target, stanza).await,
        ),
    }
}

fn muc_proxy_result_to_outcome(
    result: Result<Vec<RemoteStanza>, OrderedRelayNackReason>,
) -> RemoteDeliveryOutcome {
    match result {
        Ok(replies) => RemoteDeliveryOutcome {
            delivery: FullJidDeliveryOutcome::Delivered,
            client_replies: replies.into_iter().map(|reply| reply.0).collect(),
            maybe_committed: false,
            join_repair_allowed: false,
        },
        Err(OrderedRelayNackReason::MaybeCommitted) => {
            no_client_reply_outcome_with_commit_state(FullJidDeliveryOutcome::Dropped, true)
        }
        Err(OrderedRelayNackReason::TargetUnavailable) => {
            no_client_reply_outcome(FullJidDeliveryOutcome::Unavailable)
        }
        Err(_) => no_client_reply_outcome(FullJidDeliveryOutcome::Dropped),
    }
}

fn muc_proxy_result_to_ordered_outcome(
    kind: OrderedRelayMucProxyKind,
    result: Result<Vec<RemoteStanza>, OrderedRelayNackReason>,
) -> OrderedRelayMucProxyOutcome {
    match result {
        Ok(replies) => OrderedRelayMucProxyOutcome::Delivered(
            replies.into_iter().map(|reply| reply.0).collect(),
        ),
        Err(OrderedRelayNackReason::TargetUnavailable) => OrderedRelayMucProxyOutcome::Unavailable,
        Err(OrderedRelayNackReason::MaybeCommitted)
            if kind == OrderedRelayMucProxyKind::JoinPresence =>
        {
            OrderedRelayMucProxyOutcome::JoinMaybeCommitted
        }
        Err(OrderedRelayNackReason::MaybeCommitted) => OrderedRelayMucProxyOutcome::MaybeCommitted,
        Err(_) => OrderedRelayMucProxyOutcome::Dropped,
    }
}

async fn deliver_local_after_target_refresh(
    services: &OrderedRelayDeliveryServices,
    target: &jid::Jid,
    stanza: &Stanza,
) -> FullJidDeliveryOutcome {
    match target.clone().try_into_full() {
        Ok(full) => deliver_local_full_jid_after_target_refresh(services, &full, stanza).await,
        Err(bare) => match route_local_bare_jid_with_timeout(services, &bare, stanza, None).await {
            Ok(replies) if !replies.is_empty() => FullJidDeliveryOutcome::Unavailable,
            Ok(_) => FullJidDeliveryOutcome::Delivered,
            Err(error) => {
                tracing::warn!(
                    bare_jid = %bare,
                    ?error,
                    "ordered relay: target-owner refresh resolved to local bare-JID \
                     owner but local delivery did not complete"
                );
                FullJidDeliveryOutcome::Dropped
            }
        },
    }
}

async fn deliver_local_full_jid_after_target_refresh(
    services: &OrderedRelayDeliveryServices,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> FullJidDeliveryOutcome {
    if matches!(stanza, Stanza::Iq(_)) {
        return match deliver_reserved_full_jid_peer_live_only(services, target, stanza).await {
            Ok(()) => FullJidDeliveryOutcome::Delivered,
            Err(OrderedRelayNackReason::TargetUnavailable) => FullJidDeliveryOutcome::Unavailable,
            Err(_) => FullJidDeliveryOutcome::Dropped,
        };
    }
    crate::server::routes::interpret::deliver_peer_to_full(
        Some(&services.user_registry),
        Some(&services.sm_session_registry),
        target,
        stanza,
    )
    .await
}

async fn validate_claims(
    services: &OrderedRelayDeliveryServices,
    envelope: &RemoteStanzaEnvelope,
) -> Result<(), OrderedRelayNackReason> {
    let origin = services
        .claim_store
        .current_claim(&envelope.origin_claim.entity)
        .await
        .map_err(|error| {
            tracing::warn!(
                entity = %envelope.origin_claim.entity,
                %error,
                "ordered relay: origin claim lookup failed"
            );
            OrderedRelayNackReason::Unreachable
        })?
        .ok_or(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        })?;
    if !origin.owner_lease_fresh
        || origin.claim_epoch != envelope.origin_claim.epoch
        || origin.owner.node_id != envelope.asserted_origin_node.as_str()
    {
        return Err(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        });
    }
    validate_origin_proof(services, envelope, &origin.owner).await?;

    let sender = services
        .claim_store
        .current_claim(&envelope.sender_claim.entity)
        .await
        .map_err(|error| {
            tracing::warn!(
                entity = %envelope.sender_claim.entity,
                %error,
                "ordered relay: sender claim lookup failed"
            );
            OrderedRelayNackReason::Unreachable
        })?
        .ok_or(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Sender,
        })?;
    if !sender.owner_lease_fresh
        || sender.claim_epoch != envelope.sender_claim.epoch
        || sender.owner != origin.owner
    {
        return Err(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Sender,
        });
    }

    let target = services
        .claim_store
        .current_claim(&envelope.target_claim.entity)
        .await
        .map_err(|error| {
            tracing::warn!(
                entity = %envelope.target_claim.entity,
                %error,
                "ordered relay: target claim lookup failed"
            );
            OrderedRelayNackReason::Unreachable
        })?
        .ok_or(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Target,
        })?;
    let me = services.node_identity.current();
    if !target.owner_lease_fresh
        || target.claim_epoch != envelope.target_claim.epoch
        || target.owner != me
    {
        return Err(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Target,
        });
    }
    Ok(())
}

async fn validate_origin_proof(
    services: &OrderedRelayDeliveryServices,
    envelope: &RemoteStanzaEnvelope,
    origin_owner: &waddle_xmpp::ownership::NodeIdentity,
) -> Result<(), OrderedRelayNackReason> {
    let Some(proof) = &envelope.origin_proof else {
        tracing::warn!(
            asserted_origin_node = %envelope.asserted_origin_node,
            "ordered relay: unsigned origin envelope rejected"
        );
        return Err(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        });
    };
    let public_key = PublicKey::try_decode_protobuf(&proof.public_key).map_err(|error| {
        tracing::warn!(
            %error,
            asserted_origin_node = %envelope.asserted_origin_node,
            "ordered relay: origin proof public key did not decode"
        );
        OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        }
    })?;
    let signing_bytes = envelope.signing_bytes().map_err(|error| {
        tracing::warn!(
            %error,
            asserted_origin_node = %envelope.asserted_origin_node,
            "ordered relay: failed to serialize origin verification bytes"
        );
        OrderedRelayNackReason::ParseFailure
    })?;
    if !public_key.verify(&signing_bytes, &proof.signature) {
        tracing::warn!(
            asserted_origin_node = %envelope.asserted_origin_node,
            "ordered relay: origin proof signature verification failed"
        );
        return Err(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        });
    }
    let signed_peer = public_key.to_peer_id();
    let signed_peer_id = signed_peer.to_string();
    let registered_peer_id = services
        .node_lease
        .peer_id_for_node(origin_owner)
        .await
        .map_err(|error| {
            tracing::warn!(
                %error,
                node_id = %origin_owner.node_id,
                "ordered relay: failed to load origin node PeerId binding"
            );
            OrderedRelayNackReason::Unreachable
        })?
        .ok_or_else(|| {
            tracing::warn!(
                node_id = %origin_owner.node_id,
                "ordered relay: origin node has no PeerId binding"
            );
            OrderedRelayNackReason::NotOwner {
                role: OrderedRelayClaimRole::Origin,
            }
        })?;
    if registered_peer_id != signed_peer_id {
        tracing::warn!(
            node_id = %origin_owner.node_id,
            registered_peer_id = %registered_peer_id,
            signed_peer_id = %signed_peer_id,
            "ordered relay: origin proof PeerId does not match node lease binding"
        );
        return Err(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        });
    }
    let enrolled = services
        .allowlist_store
        .enrolled_peers()
        .await
        .map_err(|error| {
            tracing::warn!(
                %error,
                node_id = %origin_owner.node_id,
                signed_peer_id = %signed_peer_id,
                "ordered relay: failed to revalidate origin PeerId allowlist enrollment"
            );
            OrderedRelayNackReason::Unreachable
        })?;
    if !enrolled.contains(&signed_peer) {
        tracing::warn!(
            node_id = %origin_owner.node_id,
            signed_peer_id = %signed_peer_id,
            "ordered relay: origin PeerId is not enrolled in current allowlist"
        );
        return Err(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        });
    }
    Ok(())
}

async fn outcome_for_nack(
    services: &OrderedRelayDeliveryServices,
    target_entity: &Entity,
    previous_owner: &waddle_xmpp::ownership::NodeIdentity,
    nack: &OrderedRelayNack,
    is_iq: bool,
) -> (Option<FullJidDeliveryOutcome>, NackChannelAction, bool) {
    match nack.reason {
        OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        }
        | OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Sender,
        } => (
            Some(definite_no_effect_outcome(is_iq)),
            NackChannelAction::Divert(OrderedRelayDiversionReason::NotOwner),
            false,
        ),
        OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Target,
        } => {
            let Some(snapshot) = current_claim(services, target_entity).await else {
                return (
                    Some(FullJidDeliveryOutcome::Unavailable),
                    NackChannelAction::Divert(OrderedRelayDiversionReason::Unreachable),
                    false,
                );
            };
            if !snapshot.owner_lease_fresh {
                return (
                    Some(FullJidDeliveryOutcome::Unavailable),
                    NackChannelAction::Divert(OrderedRelayDiversionReason::Unreachable),
                    false,
                );
            }
            let me = services.node_identity.current();
            if snapshot.owner == me {
                return (None, NackChannelAction::Forget, false);
            }
            if snapshot.owner != *previous_owner {
                tracing::debug!(
                    entity_id = %target_entity.id,
                    previous_owner = %previous_owner.node_id,
                    refreshed_owner = %snapshot.owner.node_id,
                    "ordered relay: target-owner changed after retry window; suppressing client fallback"
                );
                return (
                    Some(definite_no_effect_outcome(is_iq)),
                    NackChannelAction::Forget,
                    false,
                );
            }
            (
                Some(definite_no_effect_outcome(is_iq)),
                NackChannelAction::Divert(OrderedRelayDiversionReason::NotOwner),
                false,
            )
        }
        OrderedRelayNackReason::TargetUnavailable => (
            Some(FullJidDeliveryOutcome::Unavailable),
            NackChannelAction::Divert(diversion_reason_for_nack(nack)),
            false,
        ),
        OrderedRelayNackReason::InFlight => (
            Some(FullJidDeliveryOutcome::Dropped),
            NackChannelAction::Keep,
            true,
        ),
        OrderedRelayNackReason::MaybeCommitted => (
            Some(FullJidDeliveryOutcome::Dropped),
            NackChannelAction::Divert(OrderedRelayDiversionReason::MaybeCommitted),
            true,
        ),
        OrderedRelayNackReason::Diverted(ref diversion)
            if diversion.reason == OrderedRelayDiversionReason::MaybeCommitted =>
        {
            (
                Some(FullJidDeliveryOutcome::Dropped),
                NackChannelAction::Divert(OrderedRelayDiversionReason::MaybeCommitted),
                true,
            )
        }
        OrderedRelayNackReason::Diverted(_) => (
            Some(definite_no_effect_outcome(is_iq)),
            NackChannelAction::Divert(diversion_reason_for_nack(nack)),
            false,
        ),
        OrderedRelayNackReason::Unreachable => (
            Some(definite_no_effect_outcome(is_iq)),
            NackChannelAction::Divert(diversion_reason_for_nack(nack)),
            false,
        ),
        OrderedRelayNackReason::Gap { .. }
        | OrderedRelayNackReason::ParseFailure
        | OrderedRelayNackReason::Backpressure => (
            Some(definite_no_effect_outcome(is_iq)),
            NackChannelAction::Divert(diversion_reason_for_nack(nack)),
            false,
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NackChannelAction {
    Divert(OrderedRelayDiversionReason),
    Forget,
    Keep,
}

fn definite_no_effect_outcome(is_iq: bool) -> FullJidDeliveryOutcome {
    if is_iq {
        FullJidDeliveryOutcome::Unavailable
    } else {
        FullJidDeliveryOutcome::Dropped
    }
}

fn replies_for_origin_handoff(stanza: &Stanza, outcome: FullJidDeliveryOutcome) -> Vec<Stanza> {
    match outcome {
        FullJidDeliveryOutcome::Unavailable => {
            crate::server::routes::interpret::fallback_reply_for_undeliverable_iq(stanza)
                .into_iter()
                .collect()
        }
        FullJidDeliveryOutcome::Delivered
        | FullJidDeliveryOutcome::QueuedDetached
        | FullJidDeliveryOutcome::Dropped
        | FullJidDeliveryOutcome::MaybeCommitted => Vec::new(),
    }
}

fn diversion_reason_for_nack(nack: &OrderedRelayNack) -> OrderedRelayDiversionReason {
    match &nack.reason {
        OrderedRelayNackReason::Gap { .. }
        | OrderedRelayNackReason::ParseFailure
        | OrderedRelayNackReason::Diverted(_) => OrderedRelayDiversionReason::OrderingGap,
        OrderedRelayNackReason::NotOwner { .. } => OrderedRelayDiversionReason::NotOwner,
        OrderedRelayNackReason::Unreachable | OrderedRelayNackReason::TargetUnavailable => {
            OrderedRelayDiversionReason::Unreachable
        }
        OrderedRelayNackReason::InFlight | OrderedRelayNackReason::Backpressure => {
            OrderedRelayDiversionReason::Backpressure
        }
        OrderedRelayNackReason::MaybeCommitted => OrderedRelayDiversionReason::MaybeCommitted,
    }
}

fn channel_diversion_for_ask_error(error: &RelayAskError) -> Option<OrderedRelayDiversionReason> {
    match error {
        RelayAskError::NotFound { .. } => None,
        RelayAskError::Send {
            failure: RelaySendFailure::MailboxFull,
            ..
        } => Some(OrderedRelayDiversionReason::Backpressure),
        RelayAskError::Send { .. } | RelayAskError::Cancelled => {
            Some(OrderedRelayDiversionReason::Unreachable)
        }
    }
}

fn ask_error_allows_target_refresh(error: &RelayAskError) -> bool {
    match error {
        RelayAskError::NotFound { .. } => true,
        RelayAskError::Send {
            effect: RelaySendEffect::NoEffect,
            ..
        } => true,
        RelayAskError::Cancelled
        | RelayAskError::Send {
            effect: RelaySendEffect::MaybeCommitted,
            ..
        } => false,
    }
}

fn ask_error_proves_remote_resource_ref_stale(error: &RelayAskError) -> bool {
    matches!(
        error,
        RelayAskError::NotFound { .. }
            | RelayAskError::Send {
                failure: RelaySendFailure::StaleRef,
                effect: RelaySendEffect::NoEffect,
                ..
            }
    )
}

fn outcome_for_ask_error(error: &RelayAskError, is_iq: bool) -> Option<FullJidDeliveryOutcome> {
    tracing::warn!(
        ?error,
        "ordered relay: remote ask failed; classifying for client fallback"
    );
    match error {
        RelayAskError::NotFound { .. } => None,
        RelayAskError::Cancelled => Some(FullJidDeliveryOutcome::Dropped),
        RelayAskError::Send { effect, .. } => Some(match effect {
            RelaySendEffect::NoEffect => definite_no_effect_outcome(is_iq),
            RelaySendEffect::MaybeCommitted => FullJidDeliveryOutcome::MaybeCommitted,
        }),
    }
}

fn ask_error_maybe_committed(error: &RelayAskError) -> bool {
    matches!(
        error,
        RelayAskError::Send {
            effect: RelaySendEffect::MaybeCommitted,
            ..
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clustering::ordered_relay::OrderedRelaySequence;
    use kameo::actor::Spawn;
    use libp2p::PeerId;
    use std::collections::HashSet;
    use waddle_xmpp::ownership::{ClaimEpoch, ClaimError, InProcessClaimStore, NodeIdentity};
    use xmpp_parsers::message::{Lang, Message};

    struct StaticNodeLease {
        origin: NodeIdentity,
        peer_id: String,
    }

    #[async_trait::async_trait]
    impl NodeLeaseStore for StaticNodeLease {
        async fn list_orphaned_room_actor_claims_page(
            &self,
            _after: Option<crate::clustering::claims::RoomOrphanScanCursor>,
            _limit: usize,
        ) -> Result<crate::clustering::claims::OrphanedRoomActorClaimPage, ClaimError> {
            Ok(crate::clustering::claims::OrphanedRoomActorClaimPage {
                candidates: Vec::new(),
                next_cursor: None,
                has_more: false,
                quarantined: 0,
            })
        }

        async fn register(
            &self,
            _me: &NodeIdentity,
            _pod_template_hash: Option<String>,
        ) -> Result<(), ClaimError> {
            Ok(())
        }

        async fn peer_id_for_node(
            &self,
            node: &NodeIdentity,
        ) -> Result<Option<String>, ClaimError> {
            if node == &self.origin {
                Ok(Some(self.peer_id.clone()))
            } else {
                Ok(None)
            }
        }

        async fn heartbeat(
            &self,
            _me: &NodeIdentity,
            _lease_ttl: Duration,
        ) -> Result<bool, ClaimError> {
            Ok(true)
        }

        async fn expire(
            &self,
            _owner: &NodeIdentity,
            _lease_ttl: Duration,
        ) -> Result<bool, ClaimError> {
            Ok(true)
        }

        async fn mark_draining(&self, _me: &NodeIdentity) -> Result<(), ClaimError> {
            Ok(())
        }

        async fn count_other_live_nodes(
            &self,
            _me: &NodeIdentity,
            _lease_ttl: Duration,
        ) -> Result<usize, ClaimError> {
            Ok(0)
        }

        async fn reconcile(
            &self,
            _me: &NodeIdentity,
            _locally_owned: &[Entity],
        ) -> Result<Vec<Entity>, ClaimError> {
            Ok(Vec::new())
        }

        async fn report_steal_intent(
            &self,
            _entity: &Entity,
            _reporter: &NodeIdentity,
        ) -> Result<(), ClaimError> {
            Ok(())
        }

        async fn owner_steal_intents(
            &self,
            _me: &NodeIdentity,
        ) -> Result<Vec<(Entity, ClaimEpoch)>, ClaimError> {
            Ok(Vec::new())
        }

        async fn clear_steal_intent(
            &self,
            _entity: &Entity,
            _me: &NodeIdentity,
            _mine: ClaimEpoch,
        ) -> Result<u64, ClaimError> {
            Ok(0)
        }

        async fn list_orphaned_sm_session_claims(
            &self,
        ) -> Result<Vec<crate::clustering::claims::OrphanedSmSessionClaim>, ClaimError> {
            Ok(Vec::new())
        }

        async fn current_generation(&self) -> Result<Option<String>, ClaimError> {
            Ok(None)
        }
    }

    struct StaticAllowlist {
        peer_id: PeerId,
    }

    #[async_trait::async_trait]
    impl AllowlistStore for StaticAllowlist {
        async fn ensure_schema(&self) -> Result<(), super::super::allowlist::AllowlistError> {
            Ok(())
        }

        async fn enrolled_peers(
            &self,
        ) -> Result<HashSet<PeerId>, super::super::allowlist::AllowlistError> {
            Ok(HashSet::from([self.peer_id]))
        }
    }

    fn origin_identity() -> NodeIdentity {
        NodeIdentity::new("origin-node", "origin-epoch")
    }

    fn receiver_identity() -> NodeIdentity {
        NodeIdentity::new("receiver-node", "receiver-epoch")
    }

    fn other_identity() -> NodeIdentity {
        NodeIdentity::new("other-node", "other-epoch")
    }

    fn origin_entity() -> Entity {
        Entity::new(EntityType::SmSession, "stream-1")
    }

    fn sender_full() -> jid::FullJid {
        "romeo@example.test/home".parse().expect("full jid")
    }

    fn sender_entity() -> Entity {
        user_entity(&sender_full().to_bare())
    }

    fn target_bare() -> jid::BareJid {
        "juliet@example.test".parse().expect("bare jid")
    }

    fn target_full() -> jid::FullJid {
        "juliet@example.test/phone".parse().expect("full jid")
    }

    fn target_entity() -> Entity {
        user_entity(&target_bare())
    }

    fn envelope_claims(target_epoch: i64) -> OrderedRelayEnvelopeClaims {
        OrderedRelayEnvelopeClaims::new(
            OrderedRelayClaim {
                entity: origin_entity(),
                epoch: ClaimEpoch(0),
            },
            OrderedRelayClaim {
                entity: sender_entity(),
                epoch: ClaimEpoch(0),
            },
            OrderedRelayClaim {
                entity: target_entity(),
                epoch: ClaimEpoch(target_epoch),
            },
        )
    }

    fn message_payload() -> OrderedRelayPayload {
        let full = target_full();
        let mut message = Message::new(Some(jid::Jid::from(full.clone())));
        message.from = Some(jid::Jid::from(sender_full()));
        message.type_ = xmpp_parsers::message::MessageType::Chat;
        message
            .bodies
            .insert(Lang::new(), "hello from remote".to_string());
        OrderedRelayPayload::Message {
            recipient: jid::Jid::from(full),
            stanza: RemoteStanza(Stanza::Message(message)),
        }
    }

    fn iq_payload() -> OrderedRelayPayload {
        let full = target_full();
        let mut iq = xmpp_parsers::iq::Iq::from_get("iq-1", xmpp_parsers::ping::Ping);
        *iq.from_mut() = Some(jid::Jid::from(sender_full()));
        *iq.to_mut() = Some(jid::Jid::from(full.clone()));
        OrderedRelayPayload::Iq {
            recipient: jid::Jid::from(full),
            stanza: RemoteStanza(Stanza::Iq(Box::new(iq))),
        }
    }

    #[test]
    fn full_jid_bridge_rejects_groupchat_payloads() {
        let target = target_full();
        let mut message = Message::new(Some(jid::Jid::from(target.clone())));
        message.type_ = xmpp_parsers::message::MessageType::Groupchat;

        assert!(payload_for_recipient(jid::Jid::from(target), &Stanza::Message(message)).is_none());
    }

    fn envelope() -> RemoteStanzaEnvelope {
        RemoteStanzaEnvelope {
            asserted_origin_node: NodeId::new(origin_identity().node_id),
            channel: OrderedRelayChannel {
                origin: OrderedRelayOrigin::SmSession(
                    waddle_xmpp::pending_delivery::SmSessionId::new("stream-1"),
                ),
                recipient: OrderedRelayRecipient::FullJid(target_full()),
                target_epoch: waddle_xmpp::ownership::ClaimEpoch(0),
            },
            sequence: OrderedRelaySequence::FIRST,
            origin_inbound_sequence: OriginInboundSequence(1),
            origin_claim: OrderedRelayClaim {
                entity: origin_entity(),
                epoch: waddle_xmpp::ownership::ClaimEpoch(0),
            },
            sender_claim: OrderedRelayClaim {
                entity: sender_entity(),
                epoch: waddle_xmpp::ownership::ClaimEpoch(0),
            },
            target_claim: OrderedRelayClaim {
                entity: target_entity(),
                epoch: waddle_xmpp::ownership::ClaimEpoch(0),
            },
            payload: message_payload(),
            origin_proof: None,
        }
    }

    fn sign_envelope(
        mut envelope: RemoteStanzaEnvelope,
        keypair: &Keypair,
    ) -> RemoteStanzaEnvelope {
        let signing_bytes = envelope.signing_bytes().expect("signing bytes");
        envelope.origin_proof = Some(OrderedRelayOriginProof {
            public_key: keypair.public().encode_protobuf(),
            signature: keypair.sign(&signing_bytes).expect("sign envelope"),
        });
        envelope
    }

    async fn envelope_for_services(
        services: &OrderedRelayDeliveryServices,
    ) -> RemoteStanzaEnvelope {
        async fn epoch_for(services: &OrderedRelayDeliveryServices, entity: &Entity) -> ClaimEpoch {
            services
                .claim_store
                .current_claim(entity)
                .await
                .expect("claim lookup")
                .expect("seeded claim")
                .claim_epoch
        }

        let mut envelope = envelope();
        envelope.origin_claim.epoch = epoch_for(services, &origin_entity()).await;
        envelope.sender_claim.epoch = epoch_for(services, &sender_entity()).await;
        let target_epoch = epoch_for(services, &target_entity()).await;
        envelope.target_claim.epoch = target_epoch;
        envelope.channel.target_epoch = target_epoch;
        envelope
    }

    async fn signed_envelope_for_services(
        services: &OrderedRelayDeliveryServices,
        keypair: &Keypair,
    ) -> RemoteStanzaEnvelope {
        sign_envelope(envelope_for_services(services).await, keypair)
    }

    fn test_peer_id() -> String {
        Keypair::generate_ed25519()
            .public()
            .to_peer_id()
            .to_string()
    }

    async fn services_with_claims(
        origin_owner: NodeIdentity,
        target_owner: NodeIdentity,
        receiver: NodeIdentity,
        origin_peer_id: String,
    ) -> OrderedRelayDeliveryServices {
        services_with_claims_and_blocking(
            origin_owner,
            target_owner,
            receiver,
            origin_peer_id,
            Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new()),
        )
        .await
    }

    async fn services_with_claims_and_blocking(
        origin_owner: NodeIdentity,
        target_owner: NodeIdentity,
        receiver: NodeIdentity,
        origin_peer_id: String,
        blocking_storage: Arc<dyn BlockingStorage>,
    ) -> OrderedRelayDeliveryServices {
        let store = Arc::new(InProcessClaimStore::new());
        store
            .acquire(&origin_entity(), &origin_owner)
            .await
            .expect("origin claim");
        store
            .acquire(&sender_entity(), &origin_owner)
            .await
            .expect("sender claim");
        store
            .acquire(&target_entity(), &target_owner)
            .await
            .expect("target claim");
        OrderedRelayDeliveryServices {
            claim_store: store,
            allowlist_store: Arc::new(StaticAllowlist {
                peer_id: origin_peer_id.parse().expect("valid test peer id"),
            }),
            node_lease: Arc::new(StaticNodeLease {
                origin: origin_owner,
                peer_id: origin_peer_id,
            }),
            node_identity: SharedNodeIdentity::new(receiver),
            connection_registry: Arc::new(ConnectionRegistry::new()),
            user_registry: UserRegistryActor::spawn(UserRegistryActor::new()),
            sm_session_registry: Arc::new(InMemorySmSessionRegistry::new()),
            blocking_storage,
            web_socket_state: Weak::new(),
        }
    }

    #[tokio::test]
    async fn unwired_bridge_reports_unreachable_without_advancing_effects() {
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );

        let err = bridge
            .deliver_reserved(&envelope())
            .await
            .expect_err("unwired bridge cannot deliver");

        assert_eq!(err, OrderedRelayNackReason::Unreachable);
    }

    #[tokio::test]
    async fn receiver_rejects_target_claim_not_owned_by_this_node() {
        let keypair = Keypair::generate_ed25519();
        let services = services_with_claims(
            origin_identity(),
            other_identity(),
            receiver_identity(),
            keypair.public().to_peer_id().to_string(),
        )
        .await;

        let envelope = signed_envelope_for_services(&services, &keypair).await;
        let err = validate_claims(&services, &envelope)
            .await
            .expect_err("foreign target owner is not this relay");

        assert_eq!(
            err,
            OrderedRelayNackReason::NotOwner {
                role: OrderedRelayClaimRole::Target
            }
        );
    }

    #[tokio::test]
    async fn receiver_accepts_fresh_origin_and_local_target_claims() {
        let keypair = Keypair::generate_ed25519();
        let services = services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            keypair.public().to_peer_id().to_string(),
        )
        .await;

        let envelope = signed_envelope_for_services(&services, &keypair).await;
        validate_claims(&services, &envelope)
            .await
            .expect("claims match origin and receiver");
    }

    #[tokio::test]
    async fn receiver_rejects_stale_sender_claim_before_delivery_effects() {
        let keypair = Keypair::generate_ed25519();
        let services = services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            keypair.public().to_peer_id().to_string(),
        )
        .await;
        let mut envelope = envelope_for_services(&services).await;
        envelope.sender_claim.epoch = ClaimEpoch(99);
        let envelope = sign_envelope(envelope, &keypair);

        let err = validate_claims(&services, &envelope)
            .await
            .expect_err("stale sender claim must be rejected");

        assert_eq!(
            err,
            OrderedRelayNackReason::NotOwner {
                role: OrderedRelayClaimRole::Sender
            }
        );
    }

    #[tokio::test]
    async fn bare_presence_direct_drops_blocked_sender_before_detached_replay() {
        use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};

        let blocking = Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new());
        blocking.set_blocklist(target_bare(), vec![sender_full().to_bare()]);
        let services = services_with_claims_and_blocking(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
            blocking,
        )
        .await;

        let detached_stream = "stream-detached-presence";
        services
            .sm_session_registry
            .store_session(DetachedSession {
                stream_id: detached_stream.to_string(),
                user_id: target_bare().to_string(),
                jid: target_full(),
                inbound_count: 0,
                outbound_count: 0,
                last_acked: 0,
                replay_gap_through: None,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(300),
                detached_at: std::time::Instant::now(),
                carbons_enabled: false,
                roster_interested: false,
                blocklist_interested: false,
                presence_available: true,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
                presence_payloads: Vec::new(),
                pending_subscribes_flushed: false,
            })
            .await
            .expect("store detached target resource");

        let mut presence =
            xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
        presence.from = Some(jid::Jid::from(sender_full()));
        presence.to = Some(jid::Jid::from(target_bare()));

        deliver_reserved_bare_presence_direct(
            &services,
            &target_bare(),
            &Stanza::Presence(presence),
        )
        .await
        .expect("blocked presence is silently dropped");

        let detached = services
            .sm_session_registry
            .peek_session(detached_stream)
            .await
            .expect("peek detached target resource")
            .expect("detached target resource remains");
        assert!(
            detached.unacked_stanzas.is_empty(),
            "blocked remote presence must not be queued for SM replay"
        );
    }

    #[tokio::test]
    async fn receiver_nacks_iq_without_live_resource_instead_of_detached_queue() {
        let keypair = Keypair::generate_ed25519();
        let services = services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            keypair.public().to_peer_id().to_string(),
        )
        .await;
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let mut envelope = envelope_for_services(&services).await;
        envelope.payload = iq_payload();
        let envelope = sign_envelope(envelope, &keypair);
        bridge.wire(Arc::new(services));

        let err = bridge
            .deliver_reserved(&envelope)
            .await
            .expect_err("remote full-JID IQ requires a live local resource");

        assert_eq!(err, OrderedRelayNackReason::TargetUnavailable);
    }

    #[tokio::test]
    async fn receiver_delivers_full_jid_iq_as_peer_stanza() {
        let keypair = Keypair::generate_ed25519();
        let services = services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            keypair.public().to_peer_id().to_string(),
        )
        .await;
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        services
            .user_registry
            .ask(waddle_xmpp::registry::RegisterUserResource {
                jid: target_full(),
                entry: waddle_xmpp::registry::ConnectionEntry::new(tx),
            })
            .await
            .expect("register resource");
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let mut envelope = envelope_for_services(&services).await;
        envelope.payload = iq_payload();
        let envelope = sign_envelope(envelope, &keypair);
        bridge.wire(Arc::new(services));

        bridge
            .deliver_reserved(&envelope)
            .await
            .expect("remote full-JID IQ delivers to live resource");

        let outbound = rx.try_recv().expect("queued outbound stanza");
        assert_eq!(
            outbound.kind,
            waddle_xmpp::registry::DeliveryKind::PeerStanza
        );
    }

    #[tokio::test]
    async fn stale_registered_remote_resource_cleans_mirror_and_allows_local_fallback() {
        let services = Arc::new(
            services_with_claims(
                origin_identity(),
                receiver_identity(),
                receiver_identity(),
                test_peer_id(),
            )
            .await,
        );
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));

        let target = target_full();
        let (tx, _rx) = mpsc::channel(1);
        let entry = ConnectionEntry::new(tx);
        let owner = entry.carbons_handle();
        services
            .connection_registry
            .register_entry(target.clone(), entry.clone());
        services
            .user_registry
            .ask(waddle_xmpp::registry::RegisterUserResource {
                jid: target.clone(),
                entry,
            })
            .await
            .expect("register remote mirror in user actor");

        let registration_id = RemoteResourceRegistrationId::fresh();
        bridge.remote_owner_resources.lock().await.insert(
            target.clone(),
            RemoteOwnerRegistration {
                registration_id,
                socket_node: NodeId::new("missing-socket-node".to_string()),
                socket_generation: RemoteResourceSocketGeneration::next(None),
                owner: Arc::clone(&owner),
            },
        );

        let outcome = bridge
            .try_deliver_registered_remote_resource(
                &target,
                &Stanza::Message(Message::new(Some(jid::Jid::from(target.clone())))),
                DeliveryKind::PeerStanza,
            )
            .await;

        assert_eq!(outcome, None);
        assert!(
            !bridge
                .remote_owner_resources
                .lock()
                .await
                .contains_key(&target),
            "stale remote owner map entry must be removed"
        );
        assert!(
            services
                .connection_registry
                .entry_if_owner(&target, &owner)
                .is_none(),
            "stale remote connection mirror must be removed so local fallback can run"
        );
    }

    #[tokio::test]
    async fn stale_force_detach_error_cleans_old_socket_mirror() {
        let services = Arc::new(
            services_with_claims(
                origin_identity(),
                receiver_identity(),
                receiver_identity(),
                test_peer_id(),
            )
            .await,
        );
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));

        let target = target_full();
        let (old_tx, _old_rx) = mpsc::channel(1);
        let old_entry = ConnectionEntry::new(old_tx);
        let old_owner = old_entry.carbons_handle();
        services
            .connection_registry
            .register_entry(target.clone(), old_entry.clone());
        services
            .user_registry
            .ask(waddle_xmpp::registry::RegisterUserResource {
                jid: target.clone(),
                entry: old_entry,
            })
            .await
            .expect("register old remote mirror in user actor");

        let old_generation = RemoteResourceSocketGeneration::next(None);
        let registration = RemoteOwnerRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            socket_node: NodeId::new("missing-old-socket-node".to_string()),
            socket_generation: old_generation,
            owner: Arc::clone(&old_owner),
        };

        assert!(
            bridge
                .finish_remote_owner_registration_retire(
                    &services,
                    &target,
                    &registration,
                    Err(RelayAskError::NotFound {
                        node_id: registration.socket_node.clone(),
                    }),
                )
                .await,
            "a missing old socket node proves no detach was committed and should permit cleanup"
        );
        assert!(
            services
                .connection_registry
                .entry_if_owner(&target, &old_owner)
                .is_none(),
            "stale old owner mirror must be removed before replacement"
        );
        if let Some(actor) = services
            .user_registry
            .ask(waddle_xmpp::registry::GetUser {
                bare_jid: target.to_bare(),
            })
            .await
            .expect("get user actor")
        {
            assert!(
                actor
                    .ask(waddle_xmpp::registry::GetConnectionEntry {
                        jid: target.clone(),
                    })
                    .await
                    .expect("get old actor mirror")
                    .is_none(),
                "stale old user-actor mirror must be removed before replacement"
            );
        }
    }

    #[tokio::test]
    async fn receiver_nacks_dropped_peer_delivery_instead_of_acknowledging_loss() {
        let keypair = Keypair::generate_ed25519();
        let services = services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            keypair.public().to_peer_id().to_string(),
        )
        .await;
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        tx.try_send(waddle_xmpp::registry::OutboundStanza::peer_stanza(
            Stanza::Message(Message::new(Some(jid::Jid::from(target_full())))),
        ))
        .expect("seed outbound channel to capacity");
        services
            .user_registry
            .ask(waddle_xmpp::registry::RegisterUserResource {
                jid: target_full(),
                entry: waddle_xmpp::registry::ConnectionEntry::new(tx),
            })
            .await
            .expect("register resource");
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let envelope = sign_envelope(envelope_for_services(&services).await, &keypair);
        bridge.wire(Arc::new(services));

        let err = bridge
            .deliver_reserved(&envelope)
            .await
            .expect_err("full outbound channel must not be ACKed as delivered");

        assert_eq!(err, OrderedRelayNackReason::Backpressure);
    }

    #[tokio::test]
    async fn origin_not_owner_nack_is_terminal_provenance_failure() {
        let services = services_with_claims(
            origin_identity(),
            other_identity(),
            origin_identity(),
            test_peer_id(),
        )
        .await;
        let nack = OrderedRelayNack {
            channel: envelope().channel,
            sequence: OrderedRelaySequence::FIRST,
            reason: OrderedRelayNackReason::NotOwner {
                role: OrderedRelayClaimRole::Origin,
            },
        };

        let (iq_outcome, iq_action, iq_maybe_committed) = outcome_for_nack(
            &services,
            &target_entity(),
            &receiver_identity(),
            &nack,
            true,
        )
        .await;
        let (message_outcome, message_action, message_maybe_committed) = outcome_for_nack(
            &services,
            &target_entity(),
            &receiver_identity(),
            &nack,
            false,
        )
        .await;

        assert_eq!(iq_outcome, Some(FullJidDeliveryOutcome::Unavailable));
        assert_eq!(message_outcome, Some(FullJidDeliveryOutcome::Dropped));
        assert!(!iq_maybe_committed);
        assert!(!message_maybe_committed);
        assert_eq!(
            iq_action,
            NackChannelAction::Divert(OrderedRelayDiversionReason::NotOwner)
        );
        assert_eq!(
            message_action,
            NackChannelAction::Divert(OrderedRelayDiversionReason::NotOwner)
        );
    }

    #[tokio::test]
    async fn maybe_committed_diversion_suppresses_iq_fallback_on_replay() {
        let services = services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            test_peer_id(),
        )
        .await;
        let channel = envelope().channel;
        let nack = OrderedRelayNack {
            channel: channel.clone(),
            sequence: OrderedRelaySequence::FIRST,
            reason: OrderedRelayNackReason::Diverted(OrderedRelayDiversion {
                channel,
                reason: OrderedRelayDiversionReason::MaybeCommitted,
            }),
        };

        let (outcome, action, maybe_committed) = outcome_for_nack(
            &services,
            &target_entity(),
            &receiver_identity(),
            &nack,
            true,
        )
        .await;

        assert_eq!(outcome, Some(FullJidDeliveryOutcome::Dropped));
        assert!(maybe_committed);
        assert_eq!(
            action,
            NackChannelAction::Divert(OrderedRelayDiversionReason::MaybeCommitted)
        );
    }

    #[test]
    fn maybe_committed_remote_delivery_maps_to_fallback_suppressing_outcome() {
        let outcome =
            no_client_reply_outcome_with_commit_state(FullJidDeliveryOutcome::Dropped, true);

        assert_eq!(
            caller_delivery_outcome(outcome),
            FullJidDeliveryOutcome::MaybeCommitted
        );
    }

    #[tokio::test]
    async fn failed_ordered_delivery_sticky_diverts_channel_instead_of_rewinding_sequence() {
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let channel = envelope().channel;
        {
            let mut sender = bridge.sender_state.lock().await;
            let first = sender
                .next_envelope(
                    NodeId::new(origin_identity().node_id),
                    channel.clone(),
                    OriginInboundSequence(1),
                    envelope_claims(0),
                    message_payload(),
                )
                .expect("first envelope allocates");
            assert_eq!(first.sequence, OrderedRelaySequence::FIRST);
        }

        let nack = OrderedRelayNack {
            channel: channel.clone(),
            sequence: OrderedRelaySequence::FIRST,
            reason: OrderedRelayNackReason::TargetUnavailable,
        };
        bridge
            .divert_channel(channel.clone(), diversion_reason_for_nack(&nack))
            .await;

        let diverted = bridge
            .sender_state
            .lock()
            .await
            .next_envelope(
                NodeId::new(origin_identity().node_id),
                channel,
                OriginInboundSequence(2),
                envelope_claims(0),
                message_payload(),
            )
            .expect_err("later sends must not restart at sequence one");
        assert_eq!(diverted.reason, OrderedRelayDiversionReason::Unreachable);
    }

    #[tokio::test]
    async fn not_owner_nack_clears_sender_channel_for_refreshed_owner_retry() {
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let channel = envelope().channel;
        {
            let mut sender = bridge.sender_state.lock().await;
            let first = sender
                .next_envelope(
                    NodeId::new(origin_identity().node_id),
                    channel.clone(),
                    OriginInboundSequence(1),
                    envelope_claims(0),
                    message_payload(),
                )
                .expect("first envelope allocates");
            assert_eq!(first.sequence, OrderedRelaySequence::FIRST);
        }

        bridge
            .apply_nack_channel_action(channel.clone(), NackChannelAction::Forget)
            .await;

        let refreshed_channel = OrderedRelayChannel {
            target_epoch: ClaimEpoch(1),
            ..channel
        };
        let retried = bridge
            .sender_state
            .lock()
            .await
            .next_envelope(
                NodeId::new(origin_identity().node_id),
                refreshed_channel,
                OriginInboundSequence(2),
                envelope_claims(1),
                message_payload(),
            )
            .expect("not-owner no-effect path must allow refreshed-owner retry");
        assert_eq!(retried.sequence, OrderedRelaySequence::FIRST);
    }

    #[tokio::test]
    async fn relay_lookup_miss_rolls_back_unseen_sender_sequence() {
        let bridge = Arc::new(OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        ));
        let channel = envelope().channel;
        {
            let mut sender = bridge.sender_state.lock().await;
            let first = sender
                .next_envelope(
                    NodeId::new(origin_identity().node_id),
                    channel.clone(),
                    OriginInboundSequence(1),
                    envelope_claims(0),
                    message_payload(),
                )
                .expect("first envelope allocates");
            assert_eq!(first.sequence, OrderedRelaySequence::FIRST);
        }

        let outcome = OrderedRelayDeliveryBridge::finish_prepared_delivery_result(
            Arc::clone(&bridge),
            PreparedRemoteDelivery {
                services: Arc::new(
                    services_with_claims(
                        origin_identity(),
                        receiver_identity(),
                        receiver_identity(),
                        test_peer_id(),
                    )
                    .await,
                ),
                target_entity: target_entity(),
                previous_owner: receiver_identity(),
                channel: channel.clone(),
                envelope: envelope(),
                target: jid::Jid::from(target_full()),
                stanza: Stanza::Message(Message::new(Some(jid::Jid::from(target_full())))),
                is_iq: false,
            },
            Err(RelayAskError::NotFound {
                node_id: NodeId::new(receiver_identity().node_id),
            }),
        )
        .await;
        assert!(
            outcome.is_none(),
            "relay lookup miss must let the caller continue normal fallback"
        );

        let next = bridge
            .sender_state
            .lock()
            .await
            .next_envelope(
                NodeId::new(origin_identity().node_id),
                channel,
                OriginInboundSequence(2),
                envelope_claims(0),
                message_payload(),
            )
            .expect("lookup miss must leave the ordered channel usable");
        assert_eq!(next.sequence, OrderedRelaySequence::FIRST);
    }

    #[tokio::test]
    async fn relay_lookup_miss_retries_established_channel_at_missed_sequence() {
        let bridge = Arc::new(OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        ));
        let channel = envelope().channel;
        let mut receiver = super::super::ordered_relay::OrderedRelayReceiverState::default();
        let first = bridge
            .sender_state
            .lock()
            .await
            .next_envelope(
                NodeId::new(origin_identity().node_id),
                channel.clone(),
                OriginInboundSequence(1),
                envelope_claims(0),
                message_payload(),
            )
            .expect("first envelope allocates");
        let reserved = match receiver.reserve(first) {
            super::super::ordered_relay::OrderedRelayReservation::Reserved(reserved) => reserved,
            other => panic!("first envelope should reserve, got {other:?}"),
        };
        assert!(matches!(
            receiver.commit_reserved(*reserved),
            OrderedRelayReply::Ack(_)
        ));

        let missed = bridge
            .sender_state
            .lock()
            .await
            .next_envelope(
                NodeId::new(origin_identity().node_id),
                channel.clone(),
                OriginInboundSequence(2),
                envelope_claims(0),
                message_payload(),
            )
            .expect("second envelope allocates");
        assert_eq!(missed.sequence, OrderedRelaySequence(2));

        let outcome = OrderedRelayDeliveryBridge::finish_prepared_delivery_result(
            Arc::clone(&bridge),
            PreparedRemoteDelivery {
                services: Arc::new(
                    services_with_claims(
                        origin_identity(),
                        receiver_identity(),
                        receiver_identity(),
                        test_peer_id(),
                    )
                    .await,
                ),
                target_entity: target_entity(),
                previous_owner: receiver_identity(),
                channel: channel.clone(),
                envelope: missed,
                target: jid::Jid::from(target_full()),
                stanza: Stanza::Message(Message::new(Some(jid::Jid::from(target_full())))),
                is_iq: false,
            },
            Err(RelayAskError::NotFound {
                node_id: NodeId::new(receiver_identity().node_id),
            }),
        )
        .await;
        assert!(outcome.is_none());

        let retry = bridge
            .sender_state
            .lock()
            .await
            .next_envelope(
                NodeId::new(origin_identity().node_id),
                channel,
                OriginInboundSequence(3),
                envelope_claims(0),
                message_payload(),
            )
            .expect("lookup miss must retry at the missed sequence");
        assert_eq!(retry.sequence, OrderedRelaySequence(2));
        assert!(matches!(
            receiver.reserve(retry),
            super::super::ordered_relay::OrderedRelayReservation::Reserved(_)
        ));
    }

    #[tokio::test]
    async fn in_flight_nack_suppresses_fallback_without_join_repair() {
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let channel = envelope().channel;
        let outcome = OrderedRelayDeliveryBridge::finish_prepared_delivery_result(
            Arc::clone(&bridge),
            PreparedRemoteDelivery {
                services: Arc::new(
                    services_with_claims(
                        origin_identity(),
                        receiver_identity(),
                        receiver_identity(),
                        test_peer_id(),
                    )
                    .await,
                ),
                target_entity: target_entity(),
                previous_owner: receiver_identity(),
                channel: channel.clone(),
                envelope: envelope(),
                target: jid::Jid::from(target_full()),
                stanza: Stanza::Message(Message::new(Some(jid::Jid::from(target_full())))),
                is_iq: false,
            },
            Ok(OrderedRelayReply::Nack(OrderedRelayNack {
                channel,
                sequence: OrderedRelaySequence::FIRST,
                reason: OrderedRelayNackReason::InFlight,
            })),
        )
        .await
        .expect("InFlight is an attempted delivery outcome");

        assert_eq!(outcome.delivery, FullJidDeliveryOutcome::Dropped);
        assert!(outcome.maybe_committed);
        assert!(
            !outcome.join_repair_allowed,
            "duplicate pending receiver effect must not race MUC join repair"
        );
    }

    #[tokio::test]
    async fn same_owner_target_not_owner_nack_diverts_rejected_channel() {
        let services = services_with_claims(
            origin_identity(),
            receiver_identity(),
            origin_identity(),
            test_peer_id(),
        )
        .await;
        let nack = OrderedRelayNack {
            channel: envelope().channel,
            sequence: OrderedRelaySequence(5),
            reason: OrderedRelayNackReason::NotOwner {
                role: OrderedRelayClaimRole::Target,
            },
        };
        let (outcome, action, maybe_committed) = outcome_for_nack(
            &services,
            &target_entity(),
            &receiver_identity(),
            &nack,
            true,
        )
        .await;
        assert_eq!(outcome, Some(FullJidDeliveryOutcome::Unavailable));
        assert!(!maybe_committed);
        assert_eq!(
            action,
            NackChannelAction::Divert(OrderedRelayDiversionReason::NotOwner)
        );

        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let channel = envelope().channel;
        bridge
            .apply_nack_channel_action(channel.clone(), action)
            .await;

        let diverted = bridge
            .sender_state
            .lock()
            .await
            .next_envelope(
                NodeId::new(origin_identity().node_id),
                channel,
                OriginInboundSequence(6),
                envelope_claims(1),
                message_payload(),
            )
            .expect_err("same-owner claim churn must divert the rejected channel");
        assert_eq!(diverted.reason, OrderedRelayDiversionReason::NotOwner);
    }

    #[test]
    fn handoff_completion_synthesizes_iq_fallback_only_for_unavailable() {
        let mut iq = xmpp_parsers::iq::Iq::from_get("iq-1", xmpp_parsers::ping::Ping);
        *iq.to_mut() = Some(jid::Jid::from(target_full()));
        let stanza = Stanza::Iq(Box::new(iq));

        assert_eq!(
            replies_for_origin_handoff(&stanza, FullJidDeliveryOutcome::Delivered).len(),
            0
        );
        assert_eq!(
            replies_for_origin_handoff(&stanza, FullJidDeliveryOutcome::Dropped).len(),
            0
        );
        assert_eq!(
            replies_for_origin_handoff(&stanza, FullJidDeliveryOutcome::Unavailable).len(),
            1
        );
    }

    #[test]
    fn muc_join_maybe_committed_keeps_join_specific_outcome() {
        assert!(matches!(
            muc_proxy_result_to_ordered_outcome(
                OrderedRelayMucProxyKind::JoinPresence,
                Err(OrderedRelayNackReason::MaybeCommitted)
            ),
            OrderedRelayMucProxyOutcome::JoinMaybeCommitted
        ));
        assert!(matches!(
            muc_proxy_result_to_ordered_outcome(
                OrderedRelayMucProxyKind::OccupantPresence,
                Err(OrderedRelayNackReason::MaybeCommitted)
            ),
            OrderedRelayMucProxyOutcome::MaybeCommitted
        ));

        let room_jid: jid::BareJid = "room@muc.example.test".parse().expect("room jid");
        let target = RemoteResourceRouteTarget::MucProxy {
            room_jid,
            kind: OrderedRelayMucProxyKind::JoinPresence,
            stanza: RemoteStanza(Stanza::Presence(xmpp_parsers::presence::Presence::new(
                xmpp_parsers::presence::Type::None,
            ))),
        };
        let maybe_committed = RelayAskError::Send {
            failure: RelaySendFailure::ReplyTimeout,
            effect: RelaySendEffect::MaybeCommitted,
            message: "reply timeout after enqueue".to_string(),
        };
        assert!(matches!(
            remote_resource_muc_ask_error_outcome(&target, &maybe_committed),
            OrderedRelayMucProxyOutcome::JoinMaybeCommitted
        ));
    }

    #[test]
    fn iq_ask_error_classifier_falls_back_only_for_definite_no_effect_failures() {
        let not_found = RelayAskError::NotFound {
            node_id: NodeId::new("missing-node".to_string()),
        };
        assert!(ask_error_allows_target_refresh(&not_found));
        assert_eq!(outcome_for_ask_error(&not_found, true), None);
        let mailbox_full = RelayAskError::Send {
            failure: RelaySendFailure::MailboxFull,
            effect: RelaySendEffect::NoEffect,
            message: "mailbox full".to_string(),
        };
        assert!(ask_error_allows_target_refresh(&mailbox_full));
        assert_eq!(
            outcome_for_ask_error(&mailbox_full, true),
            Some(FullJidDeliveryOutcome::Unavailable)
        );
        assert_eq!(channel_diversion_for_ask_error(&not_found), None);
        assert_eq!(
            channel_diversion_for_ask_error(&mailbox_full),
            Some(OrderedRelayDiversionReason::Backpressure)
        );
        let stale_ref = RelayAskError::Send {
            failure: RelaySendFailure::StaleRef,
            effect: RelaySendEffect::NoEffect,
            message: "actor not running before enqueue".to_string(),
        };
        assert!(ask_error_allows_target_refresh(&stale_ref));
        assert_eq!(
            outcome_for_ask_error(&stale_ref, true),
            Some(FullJidDeliveryOutcome::Unavailable)
        );
        let reply_timeout = RelayAskError::Send {
            failure: RelaySendFailure::ReplyTimeout,
            effect: RelaySendEffect::MaybeCommitted,
            message: "reply timeout".to_string(),
        };
        assert!(!ask_error_allows_target_refresh(&reply_timeout));
        assert_eq!(
            outcome_for_ask_error(&reply_timeout, true),
            Some(FullJidDeliveryOutcome::MaybeCommitted)
        );
        let codec_after_handler = RelayAskError::Send {
            failure: RelaySendFailure::Codec,
            effect: RelaySendEffect::MaybeCommitted,
            message: "reply codec failed after handler".to_string(),
        };
        assert!(!ask_error_allows_target_refresh(&codec_after_handler));
        assert_eq!(
            outcome_for_ask_error(&codec_after_handler, true),
            Some(FullJidDeliveryOutcome::MaybeCommitted)
        );
        assert!(!ask_error_allows_target_refresh(&RelayAskError::Cancelled));
        assert_eq!(
            channel_diversion_for_ask_error(&RelayAskError::Cancelled),
            Some(OrderedRelayDiversionReason::Unreachable)
        );
    }
}
