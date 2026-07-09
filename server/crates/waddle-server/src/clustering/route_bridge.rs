//! ADR-0017 Phase 4 Slice 3: ordered-relay bridge for cross-node 1:1
//! full-JID delivery.
//!
//! The relay actor is spawned during clustering bring-up, before the
//! WebSocket routing graph exists. This bridge mirrors
//! [`super::resume_bridge::ResumeStealBridge`]: construct it empty at swarm
//! spawn time, then wire the narrow services it needs once
//! `create_websocket_state` has built the live registries.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::Duration;

use kameo::actor::ActorRef;
use libp2p::identity::{Keypair, PublicKey};
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use waddle_xmpp::ownership::{
    ClaimEpoch, ClaimSnapshot, ClaimStore, Entity, EntityType, NodeIdentity, SharedNodeIdentity,
};
use waddle_xmpp::protocol::CarbonKind;
use waddle_xmpp::registry::{
    BroadcastOutcome, ConnectionEntry, ConnectionPlacement, ConnectionRegistry, DeliveryKind,
    ForceDetachOutcome, ForceDetachReason, ForceDetachRequest, GetOrCreateUser, OutboundStanza,
    PresenceState, RegisterUserResourceIfOwnerOrAbsent, UnregisterUserResource, UserRegistryActor,
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
    RelayRemoteResourceUnregisterStatus, RelayRemoteResourceUpdateReply,
    RelayRemoteResourceUpdateStatus, RelayRemoteUserSideEffect, RelayRemoteUserSideEffectReply,
    RelayRemoteUserSideEffectStatus, RelayRouteRemoteResourceStanza,
    RelayRouteRemoteResourceStanzaReply, RelaySendEffect, RelaySendFailure,
    RelayUnregisterRemoteUserResource, RelayUpdateRemoteUserResource,
};
pub use super::remote_resource_admission::{
    RemoteResourceAdmissionEpoch, RemoteResourceRegistrationId,
};
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
const MAX_PENDING_REMOTE_SOCKET_CLEANUPS_PER_JID: usize = 8;
const MAX_REMOTE_SOCKET_CLEANUP_REGISTRATIONS: usize = 4096;
const REMOTE_SOCKET_GLOBAL_CLEANUP_RETRY_BATCH: usize = 32;
const MAX_PENDING_REMOTE_OWNER_CLEANUPS_PER_JID: usize = 8;
const MAX_REMOTE_OWNER_CLEANUP_REGISTRATIONS: usize = 4096;
const REMOTE_OWNER_GLOBAL_CLEANUP_RETRY_BATCH: usize = 32;
const REMOTE_RESOURCE_OUTBOUND_CHANNEL_SIZE: usize = 256;
#[cfg(not(test))]
const REMOTE_RESOURCE_STALE_DETACH_TIMEOUT: Duration = Duration::from_secs(1);
#[cfg(test)]
const REMOTE_RESOURCE_STALE_DETACH_TIMEOUT: Duration = Duration::from_millis(50);

type RemoteDeliveryFuture<'a> =
    Pin<Box<dyn Future<Output = Option<FullJidDeliveryOutcome>> + Send + 'a>>;

/// Narrow service bundle needed by the relay receiver to validate ownership
/// and hand an inbound full-JID stanza to the local `UserActor` delivery path.
pub struct OrderedRelayDeliveryServices {
    pub claim_store: Arc<dyn ClaimStore>,
    pub remote_resource_admission_store:
        Arc<dyn super::remote_resource_admission::RemoteResourceAdmissionStore>,
    pub allowlist_store: Arc<dyn AllowlistStore>,
    pub node_lease: Arc<dyn NodeLeaseStore>,
    pub node_identity: SharedNodeIdentity,
    pub connection_registry: Arc<ConnectionRegistry>,
    pub user_registry: ActorRef<UserRegistryActor>,
    pub sm_session_registry: Arc<InMemorySmSessionRegistry>,
    pub blocking_storage: Arc<dyn BlockingStorage>,
    pub web_socket_state: Weak<WebSocketState>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct RemoteResourceSocketGeneration(u64);

impl RemoteResourceSocketGeneration {
    #[cfg(test)]
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
    pub admission_epoch: RemoteResourceAdmissionEpoch,
    pub socket_generation: RemoteResourceSocketGeneration,
    pub socket_node: NodeIdentity,
    pub expected_user_owner: NodeIdentity,
    pub expected_user_claim_epoch: ClaimEpoch,
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
    Failed,
}

/// Exact cluster-wide identity of one physical full-JID socket admission.
///
/// Every clustered socket carries this token, including a socket colocated
/// with its authoritative `UserActor`. It is deliberately independent of
/// placement: moving the actor must never create a second ordering domain for
/// the same physical full JID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PhysicalResourceAdmissionToken {
    registration_id: RemoteResourceRegistrationId,
    admission_epoch: RemoteResourceAdmissionEpoch,
    socket_generation: RemoteResourceSocketGeneration,
    socket_node: NodeIdentity,
}

/// Exact admission proof for one inbound physical socket. Local placements
/// continue through the ordinary in-process SM/entity path; only a physical
/// socket whose authoritative `UserActor` is remote becomes a relay origin.
pub(crate) enum PhysicalResourceRouteOrigin {
    LocalSocket,
    RemoteMirror(RemoteResourceOriginSnapshot),
}

#[derive(Debug, Clone)]
struct RemoteSocketRegistration {
    registration_id: RemoteResourceRegistrationId,
    admission_epoch: RemoteResourceAdmissionEpoch,
    socket_generation: RemoteResourceSocketGeneration,
    socket_node: NodeIdentity,
    owner: Arc<AtomicBool>,
    user_owner: NodeIdentity,
    user_claim_epoch: ClaimEpoch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteResourceOriginSnapshot {
    pub jid: jid::FullJid,
    pub registration_id: RemoteResourceRegistrationId,
    pub admission_epoch: RemoteResourceAdmissionEpoch,
    pub socket_generation: RemoteResourceSocketGeneration,
    pub socket_node: NodeIdentity,
    pub user_owner: NodeIdentity,
    pub user_claim_epoch: ClaimEpoch,
}

#[derive(Debug, Clone)]
struct RemoteOwnerRegistration {
    registration_id: RemoteResourceRegistrationId,
    admission_epoch: RemoteResourceAdmissionEpoch,
    socket_node: NodeIdentity,
    socket_generation: RemoteResourceSocketGeneration,
    user_owner: NodeIdentity,
    user_claim_epoch: ClaimEpoch,
    owner: Arc<AtomicBool>,
    placement: ConnectionPlacement,
}

/// Holds the per-full-JID publication lock from durable reservation through
/// final XEP-0198 registration. A newer node may reserve a later durable epoch
/// while this guard is held, but its owner-side publication cannot interleave
/// with this one; the final exact reproof makes the older publisher roll back.
pub(crate) struct PhysicalResourceRegistrationGuard {
    jid: jid::FullJid,
    registration: RemoteSocketRegistration,
    fence_generation: u64,
    lock: Arc<Mutex<()>>,
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

impl PhysicalResourceRegistrationGuard {
    pub(crate) fn token(&self) -> PhysicalResourceAdmissionToken {
        PhysicalResourceAdmissionToken {
            registration_id: self.registration.registration_id,
            admission_epoch: self.registration.admission_epoch,
            socket_generation: self.registration.socket_generation,
            socket_node: self.registration.socket_node.clone(),
        }
    }
}

struct RemoteOwnerOperationGuard {
    inflight: Arc<StdMutex<HashSet<RemoteResourceRegistrationId>>>,
    registration_id: RemoteResourceRegistrationId,
}

impl RemoteOwnerOperationGuard {
    fn begin(
        inflight: Arc<StdMutex<HashSet<RemoteResourceRegistrationId>>>,
        registration_id: RemoteResourceRegistrationId,
    ) -> Option<Self> {
        let inserted = inflight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(registration_id);
        inserted.then_some(Self {
            inflight,
            registration_id,
        })
    }
}

impl Drop for RemoteOwnerOperationGuard {
    fn drop(&mut self) {
        self.inflight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.registration_id);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteUserClaimValidationError {
    Stale,
    Unavailable,
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
    asserted_origin_node: NodeIdentity,
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
    remote_socket_pending_cleanup:
        Mutex<HashMap<RemoteResourceRegistrationId, (jid::FullJid, RemoteSocketRegistration)>>,
    remote_socket_generation: AtomicU64,
    remote_owner_resources: Mutex<HashMap<jid::FullJid, RemoteOwnerRegistration>>,
    remote_owner_pending_cleanup:
        Mutex<HashMap<RemoteResourceRegistrationId, (jid::FullJid, RemoteOwnerRegistration)>>,
    remote_owner_cleanup_inflight: Arc<StdMutex<HashSet<RemoteResourceRegistrationId>>>,
    remote_owner_registration_locks: Mutex<HashMap<jid::FullJid, Arc<Mutex<()>>>>,
    remote_resource_fence_generation: AtomicU64,
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

impl OrderedRelayDeliveryBridge {
    pub fn new(stop_token: CancellationToken, messaging: &ClusteringMessagingConfig) -> Arc<Self> {
        Arc::new(Self {
            services: OnceLock::new(),
            origin_signer: OnceLock::new(),
            sender_state: Mutex::new(OrderedRelaySenderState::default()),
            channel_locks: Mutex::new(HashMap::new()),
            remote_socket_resources: Mutex::new(HashMap::new()),
            remote_socket_pending_cleanup: Mutex::new(HashMap::new()),
            remote_socket_generation: AtomicU64::new(0),
            remote_owner_resources: Mutex::new(HashMap::new()),
            remote_owner_pending_cleanup: Mutex::new(HashMap::new()),
            remote_owner_cleanup_inflight: Arc::new(StdMutex::new(HashSet::new())),
            remote_owner_registration_locks: Mutex::new(HashMap::new()),
            remote_resource_fence_generation: AtomicU64::new(0),
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

    /// Drop every active remote-resource placement record after this node
    /// self-fences. Connection/UserActor teardown owns the actual entry
    /// retirement; this clears the bridge's routing indexes so a fresh node
    /// epoch cannot inherit stale owner or socket registrations.
    ///
    /// Socket generations intentionally survive the fence. `node_id` is the
    /// process-stable relay address, so a resource that reconnects after the
    /// epoch rotates must publish a generation strictly newer than the one an
    /// owner may still remember for that same socket node.
    pub(crate) async fn clear_remote_resource_state_on_self_fence(&self) {
        let socket_registrations = {
            let mut registrations = self.remote_socket_resources.lock().await;
            if self
                .remote_resource_fence_generation
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |generation| {
                    generation.checked_add(1)
                })
                .is_err()
            {
                tracing::error!(
                    "remote-resource fence generation exhausted; bridge remains permanently fenced"
                );
            }
            std::mem::take(&mut *registrations)
        };
        if !socket_registrations.is_empty() {
            let mut pending = self.remote_socket_pending_cleanup.lock().await;
            for (jid, registration) in socket_registrations {
                pending.insert(registration.registration_id, (jid, registration));
            }
        }
        let owner_registrations = {
            let mut registrations = self.remote_owner_resources.lock().await;
            std::mem::take(&mut *registrations)
        };
        if !owner_registrations.is_empty() {
            let mut pending = self.remote_owner_pending_cleanup.lock().await;
            for (jid, registration) in owner_registrations {
                if !Self::insert_remote_owner_cleanup_reservation(&mut pending, &jid, &registration)
                {
                    tracing::error!(
                        jid = %jid,
                        registration_id = ?registration.registration_id,
                        "self-fenced remote-owner registration lacked bounded cleanup capacity"
                    );
                }
            }
        }
        if let Some(services) = self.services.get() {
            let mut seen = HashSet::new();
            let mut cancellations = Vec::new();
            {
                let pending = self.remote_socket_pending_cleanup.lock().await;
                for (jid, registration) in pending.values() {
                    if seen.insert(registration.registration_id) {
                        cancellations.push((
                            jid.clone(),
                            registration.registration_id,
                            registration.admission_epoch,
                            registration.socket_node.clone(),
                        ));
                    }
                }
            }
            {
                let pending = self.remote_owner_pending_cleanup.lock().await;
                for (jid, registration) in pending.values() {
                    if seen.insert(registration.registration_id) {
                        cancellations.push((
                            jid.clone(),
                            registration.registration_id,
                            registration.admission_epoch,
                            registration.socket_node.clone(),
                        ));
                    }
                }
            }
            for batch in cancellations.chunks(REMOTE_SOCKET_GLOBAL_CLEANUP_RETRY_BATCH) {
                let attempts = batch
                    .iter()
                    .map(|(jid, registration_id, epoch, socket_node)| {
                        services.remote_resource_admission_store.cancel(
                            jid,
                            *registration_id,
                            *epoch,
                            socket_node,
                        )
                    });
                for result in futures::future::join_all(attempts).await {
                    if let Err(error) = result {
                        tracing::warn!(
                            %error,
                            "self-fence could not revoke an exact remote-resource admission"
                        );
                    }
                }
            }
        }
        // Do not clear the per-JID lock index here: an old registration task
        // may still hold a strong reference and guard. Fence-generation
        // checks invalidate its work; normal strong-count cleanup retires the
        // lock only after every overlapping guard is gone.
    }

    fn remote_resource_registration_allowed(
        &self,
        services: &OrderedRelayDeliveryServices,
        fence_generation: u64,
    ) -> bool {
        if fence_generation == u64::MAX
            || self.remote_resource_fence_generation.load(Ordering::SeqCst) != fence_generation
        {
            return false;
        }
        services
            .web_socket_state
            .upgrade()
            .is_none_or(|state| state.deps.app_state.clustering_readiness.is_ready())
    }

    fn next_remote_socket_generation(&self) -> Option<RemoteResourceSocketGeneration> {
        let previous = self
            .remote_socket_generation
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .ok()?;
        previous.checked_add(1).map(RemoteResourceSocketGeneration)
    }

    pub(crate) async fn forget_remote_resource_state_if_owner(
        &self,
        jid: &jid::FullJid,
        owner: &Arc<AtomicBool>,
        placement: waddle_xmpp::registry::ConnectionPlacement,
    ) {
        match placement {
            waddle_xmpp::registry::ConnectionPlacement::LocalSocket => {
                let registration = {
                    let mut registrations = self.remote_socket_resources.lock().await;
                    if registrations
                        .get(jid)
                        .is_some_and(|registration| Arc::ptr_eq(&registration.owner, owner))
                    {
                        registrations.remove(jid)
                    } else {
                        None
                    }
                };
                if let Some(registration) = registration {
                    // Physical retirement is not proof that a remote owner
                    // mirror was removed. Keep the exact tuple until the
                    // unregister path receives a terminal owner reply.
                    self.retain_remote_socket_cleanup(jid, &registration).await;
                    if let Some(services) = self.services.get() {
                        self.cancel_remote_resource_admission(services, jid, &registration)
                            .await;
                    }
                }
                let mut removed_owner = Vec::new();
                {
                    let mut registrations = self.remote_owner_resources.lock().await;
                    if registrations.get(jid).is_some_and(|registration| {
                        registration.placement == ConnectionPlacement::LocalSocket
                            && Arc::ptr_eq(&registration.owner, owner)
                    }) {
                        if let Some(registration) = registrations.remove(jid) {
                            removed_owner.push(registration);
                        }
                    }
                }
                self.remote_owner_pending_cleanup.lock().await.retain(
                    |_, (pending_jid, registration)| {
                        let keep = pending_jid != jid
                            || registration.placement != ConnectionPlacement::LocalSocket
                            || !Arc::ptr_eq(&registration.owner, owner);
                        if !keep {
                            removed_owner.push(registration.clone());
                        }
                        keep
                    },
                );
                if let Some(services) = self.services.get() {
                    for registration in removed_owner {
                        if !self
                            .cancel_remote_owner_admission(services, jid, &registration)
                            .await
                        {
                            self.retain_remote_owner_cleanup(jid, &registration).await;
                        }
                    }
                }
            }
            waddle_xmpp::registry::ConnectionPlacement::RemoteMirror => {
                let mut removed = Vec::new();
                {
                    let mut registrations = self.remote_owner_resources.lock().await;
                    if registrations
                        .get(jid)
                        .is_some_and(|registration| Arc::ptr_eq(&registration.owner, owner))
                    {
                        if let Some(registration) = registrations.remove(jid) {
                            removed.push(registration);
                        }
                    }
                }
                self.remote_owner_pending_cleanup.lock().await.retain(
                    |_, (pending_jid, registration)| {
                        let keep = pending_jid != jid || !Arc::ptr_eq(&registration.owner, owner);
                        if !keep {
                            removed.push(registration.clone());
                        }
                        keep
                    },
                );
                if let Some(services) = self.services.get() {
                    for registration in removed {
                        if !self
                            .cancel_remote_owner_admission(services, jid, &registration)
                            .await
                        {
                            self.retain_remote_owner_cleanup(jid, &registration).await;
                        }
                    }
                }
            }
        }
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
        let registration = self
            .remote_socket_resources
            .lock()
            .await
            .get(jid)
            .filter(|registration| Arc::ptr_eq(&registration.owner, owner))?
            .clone();
        let services = self.services.get()?;
        services.connection_registry.entry_if_owner(jid, owner)?;
        Some(RemoteResourceOriginSnapshot {
            jid: jid.clone(),
            registration_id: registration.registration_id,
            admission_epoch: registration.admission_epoch,
            socket_generation: registration.socket_generation,
            socket_node: registration.socket_node.clone(),
            user_owner: registration.user_owner.clone(),
            user_claim_epoch: registration.user_claim_epoch,
        })
    }

    pub(crate) async fn physical_resource_origin_if_owner(
        &self,
        jid: &jid::FullJid,
        owner: &Arc<AtomicBool>,
        token: &PhysicalResourceAdmissionToken,
    ) -> Option<PhysicalResourceRouteOrigin> {
        let registration = self
            .remote_socket_resources
            .lock()
            .await
            .get(jid)
            .filter(|registration| {
                Arc::ptr_eq(&registration.owner, owner)
                    && physical_token_matches_registration(token, registration)
            })?
            .clone();
        let services = self.services.get()?;
        if services.node_identity.current() != registration.socket_node
            || services
                .connection_registry
                .entry_if_owner(jid, owner)
                .is_none()
            || services
                .web_socket_state
                .upgrade()
                .is_some_and(|state| !state.deps.app_state.clustering_readiness.is_ready())
        {
            self.detach_stale_remote_socket_resource(jid, &registration)
                .await;
            return None;
        }
        match exact_remote_user_claim_is_current(
            services,
            jid,
            &registration.user_owner,
            registration.user_claim_epoch,
        )
        .await
        {
            Ok(()) => {}
            Err(RemoteUserClaimValidationError::Stale) => {
                self.detach_stale_remote_socket_resource(jid, &registration)
                    .await;
                return None;
            }
            Err(RemoteUserClaimValidationError::Unavailable) => return None,
        }
        match exact_remote_resource_admission_is_current(
            services,
            jid,
            registration.registration_id,
            registration.admission_epoch,
            &registration.socket_node,
        )
        .await
        {
            Ok(()) => {}
            Err(RemoteUserClaimValidationError::Stale) => {
                self.detach_stale_remote_socket_resource(jid, &registration)
                    .await;
                return None;
            }
            Err(RemoteUserClaimValidationError::Unavailable) => return None,
        }
        if registration.user_owner == services.node_identity.current() {
            match self
                .local_owner_publication_is_current(services, jid, &registration)
                .await
            {
                Ok(()) => {}
                Err(RelayRemoteResourceRegistrationStatus::StaleRegistration) => {
                    self.detach_stale_remote_socket_resource(jid, &registration)
                        .await;
                    return None;
                }
                Err(_) => return None,
            }
        }
        let origin = RemoteResourceOriginSnapshot {
            jid: jid.clone(),
            registration_id: registration.registration_id,
            admission_epoch: registration.admission_epoch,
            socket_generation: registration.socket_generation,
            socket_node: registration.socket_node,
            user_owner: registration.user_owner,
            user_claim_epoch: registration.user_claim_epoch,
        };
        if origin.user_owner == services.node_identity.current() {
            Some(PhysicalResourceRouteOrigin::LocalSocket)
        } else {
            Some(PhysicalResourceRouteOrigin::RemoteMirror(origin))
        }
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
        let Some(services) = self.services.get().cloned() else {
            return Some(FullJidDeliveryOutcome::Unavailable);
        };
        match remote_owner_registration_is_current(&services, target, &registration).await {
            Ok(()) => {}
            Err(RelayRemoteResourceRegistrationStatus::StaleRegistration) => {
                let cleaned = self
                    .cleanup_remote_owner_resource_if_registration(
                        target,
                        registration.registration_id,
                    )
                    .await;
                if !cleaned
                    || services
                        .connection_registry
                        .entry_if_owner(target, &registration.owner)
                        .is_some()
                {
                    return Some(FullJidDeliveryOutcome::Unavailable);
                }
                return None;
            }
            Err(_) => return Some(FullJidDeliveryOutcome::Unavailable),
        }
        if registration.placement == ConnectionPlacement::LocalSocket {
            // The exact durable admission was proved above. The ordinary local
            // registry/actor path owns the actual enqueue, avoiding a needless
            // relay loopback while preserving the shared ordering boundary.
            return None;
        }
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
                asserted_origin_node: me.clone(),
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
                asserted_origin_node: me.clone(),
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
        if let Some(remote_origin) = remote_resource_origin(origin) {
            return Arc::clone(self)
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
                .await;
        }
        self.try_proxy_muc_remote_from_local_origin(room_jid, stanza, kind, origin)
            .await
    }

    async fn try_proxy_muc_remote_from_local_origin(
        self: &Arc<Self>,
        room_jid: &jid::BareJid,
        stanza: &Stanza,
        kind: OrderedRelayMucProxyKind,
        origin: &OrderedRelayRouteOrigin,
    ) -> Option<OrderedRelayMucProxyOutcome> {
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

        let (origin_entity, channel_origin) = route_origin_claim(&origin.kind);
        let origin_snapshot = current_claim(&services, &origin_entity).await?;
        if !origin_snapshot.owner_lease_fresh || origin_snapshot.owner != me {
            tracing::debug!(
                room = %room_jid,
                origin_entity = %origin_entity,
                "ordered relay: MUC origin entity is not currently owned locally; \
                 keeping local fallback path"
            );
            return None;
        }
        let sender_claim =
            current_fresh_local_relay_claim(&services, &origin.sender_entity, &me, "sender")
                .await?;

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
            asserted_origin_node: me.clone(),
            origin_inbound_sequence: OriginInboundSequence(origin.inbound_sequence),
            origin_claim: origin_claim.clone(),
            sender_claim: sender_claim.clone(),
            target_claim: target_claim.clone(),
            payload: payload.clone(),
            target: jid::Jid::from(room_jid.clone()),
            stanza: stanza.clone(),
            is_iq: matches!(stanza, Stanza::Iq(_)),
        };

        let outcome = Arc::clone(self).deliver_seeded_remote(seed, true).await?;
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
                            asserted_origin_node: me.clone(),
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
                            return Some(OrderedRelayMucProxyOutcome::Delivered(
                                retry.client_replies,
                            ));
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
                                return Some(OrderedRelayMucProxyOutcome::Delivered(
                                    repair.client_replies,
                                ));
                            }
                            FullJidDeliveryOutcome::Unavailable
                            | FullJidDeliveryOutcome::Dropped
                            | FullJidDeliveryOutcome::MaybeCommitted => {}
                        }
                    }
                }
                return Some(OrderedRelayMucProxyOutcome::JoinMaybeCommitted);
            }
            return Some(OrderedRelayMucProxyOutcome::MaybeCommitted);
        }

        Some(match outcome.delivery {
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

    /// Reserve the one cluster-wide physical-socket epoch before either the
    /// transport registry or the authoritative `UserActor` can observe this
    /// full JID. The returned guard serializes owner-side publication for the
    /// JID until [`Self::finalize_physical_user_resource`] or
    /// [`Self::abort_physical_user_resource`] consumes it.
    pub(crate) async fn begin_physical_user_resource(
        self: &Arc<Self>,
        jid: &jid::FullJid,
        owner: Arc<AtomicBool>,
    ) -> Result<PhysicalResourceRegistrationGuard, RemoteResourceRegisterOutcome> {
        let Some(lock) = self.lock_for_remote_owner_registration(jid).await else {
            return Err(RemoteResourceRegisterOutcome::Failed);
        };
        let guard = Arc::clone(&lock).lock_owned().await;
        let Some(services) = self.services.get().cloned() else {
            drop(guard);
            self.remove_remote_owner_registration_lock_if_unused(jid, &lock)
                .await;
            return Err(RemoteResourceRegisterOutcome::Failed);
        };
        let fence_generation = self.remote_resource_fence_generation.load(Ordering::SeqCst);
        if !self.remote_resource_registration_allowed(&services, fence_generation) {
            drop(guard);
            self.remove_remote_owner_registration_lock_if_unused(jid, &lock)
                .await;
            return Err(RemoteResourceRegisterOutcome::Failed);
        }

        // Locating or creating the authoritative actor does not publish this
        // full-JID resource. The physical epoch below is still reserved before
        // either ConnectionRegistry or UserActor resource registration.
        let target_entity = user_entity(&jid.to_bare());
        let mut target_snapshot = current_claim(&services, &target_entity).await;
        if target_snapshot.is_none() {
            let created = services
                .user_registry
                .ask(GetOrCreateUser {
                    bare_jid: jid.to_bare(),
                })
                .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
                .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
                .await;
            if created.is_err() {
                drop(guard);
                self.remove_remote_owner_registration_lock_if_unused(jid, &lock)
                    .await;
                return Err(RemoteResourceRegisterOutcome::Failed);
            }
            target_snapshot = current_claim(&services, &target_entity).await;
        }
        let Some(target_snapshot) = target_snapshot.filter(|snapshot| snapshot.owner_lease_fresh)
        else {
            drop(guard);
            self.remove_remote_owner_registration_lock_if_unused(jid, &lock)
                .await;
            return Err(RemoteResourceRegisterOutcome::Failed);
        };
        if !self.remote_resource_registration_allowed(&services, fence_generation) {
            drop(guard);
            self.remove_remote_owner_registration_lock_if_unused(jid, &lock)
                .await;
            return Err(RemoteResourceRegisterOutcome::Failed);
        }

        let registration_id = RemoteResourceRegistrationId::fresh();
        let Some(socket_generation) = self.next_remote_socket_generation() else {
            tracing::error!(
                "clustered physical-resource socket generation space exhausted; refusing registration"
            );
            drop(guard);
            self.remove_remote_owner_registration_lock_if_unused(jid, &lock)
                .await;
            return Err(RemoteResourceRegisterOutcome::Failed);
        };
        let socket_node = services.node_identity.current();
        if let Err(error) = services
            .remote_resource_admission_store
            .prune_stale(REMOTE_SOCKET_GLOBAL_CLEANUP_RETRY_BATCH)
            .await
        {
            tracing::warn!(%error, "failed bounded stale physical-resource admission cleanup");
        }
        let admission_epoch = match services
            .remote_resource_admission_store
            .reserve(jid, registration_id, &socket_node)
            .await
        {
            Ok(epoch) => epoch,
            Err(error) => {
                tracing::warn!(jid = %jid, %error, "failed to reserve physical-resource admission");
                drop(guard);
                self.remove_remote_owner_registration_lock_if_unused(jid, &lock)
                    .await;
                return Err(RemoteResourceRegisterOutcome::Failed);
            }
        };
        let registration = RemoteSocketRegistration {
            registration_id,
            admission_epoch,
            socket_generation,
            socket_node,
            owner,
            user_owner: target_snapshot.owner,
            user_claim_epoch: target_snapshot.claim_epoch,
        };

        // Reserving the newer durable epoch irrevocably superseded the previous
        // physical socket. Retire that exact local placement before publishing
        // the new map entry, even if a later capacity/publication step fails.
        let displaced_socket = self.remote_socket_resources.lock().await.get(jid).cloned();
        if let Some(displaced) = displaced_socket {
            self.detach_remote_socket_resource(
                jid,
                &displaced,
                ForceDetachReason::ResourceReplaced,
            )
            .await;
        }

        if !self
            .reserve_remote_socket_cleanup_capacity(jid, &registration)
            .await
        {
            if self.remote_socket_cleanup_global_capacity_exhausted().await {
                self.retry_inactive_remote_socket_cleanups(
                    REMOTE_SOCKET_GLOBAL_CLEANUP_RETRY_BATCH,
                )
                .await;
            }
            if !self
                .reserve_remote_socket_cleanup_capacity(jid, &registration)
                .await
            {
                self.cancel_remote_resource_admission(&services, jid, &registration)
                    .await;
                drop(guard);
                self.remove_remote_owner_registration_lock_if_unused(jid, &lock)
                    .await;
                return Err(RemoteResourceRegisterOutcome::Failed);
            }
        }
        if !self
            .publish_pending_remote_socket_registration(jid, registration.clone())
            .await
        {
            if self
                .cancel_remote_resource_admission(&services, jid, &registration)
                .await
            {
                self.remove_pending_remote_socket_cleanup_if_current(jid, &registration)
                    .await;
            }
            drop(guard);
            self.remove_remote_owner_registration_lock_if_unused(jid, &lock)
                .await;
            return Err(RemoteResourceRegisterOutcome::Failed);
        }

        Ok(PhysicalResourceRegistrationGuard {
            jid: jid.clone(),
            registration,
            fence_generation,
            lock,
            _guard: guard,
        })
    }

    /// Publish the already-reserved physical socket into its authoritative
    /// owner. Local and remote placements share the exact same durable tuple;
    /// only the final delivery adapter differs.
    pub(crate) async fn publish_physical_user_resource(
        self: &Arc<Self>,
        guard: &PhysicalResourceRegistrationGuard,
        entry: ConnectionEntry,
    ) -> RemoteResourceRegisterOutcome {
        let Some(services) = self.services.get().cloned() else {
            return RemoteResourceRegisterOutcome::Failed;
        };
        if !self
            .physical_registration_is_current(&services, guard, true)
            .await
        {
            return RemoteResourceRegisterOutcome::Failed;
        }
        let registration = &guard.registration;
        if registration.user_owner == services.node_identity.current() {
            return self
                .publish_local_owner_physical_resource(&services, guard, entry)
                .await;
        }

        let state = RemoteResourceStateSnapshot::from_entry(
            &entry,
            services.connection_registry.get_presence_state(&guard.jid),
        );
        let mut handle = RelayHandle::new(
            NodeId::new(registration.user_owner.node_id.clone()),
            self.stop_token.clone(),
        )
        .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        let reply = handle
            .register_remote_user_resource(RelayRegisterRemoteUserResource {
                jid: guard.jid.clone(),
                registration_id: registration.registration_id,
                admission_epoch: registration.admission_epoch,
                socket_generation: registration.socket_generation,
                socket_node: registration.socket_node.clone(),
                expected_user_owner: registration.user_owner.clone(),
                expected_user_claim_epoch: registration.user_claim_epoch,
                state,
            })
            .await;
        match reply {
            Ok(RelayRemoteResourceRegistrationReply {
                status: RelayRemoteResourceRegistrationStatus::Registered,
            }) if self
                .physical_registration_is_current(&services, guard, true)
                .await =>
            {
                RemoteResourceRegisterOutcome::Registered
            }
            Ok(RelayRemoteResourceRegistrationReply {
                status: RelayRemoteResourceRegistrationStatus::NotOwner,
            }) => RemoteResourceRegisterOutcome::NotRemote,
            Ok(_) => RemoteResourceRegisterOutcome::Failed,
            Err(error) => {
                tracing::warn!(
                    jid = %guard.jid,
                    owner_node = %registration.user_owner.node_id,
                    %error,
                    "clustered physical-resource owner publication failed"
                );
                if matches!(
                    error,
                    RelayAskError::Send {
                        effect: RelaySendEffect::MaybeCommitted,
                        ..
                    } | RelayAskError::Cancelled
                ) {
                    let _ = self
                        .compensate_remote_socket_registration(
                            &mut handle,
                            &guard.jid,
                            registration,
                        )
                        .await;
                }
                RemoteResourceRegisterOutcome::Failed
            }
        }
    }

    async fn publish_local_owner_physical_resource(
        self: &Arc<Self>,
        services: &OrderedRelayDeliveryServices,
        guard: &PhysicalResourceRegistrationGuard,
        entry: ConnectionEntry,
    ) -> RemoteResourceRegisterOutcome {
        let registration = &guard.registration;
        if let Some(displaced) = self
            .remote_owner_resources
            .lock()
            .await
            .get(&guard.jid)
            .cloned()
        {
            if !remote_socket_and_owner_registration_match(registration, &displaced)
                && !self
                    .retire_remote_owner_registration(services, &guard.jid, &displaced)
                    .await
            {
                return RemoteResourceRegisterOutcome::Failed;
            }
            self.remove_remote_owner_registration_if_current(&guard.jid, &displaced)
                .await;
        }
        let owner_registration = RemoteOwnerRegistration {
            registration_id: registration.registration_id,
            admission_epoch: registration.admission_epoch,
            socket_node: registration.socket_node.clone(),
            socket_generation: registration.socket_generation,
            user_owner: registration.user_owner.clone(),
            user_claim_epoch: registration.user_claim_epoch,
            owner: registration.owner.clone(),
            placement: ConnectionPlacement::LocalSocket,
        };
        let Some(_operation) = self
            .reserve_remote_owner_cleanup_capacity(&guard.jid, &owner_registration)
            .await
        else {
            return RemoteResourceRegisterOutcome::Failed;
        };
        if !self
            .physical_registration_is_current(services, guard, true)
            .await
        {
            self.remove_pending_remote_owner_cleanup_if_current(&guard.jid, &owner_registration)
                .await;
            return RemoteResourceRegisterOutcome::Failed;
        }
        let registered = services
            .user_registry
            .ask(RegisterUserResourceIfOwnerOrAbsent {
                jid: guard.jid.clone(),
                entry,
                owner: registration.owner.clone(),
            })
            .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
            .await;
        if !matches!(registered, Ok(true))
            || !self
                .physical_registration_is_current(services, guard, true)
                .await
        {
            self.retain_remote_owner_cleanup(&guard.jid, &owner_registration)
                .await;
            return RemoteResourceRegisterOutcome::Failed;
        }
        self.remote_owner_resources
            .lock()
            .await
            .insert(guard.jid.clone(), owner_registration);
        if self
            .physical_registration_is_current(services, guard, true)
            .await
            && self
                .local_owner_publication_is_current(services, &guard.jid, registration)
                .await
                .is_ok()
        {
            RemoteResourceRegisterOutcome::Registered
        } else {
            RemoteResourceRegisterOutcome::Failed
        }
    }

    async fn physical_registration_is_current(
        &self,
        services: &OrderedRelayDeliveryServices,
        guard: &PhysicalResourceRegistrationGuard,
        require_registry: bool,
    ) -> bool {
        let registration = &guard.registration;
        if !self.remote_resource_registration_allowed(services, guard.fence_generation)
            || services.node_identity.current() != registration.socket_node
            || !self
                .remote_socket_registration_is_current(&guard.jid, registration)
                .await
        {
            return false;
        }
        if require_registry
            && services
                .connection_registry
                .entry_if_owner(&guard.jid, &registration.owner)
                .is_none()
        {
            return false;
        }
        exact_remote_user_claim_is_current(
            services,
            &guard.jid,
            &registration.user_owner,
            registration.user_claim_epoch,
        )
        .await
        .is_ok()
            && exact_remote_resource_admission_is_current(
                services,
                &guard.jid,
                registration.registration_id,
                registration.admission_epoch,
                &registration.socket_node,
            )
            .await
            .is_ok()
    }

    async fn local_owner_publication_is_current(
        &self,
        services: &OrderedRelayDeliveryServices,
        jid: &jid::FullJid,
        socket: &RemoteSocketRegistration,
    ) -> Result<(), RelayRemoteResourceRegistrationStatus> {
        let owner_registration = self
            .remote_owner_resources
            .lock()
            .await
            .get(jid)
            .filter(|registration| {
                registration.placement == ConnectionPlacement::LocalSocket
                    && remote_socket_and_owner_registration_match(socket, registration)
            })
            .cloned()
            .ok_or(RelayRemoteResourceRegistrationStatus::StaleRegistration)?;
        remote_owner_registration_is_current(services, jid, &owner_registration).await
    }

    pub(crate) async fn finalize_physical_user_resource(
        self: &Arc<Self>,
        guard: PhysicalResourceRegistrationGuard,
    ) -> Option<PhysicalResourceAdmissionToken> {
        let current = match self.services.get() {
            Some(services) => {
                let physical = self
                    .physical_registration_is_current(services, &guard, true)
                    .await;
                physical
                    && (guard.registration.user_owner != services.node_identity.current()
                        || self
                            .local_owner_publication_is_current(
                                services,
                                &guard.jid,
                                &guard.registration,
                            )
                            .await
                            .is_ok())
            }
            None => false,
        };
        if !current {
            let jid = guard.jid.clone();
            let owner = guard.registration.owner.clone();
            drop(guard);
            self.unregister_remote_user_resource_if_owner(&jid, &owner)
                .await;
            return None;
        }
        let token = Some(guard.token());
        let jid = guard.jid.clone();
        let lock = Arc::clone(&guard.lock);
        drop(guard);
        self.remove_remote_owner_registration_lock_if_unused(&jid, &lock)
            .await;
        token
    }

    pub(crate) async fn abort_physical_user_resource(
        self: &Arc<Self>,
        guard: PhysicalResourceRegistrationGuard,
    ) {
        let jid = guard.jid.clone();
        let owner = guard.registration.owner.clone();
        drop(guard);
        self.unregister_remote_user_resource_if_owner(&jid, &owner)
            .await;
    }

    async fn publish_pending_remote_socket_registration(
        &self,
        jid: &jid::FullJid,
        registration: RemoteSocketRegistration,
    ) -> bool {
        let mut registrations = self.remote_socket_resources.lock().await;
        match registrations.get(jid) {
            Some(current) if current.socket_generation > registration.socket_generation => false,
            Some(current)
                if current.socket_generation == registration.socket_generation
                    && !remote_socket_registration_matches(current, &registration) =>
            {
                false
            }
            _ => {
                registrations.insert(jid.clone(), registration);
                true
            }
        }
    }

    pub(crate) async fn unregister_remote_user_resource_if_owner(
        &self,
        jid: &jid::FullJid,
        owner: &Arc<AtomicBool>,
    ) {
        let Some(lock) = self.lock_for_remote_owner_registration(jid).await else {
            return;
        };
        let guard = lock.lock().await;
        self.unregister_remote_user_resource_if_owner_locked(jid, owner)
            .await;
        drop(guard);
        self.remove_remote_owner_registration_lock_if_unused(jid, &lock)
            .await;
    }

    async fn unregister_remote_user_resource_if_owner_locked(
        &self,
        jid: &jid::FullJid,
        owner: &Arc<AtomicBool>,
    ) {
        let mut registrations_by_id = HashMap::new();
        if let Some(registration) = self
            .remote_socket_resources
            .lock()
            .await
            .get(jid)
            .filter(|registration| Arc::ptr_eq(&registration.owner, owner))
            .cloned()
        {
            registrations_by_id.insert(registration.registration_id, registration);
        }
        {
            let pending = self.remote_socket_pending_cleanup.lock().await;
            for (registration_id, (pending_jid, registration)) in pending.iter() {
                if pending_jid == jid && Arc::ptr_eq(&registration.owner, owner) {
                    registrations_by_id
                        .entry(*registration_id)
                        .or_insert_with(|| registration.clone());
                }
            }
        }
        if registrations_by_id.is_empty() {
            return;
        }

        let Some(services) = self.services.get() else {
            for registration in registrations_by_id.into_values() {
                self.retain_remote_socket_cleanup(jid, &registration).await;
                self.remove_remote_socket_registration_if_current(jid, &registration)
                    .await;
            }
            return;
        };

        for registration in registrations_by_id.into_values() {
            // Revoke the durable admission before asking the owner to remove
            // its mirror. A delayed register message is then terminally stale
            // even when unregister reaches the owner first.
            if !self
                .cancel_remote_resource_admission(services, jid, &registration)
                .await
            {
                self.retain_remote_socket_cleanup(jid, &registration).await;
                self.remove_remote_socket_registration_if_current(jid, &registration)
                    .await;
                continue;
            }
            self.remove_remote_socket_registration_if_current(jid, &registration)
                .await;
            if registration.user_owner == services.node_identity.current() {
                let mut owner_registration = {
                    let active = self.remote_owner_resources.lock().await;
                    active
                        .get(jid)
                        .filter(|owner_registration| {
                            remote_socket_and_owner_registration_match(
                                &registration,
                                owner_registration,
                            )
                        })
                        .cloned()
                };
                if owner_registration.is_none() {
                    owner_registration = self
                        .remote_owner_pending_cleanup
                        .lock()
                        .await
                        .get(&registration.registration_id)
                        .filter(|(pending_jid, candidate)| {
                            pending_jid == jid
                                && remote_socket_and_owner_registration_match(
                                    &registration,
                                    candidate,
                                )
                        })
                        .map(|(_, candidate)| candidate.clone());
                }
                if let Some(owner_registration) = owner_registration {
                    if !unregister_remote_owner_actor_entry(
                        services,
                        jid,
                        &owner_registration.owner,
                    )
                    .await
                    {
                        self.retain_remote_socket_cleanup(jid, &registration).await;
                        self.retain_remote_owner_cleanup(jid, &owner_registration)
                            .await;
                        continue;
                    }
                    services
                        .connection_registry
                        .unregister_if_owner(jid, &owner_registration.owner);
                    self.remove_remote_owner_registration_if_current(jid, &owner_registration)
                        .await;
                    self.remove_pending_remote_owner_cleanup_if_current(jid, &owner_registration)
                        .await;
                }
                self.remove_pending_remote_socket_cleanup_if_current(jid, &registration)
                    .await;
                continue;
            }
            let mut handle = RelayHandle::new(
                NodeId::new(registration.user_owner.node_id.clone()),
                self.stop_token.clone(),
            )
            .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
            match handle
                .unregister_remote_user_resource(RelayUnregisterRemoteUserResource {
                    jid: jid.clone(),
                    registration_id: registration.registration_id,
                    admission_epoch: registration.admission_epoch,
                    socket_generation: registration.socket_generation,
                    socket_node: registration.socket_node.clone(),
                    expected_user_owner: registration.user_owner.clone(),
                    expected_user_claim_epoch: registration.user_claim_epoch,
                })
                .await
            {
                Ok(RelayRemoteResourceUnregisterReply {
                    status: RelayRemoteResourceUnregisterStatus::Terminal,
                }) => {
                    self.remove_remote_socket_registration_if_current(jid, &registration)
                        .await;
                    self.remove_pending_remote_socket_cleanup_if_current(jid, &registration)
                        .await;
                }
                Ok(RelayRemoteResourceUnregisterReply {
                    status: RelayRemoteResourceUnregisterStatus::Retry,
                }) => {
                    self.retain_remote_socket_cleanup(jid, &registration).await;
                    self.remove_remote_socket_registration_if_current(jid, &registration)
                        .await;
                    tracing::warn!(
                        jid = %jid,
                        registration_id = ?registration.registration_id,
                        "clustered remote-resource owner could not prove unregister terminal; retaining exact metadata for retry"
                    );
                }
                Err(error) => {
                    self.retain_remote_socket_cleanup(jid, &registration).await;
                    self.remove_remote_socket_registration_if_current(jid, &registration)
                        .await;
                    tracing::warn!(
                        jid = %jid,
                        registration_id = ?registration.registration_id,
                        %error,
                        "clustered remote-resource unregister remained uncertain; retaining exact metadata for retry"
                    );
                }
            }
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
        let Some(services) = self.services.get() else {
            return;
        };
        if registration.user_owner == services.node_identity.current() {
            // LocalSocket registrations share the same `ConnectionEntry`
            // with the authoritative actor, so the caller's local atomic
            // update is already visible. Only RemoteMirror state crosses the
            // relay boundary.
            return;
        }
        let mut handle = RelayHandle::new(
            NodeId::new(registration.user_owner.node_id.clone()),
            self.stop_token.clone(),
        )
        .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        match handle
            .update_remote_user_resource(RelayUpdateRemoteUserResource {
                jid: jid.clone(),
                registration_id: registration.registration_id,
                admission_epoch: registration.admission_epoch,
                socket_generation: registration.socket_generation,
                socket_node: registration.socket_node.clone(),
                expected_user_owner: registration.user_owner.clone(),
                expected_user_claim_epoch: registration.user_claim_epoch,
                update,
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
        let mut handle = RelayHandle::new(
            NodeId::new(registration.user_owner.node_id.clone()),
            self.stop_token.clone(),
        )
        .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        match handle
            .remote_user_side_effect(RelayRemoteUserSideEffect {
                source_jid: source_jid.clone(),
                registration_id: registration.registration_id,
                admission_epoch: registration.admission_epoch,
                socket_generation: registration.socket_generation,
                socket_node: registration.socket_node.clone(),
                expected_user_owner: registration.user_owner.clone(),
                expected_user_claim_epoch: registration.user_claim_epoch,
                effect,
            })
            .await
        {
            Ok(RelayRemoteUserSideEffectReply {
                status: RelayRemoteUserSideEffectStatus::Applied,
            }) => true,
            Ok(RelayRemoteUserSideEffectReply {
                status: RelayRemoteUserSideEffectStatus::StaleRegistration,
            }) => {
                self.detach_stale_remote_socket_resource(source_jid, &registration)
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

    async fn cancel_remote_resource_admission(
        &self,
        services: &OrderedRelayDeliveryServices,
        jid: &jid::FullJid,
        registration: &RemoteSocketRegistration,
    ) -> bool {
        self.cancel_exact_remote_resource_admission(
            services,
            jid,
            registration.registration_id,
            registration.admission_epoch,
            &registration.socket_node,
        )
        .await
    }

    async fn cancel_exact_remote_resource_admission(
        &self,
        services: &OrderedRelayDeliveryServices,
        jid: &jid::FullJid,
        registration_id: RemoteResourceRegistrationId,
        admission_epoch: RemoteResourceAdmissionEpoch,
        socket_node: &NodeIdentity,
    ) -> bool {
        match services
            .remote_resource_admission_store
            .cancel(jid, registration_id, admission_epoch, socket_node)
            .await
        {
            Ok(_) => true,
            Err(error) => {
                tracing::warn!(
                    jid = %jid,
                    registration_id = ?registration_id,
                    %error,
                    "exact remote-resource admission cancellation failed"
                );
                false
            }
        }
    }

    async fn cancel_remote_owner_admission(
        &self,
        services: &OrderedRelayDeliveryServices,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
    ) -> bool {
        match services
            .remote_resource_admission_store
            .cancel(
                jid,
                registration.registration_id,
                registration.admission_epoch,
                &registration.socket_node,
            )
            .await
        {
            Ok(_) => true,
            Err(error) => {
                tracing::warn!(
                    jid = %jid,
                    registration_id = ?registration.registration_id,
                    %error,
                    "exact remote-owner admission cancellation failed"
                );
                false
            }
        }
    }

    async fn remove_remote_socket_registration_if_current(
        &self,
        jid: &jid::FullJid,
        registration: &RemoteSocketRegistration,
    ) {
        let mut registrations = self.remote_socket_resources.lock().await;
        if registrations
            .get(jid)
            .is_some_and(|current| remote_socket_registration_matches(current, registration))
        {
            registrations.remove(jid);
        }
    }

    async fn remote_socket_registration_is_current(
        &self,
        jid: &jid::FullJid,
        registration: &RemoteSocketRegistration,
    ) -> bool {
        self.remote_socket_resources
            .lock()
            .await
            .get(jid)
            .is_some_and(|current| remote_socket_registration_matches(current, registration))
    }

    async fn retain_remote_socket_cleanup(
        &self,
        jid: &jid::FullJid,
        registration: &RemoteSocketRegistration,
    ) {
        self.remote_socket_pending_cleanup.lock().await.insert(
            registration.registration_id,
            (jid.clone(), registration.clone()),
        );
    }

    async fn reserve_remote_socket_cleanup_capacity(
        &self,
        jid: &jid::FullJid,
        registration: &RemoteSocketRegistration,
    ) -> bool {
        let mut pending = self.remote_socket_pending_cleanup.lock().await;
        if pending
            .get(&registration.registration_id)
            .is_some_and(|(current_jid, current)| {
                current_jid == jid && remote_socket_registration_matches(current, registration)
            })
        {
            return true;
        }
        let same_jid = pending
            .values()
            .filter(|(pending_jid, _)| pending_jid == jid)
            .count();
        if same_jid >= MAX_PENDING_REMOTE_SOCKET_CLEANUPS_PER_JID {
            tracing::error!(
                jid = %jid,
                limit = MAX_PENDING_REMOTE_SOCKET_CLEANUPS_PER_JID,
                "clustered remote-resource per-JID cleanup capacity exhausted; refusing another registration"
            );
            return false;
        }
        if pending.len() >= MAX_REMOTE_SOCKET_CLEANUP_REGISTRATIONS {
            tracing::error!(
                jid = %jid,
                limit = MAX_REMOTE_SOCKET_CLEANUP_REGISTRATIONS,
                "clustered remote-resource global cleanup capacity exhausted; refusing another registration"
            );
            return false;
        }
        pending.insert(
            registration.registration_id,
            (jid.clone(), registration.clone()),
        );
        true
    }

    async fn remove_pending_remote_socket_cleanup_if_current(
        &self,
        jid: &jid::FullJid,
        registration: &RemoteSocketRegistration,
    ) {
        let mut pending = self.remote_socket_pending_cleanup.lock().await;
        if pending
            .get(&registration.registration_id)
            .is_some_and(|(current_jid, current)| {
                current_jid == jid && remote_socket_registration_matches(current, registration)
            })
        {
            pending.remove(&registration.registration_id);
        }
    }

    async fn compensate_remote_socket_registration(
        &self,
        handle: &mut RelayHandle,
        jid: &jid::FullJid,
        registration: &RemoteSocketRegistration,
    ) -> bool {
        self.remove_remote_socket_registration_if_current(jid, registration)
            .await;
        let result = handle
            .unregister_remote_user_resource(RelayUnregisterRemoteUserResource {
                jid: jid.clone(),
                registration_id: registration.registration_id,
                admission_epoch: registration.admission_epoch,
                socket_generation: registration.socket_generation,
                socket_node: registration.socket_node.clone(),
                expected_user_owner: registration.user_owner.clone(),
                expected_user_claim_epoch: registration.user_claim_epoch,
            })
            .await;
        let terminal = matches!(
            result,
            Ok(RelayRemoteResourceUnregisterReply {
                status: RelayRemoteResourceUnregisterStatus::Terminal,
            })
        );
        self.finish_remote_socket_cleanup_attempt(jid, registration, terminal)
            .await
    }

    async fn finish_remote_socket_cleanup_attempt(
        &self,
        jid: &jid::FullJid,
        registration: &RemoteSocketRegistration,
        confirmed: bool,
    ) -> bool {
        if confirmed {
            self.remove_remote_socket_registration_if_current(jid, registration)
                .await;
            self.remove_pending_remote_socket_cleanup_if_current(jid, registration)
                .await;
            true
        } else {
            self.retain_remote_socket_cleanup(jid, registration).await;
            false
        }
    }

    #[cfg(test)]
    async fn pending_remote_socket_cleanups_for_jid(
        &self,
        jid: &jid::FullJid,
    ) -> Vec<RemoteSocketRegistration> {
        self.remote_socket_pending_cleanup
            .lock()
            .await
            .values()
            .filter(|(pending_jid, _)| pending_jid == jid)
            .map(|(_, registration)| registration.clone())
            .collect()
    }

    async fn remote_socket_cleanup_global_capacity_exhausted(&self) -> bool {
        self.remote_socket_pending_cleanup.lock().await.len()
            >= MAX_REMOTE_SOCKET_CLEANUP_REGISTRATIONS
    }

    async fn inactive_remote_socket_cleanups(
        &self,
        limit: usize,
    ) -> Vec<(jid::FullJid, RemoteSocketRegistration)> {
        let Some(services) = self.services.get() else {
            return Vec::new();
        };
        let active = self.remote_socket_resources.lock().await;
        self.remote_socket_pending_cleanup
            .lock()
            .await
            .values()
            .filter(|(jid, registration)| {
                let exact_registration_is_active = active.get(jid).is_some_and(|current| {
                    remote_socket_registration_matches(current, registration)
                });
                let exact_connection_is_live = services
                    .connection_registry
                    .entry_if_owner(jid, &registration.owner)
                    .is_some();
                !exact_registration_is_active || !exact_connection_is_live
            })
            .take(limit)
            .cloned()
            .collect()
    }

    async fn retry_inactive_remote_socket_cleanups(&self, limit: usize) {
        let pending = self.inactive_remote_socket_cleanups(limit).await;
        self.retry_remote_socket_cleanups(pending).await;
    }

    async fn retry_remote_socket_cleanups(
        &self,
        pending: Vec<(jid::FullJid, RemoteSocketRegistration)>,
    ) {
        let attempts = pending.into_iter().map(|(jid, registration)| async move {
            let admission_cancelled = if let Some(services) = self.services.get() {
                self.cancel_remote_resource_admission(services, &jid, &registration)
                    .await
            } else {
                false
            };
            if !admission_cancelled {
                return (jid, registration, false);
            }
            self.remove_remote_socket_registration_if_current(&jid, &registration)
                .await;
            let mut handle = RelayHandle::new(
                NodeId::new(registration.user_owner.node_id.clone()),
                self.stop_token.clone(),
            )
            .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
            let terminal = matches!(
                handle
                    .unregister_remote_user_resource(RelayUnregisterRemoteUserResource {
                        jid: jid.clone(),
                        registration_id: registration.registration_id,
                        admission_epoch: registration.admission_epoch,
                        socket_generation: registration.socket_generation,
                        socket_node: registration.socket_node.clone(),
                        expected_user_owner: registration.user_owner.clone(),
                        expected_user_claim_epoch: registration.user_claim_epoch,
                    })
                    .await,
                Ok(RelayRemoteResourceUnregisterReply {
                    status: RelayRemoteResourceUnregisterStatus::Terminal,
                })
            );
            (jid, registration, terminal)
        });
        for (jid, registration, terminal) in futures::future::join_all(attempts).await {
            self.finish_remote_socket_cleanup_attempt(&jid, &registration, terminal)
                .await;
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

    async fn retain_remote_owner_cleanup(
        &self,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
    ) {
        let retained = {
            let mut pending = self.remote_owner_pending_cleanup.lock().await;
            Self::insert_remote_owner_cleanup_reservation(&mut pending, jid, registration)
        };
        if !retained {
            tracing::error!(
                jid = %jid,
                registration_id = ?registration.registration_id,
                "remote-owner cleanup evidence was not pre-reserved; keeping the active registration"
            );
            return;
        }
        self.remove_remote_owner_registration_if_current(jid, registration)
            .await;
    }

    fn insert_remote_owner_cleanup_reservation(
        pending: &mut HashMap<
            RemoteResourceRegistrationId,
            (jid::FullJid, RemoteOwnerRegistration),
        >,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
    ) -> bool {
        if let Some((current_jid, current)) = pending.get(&registration.registration_id) {
            if current_jid == jid && remote_owner_registration_matches(current, registration) {
                return true;
            }
            tracing::error!(
                jid = %jid,
                registration_id = ?registration.registration_id,
                "remote-owner cleanup registration ID collision; preserving existing exact evidence"
            );
            return false;
        }

        let same_jid = pending
            .values()
            .filter(|(pending_jid, _)| pending_jid == jid)
            .count();
        if same_jid >= MAX_PENDING_REMOTE_OWNER_CLEANUPS_PER_JID {
            tracing::error!(
                jid = %jid,
                limit = MAX_PENDING_REMOTE_OWNER_CLEANUPS_PER_JID,
                "clustered remote-resource owner per-JID cleanup capacity exhausted"
            );
            return false;
        }
        if pending.len() >= MAX_REMOTE_OWNER_CLEANUP_REGISTRATIONS {
            tracing::error!(
                jid = %jid,
                limit = MAX_REMOTE_OWNER_CLEANUP_REGISTRATIONS,
                "clustered remote-resource owner global cleanup capacity exhausted"
            );
            return false;
        }

        pending.insert(
            registration.registration_id,
            (jid.clone(), registration.clone()),
        );
        true
    }

    async fn reserve_remote_owner_cleanup_capacity(
        &self,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
    ) -> Option<RemoteOwnerOperationGuard> {
        let Some(operation) = RemoteOwnerOperationGuard::begin(
            Arc::clone(&self.remote_owner_cleanup_inflight),
            registration.registration_id,
        ) else {
            tracing::warn!(
                jid = %jid,
                registration_id = ?registration.registration_id,
                "remote-owner registration already has an operation in flight"
            );
            return None;
        };
        let reserved = {
            let mut pending = self.remote_owner_pending_cleanup.lock().await;
            Self::insert_remote_owner_cleanup_reservation(&mut pending, jid, registration)
        };
        if reserved {
            Some(operation)
        } else {
            None
        }
    }

    async fn remote_owner_cleanup_global_capacity_exhausted(&self) -> bool {
        self.remote_owner_pending_cleanup.lock().await.len()
            >= MAX_REMOTE_OWNER_CLEANUP_REGISTRATIONS
    }

    async fn inactive_remote_owner_cleanup_ids(
        &self,
        limit: usize,
    ) -> Vec<(jid::FullJid, RemoteResourceRegistrationId)> {
        let active = self.remote_owner_resources.lock().await;
        let inflight = self
            .remote_owner_cleanup_inflight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        self.remote_owner_pending_cleanup
            .lock()
            .await
            .iter()
            .filter(|(registration_id, (jid, registration))| {
                !inflight.contains(registration_id)
                    && !active.get(jid).is_some_and(|current| {
                        remote_owner_registration_matches(current, registration)
                    })
            })
            .take(limit)
            .map(|(registration_id, (jid, _))| (jid.clone(), *registration_id))
            .collect()
    }

    async fn retry_inactive_remote_owner_cleanups(&self, limit: usize) {
        for (jid, registration_id) in self.inactive_remote_owner_cleanup_ids(limit).await {
            let _ = self
                .cleanup_remote_owner_resource_if_registration(&jid, registration_id)
                .await;
        }
    }

    async fn remove_pending_remote_owner_cleanup_if_current(
        &self,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
    ) {
        let mut pending = self.remote_owner_pending_cleanup.lock().await;
        if pending
            .get(&registration.registration_id)
            .is_some_and(|(current_jid, current)| {
                current_jid == jid && remote_owner_registration_matches(current, registration)
            })
        {
            pending.remove(&registration.registration_id);
        }
    }

    async fn cleanup_pending_remote_owner_resources_for_jid(&self, jid: &jid::FullJid) -> bool {
        let active = self.remote_owner_resources.lock().await;
        let inflight = self
            .remote_owner_cleanup_inflight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let pending_ids: Vec<_> = self
            .remote_owner_pending_cleanup
            .lock()
            .await
            .iter()
            .filter(|(registration_id, (pending_jid, registration))| {
                pending_jid == jid
                    && !inflight.contains(registration_id)
                    && !active.get(jid).is_some_and(|current| {
                        remote_owner_registration_matches(current, registration)
                    })
            })
            .map(|(registration_id, _)| *registration_id)
            .collect();
        drop(inflight);
        drop(active);
        for registration_id in pending_ids {
            if !self
                .cleanup_remote_owner_resource_if_registration(jid, registration_id)
                .await
            {
                return false;
            }
        }
        true
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
        let fence_generation = self.remote_resource_fence_generation.load(Ordering::SeqCst);
        if !self.remote_resource_registration_allowed(&services, fence_generation) {
            return RelayRemoteResourceRegistrationReply {
                status: RelayRemoteResourceRegistrationStatus::Unavailable,
            };
        }
        if !self
            .cleanup_pending_remote_owner_resources_for_jid(&msg.jid)
            .await
        {
            return RelayRemoteResourceRegistrationReply {
                status: RelayRemoteResourceRegistrationStatus::Unavailable,
            };
        }
        match exact_remote_socket_node_is_current(&services, &msg.socket_node).await {
            Ok(()) => {}
            Err(RemoteUserClaimValidationError::Stale) => {
                return RelayRemoteResourceRegistrationReply {
                    status: RelayRemoteResourceRegistrationStatus::StaleRegistration,
                };
            }
            Err(RemoteUserClaimValidationError::Unavailable) => {
                return RelayRemoteResourceRegistrationReply {
                    status: RelayRemoteResourceRegistrationStatus::Unavailable,
                };
            }
        }
        match exact_remote_resource_admission_is_current(
            &services,
            &msg.jid,
            msg.registration_id,
            msg.admission_epoch,
            &msg.socket_node,
        )
        .await
        {
            Ok(()) => {}
            Err(RemoteUserClaimValidationError::Stale) => {
                return RelayRemoteResourceRegistrationReply {
                    status: RelayRemoteResourceRegistrationStatus::StaleRegistration,
                };
            }
            Err(RemoteUserClaimValidationError::Unavailable) => {
                return RelayRemoteResourceRegistrationReply {
                    status: RelayRemoteResourceRegistrationStatus::Unavailable,
                };
            }
        }
        let target_entity = user_entity(&msg.jid.to_bare());
        let Some(snapshot) = current_claim(&services, &target_entity).await else {
            return RelayRemoteResourceRegistrationReply {
                status: RelayRemoteResourceRegistrationStatus::NotOwner,
            };
        };
        let me = services.node_identity.current();
        if !snapshot.owner_lease_fresh
            || snapshot.owner != msg.expected_user_owner
            || snapshot.claim_epoch != msg.expected_user_claim_epoch
            || me != msg.expected_user_owner
        {
            return RelayRemoteResourceRegistrationReply {
                status: RelayRemoteResourceRegistrationStatus::NotOwner,
            };
        }
        if !self.remote_resource_registration_allowed(&services, fence_generation) {
            return RelayRemoteResourceRegistrationReply {
                status: RelayRemoteResourceRegistrationStatus::Unavailable,
            };
        }

        let displaced = {
            let registrations = self.remote_owner_resources.lock().await;
            registrations.get(&msg.jid).cloned()
        };
        if let Some(displaced) = displaced {
            if displaced.registration_id == msg.registration_id
                && displaced.admission_epoch == msg.admission_epoch
                && displaced.socket_node == msg.socket_node
                && displaced.socket_generation == msg.socket_generation
                && displaced.user_owner == msg.expected_user_owner
                && displaced.user_claim_epoch == msg.expected_user_claim_epoch
            {
                match remote_owner_registration_is_current(&services, &msg.jid, &displaced).await {
                    Ok(()) => {
                        let status = if self
                            .remote_resource_registration_allowed(&services, fence_generation)
                        {
                            RelayRemoteResourceRegistrationStatus::Registered
                        } else {
                            RelayRemoteResourceRegistrationStatus::Unavailable
                        };
                        return RelayRemoteResourceRegistrationReply { status };
                    }
                    Err(RelayRemoteResourceRegistrationStatus::StaleRegistration) => {
                        self.cleanup_remote_owner_resource_if_registration(
                            &msg.jid,
                            displaced.registration_id,
                        )
                        .await;
                    }
                    Err(status) => return RelayRemoteResourceRegistrationReply { status },
                }
            } else {
                // The incoming durable admission was proven current above, so
                // every different mirror is superseded even when its own
                // admission has already become stale. Retire the exact old
                // physical socket before publishing the new mirror; merely
                // forgetting the stale mirror would leave that socket alive.
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

        if !self
            .cleanup_pending_remote_owner_resources_for_jid(&msg.jid)
            .await
        {
            return RelayRemoteResourceRegistrationReply {
                status: RelayRemoteResourceRegistrationStatus::Unavailable,
            };
        }

        if !self.remote_resource_registration_allowed(&services, fence_generation) {
            return RelayRemoteResourceRegistrationReply {
                status: RelayRemoteResourceRegistrationStatus::Unavailable,
            };
        }

        let (tx, rx) = mpsc::channel(REMOTE_RESOURCE_OUTBOUND_CHANNEL_SIZE);
        let entry = ConnectionEntry::new_remote_mirror(tx);
        apply_remote_resource_state(&entry, &msg.state);
        let owner = entry.carbons_handle();
        let force_detach_rx = entry.take_force_detach_rx();
        let registration = RemoteOwnerRegistration {
            registration_id: msg.registration_id,
            admission_epoch: msg.admission_epoch,
            socket_node: msg.socket_node.clone(),
            socket_generation: msg.socket_generation,
            user_owner: msg.expected_user_owner.clone(),
            user_claim_epoch: msg.expected_user_claim_epoch,
            owner: owner.clone(),
            placement: ConnectionPlacement::RemoteMirror,
        };
        let _registration_operation = if let Some(operation) = self
            .reserve_remote_owner_cleanup_capacity(&msg.jid, &registration)
            .await
        {
            operation
        } else {
            if self.remote_owner_cleanup_global_capacity_exhausted().await {
                self.retry_inactive_remote_owner_cleanups(REMOTE_OWNER_GLOBAL_CLEANUP_RETRY_BATCH)
                    .await;
            }
            let Some(operation) = self
                .reserve_remote_owner_cleanup_capacity(&msg.jid, &registration)
                .await
            else {
                return RelayRemoteResourceRegistrationReply {
                    status: RelayRemoteResourceRegistrationStatus::Unavailable,
                };
            };
            operation
        };
        if let Err(error) =
            exact_remote_owner_authority_is_current(&services, &msg.jid, &registration).await
        {
            self.remove_pending_remote_owner_cleanup_if_current(&msg.jid, &registration)
                .await;
            return RelayRemoteResourceRegistrationReply {
                status: remote_registration_status_for_authority_error(error),
            };
        }
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
                if let Err(error) =
                    exact_remote_owner_authority_is_current(&services, &msg.jid, &registration)
                        .await
                {
                    if !unregister_remote_owner_actor_entry(&services, &msg.jid, &owner).await {
                        self.retain_remote_owner_cleanup(&msg.jid, &registration)
                            .await;
                        return RelayRemoteResourceRegistrationReply {
                            status: RelayRemoteResourceRegistrationStatus::Unavailable,
                        };
                    }
                    self.remove_pending_remote_owner_cleanup_if_current(&msg.jid, &registration)
                        .await;
                    return RelayRemoteResourceRegistrationReply {
                        status: remote_registration_status_for_authority_error(error),
                    };
                }
                if !services
                    .connection_registry
                    .register_entry_if_owner_or_absent(msg.jid.clone(), entry.clone(), &owner)
                {
                    if !unregister_remote_owner_actor_entry(&services, &msg.jid, &owner).await {
                        self.retain_remote_owner_cleanup(&msg.jid, &registration)
                            .await;
                        return RelayRemoteResourceRegistrationReply {
                            status: RelayRemoteResourceRegistrationStatus::Unavailable,
                        };
                    }
                    self.remove_pending_remote_owner_cleanup_if_current(&msg.jid, &registration)
                        .await;
                    return RelayRemoteResourceRegistrationReply {
                        status: RelayRemoteResourceRegistrationStatus::StaleRegistration,
                    };
                }
                match remote_owner_registration_is_current(&services, &msg.jid, &registration).await
                {
                    Ok(()) => {}
                    Err(status) => {
                        if !unregister_remote_owner_actor_entry(&services, &msg.jid, &owner).await {
                            self.retain_remote_owner_cleanup(&msg.jid, &registration)
                                .await;
                            return RelayRemoteResourceRegistrationReply {
                                status: RelayRemoteResourceRegistrationStatus::Unavailable,
                            };
                        }
                        services
                            .connection_registry
                            .unregister_if_owner(&msg.jid, &owner);
                        self.remove_pending_remote_owner_cleanup_if_current(
                            &msg.jid,
                            &registration,
                        )
                        .await;
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
                    .insert(msg.jid.clone(), registration.clone());
                // The pending exact entry is the lifetime reservation for this
                // active mirror as well as its compensation evidence. Cleanup
                // selection skips the matching active entry.
                let post_publish_failure =
                    if !self.remote_resource_registration_allowed(&services, fence_generation) {
                        Some(RelayRemoteResourceRegistrationStatus::Unavailable)
                    } else {
                        remote_owner_registration_is_current(&services, &msg.jid, &registration)
                            .await
                            .err()
                    };
                if let Some(status) = post_publish_failure {
                    self.retain_remote_owner_cleanup(&msg.jid, &registration)
                        .await;
                    if !unregister_remote_owner_actor_entry(&services, &msg.jid, &owner).await {
                        return RelayRemoteResourceRegistrationReply {
                            status: RelayRemoteResourceRegistrationStatus::Unavailable,
                        };
                    }
                    services
                        .connection_registry
                        .unregister_if_owner(&msg.jid, &owner);
                    self.remove_pending_remote_owner_cleanup_if_current(&msg.jid, &registration)
                        .await;
                    return RelayRemoteResourceRegistrationReply { status };
                }
                self.spawn_remote_resource_forwarder(msg.jid, registration, rx, force_detach_rx);
                RelayRemoteResourceRegistrationReply {
                    status: RelayRemoteResourceRegistrationStatus::Registered,
                }
            }
            Ok(false) => {
                self.remove_pending_remote_owner_cleanup_if_current(&msg.jid, &registration)
                    .await;
                RelayRemoteResourceRegistrationReply {
                    status: RelayRemoteResourceRegistrationStatus::StaleRegistration,
                }
            }
            Err(error) => {
                self.retain_remote_owner_cleanup(&msg.jid, &registration)
                    .await;
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
        let jid = msg.jid.clone();
        let Some(lock) = self.lock_for_remote_owner_registration(&jid).await else {
            return RelayRemoteResourceUnregisterReply {
                status: RelayRemoteResourceUnregisterStatus::Retry,
            };
        };
        let guard = lock.lock().await;
        let reply = self
            .unregister_remote_user_resource_on_owner_locked(msg)
            .await;
        drop(guard);
        self.remove_remote_owner_registration_lock_if_unused(&jid, &lock)
            .await;
        reply
    }

    async fn unregister_remote_user_resource_on_owner_locked(
        &self,
        msg: RelayUnregisterRemoteUserResource,
    ) -> RelayRemoteResourceUnregisterReply {
        let Some(services) = self.services.get().cloned() else {
            return RelayRemoteResourceUnregisterReply {
                status: RelayRemoteResourceUnregisterStatus::Retry,
            };
        };
        if let Err(error) = services
            .remote_resource_admission_store
            .cancel(
                &msg.jid,
                msg.registration_id,
                msg.admission_epoch,
                &msg.socket_node,
            )
            .await
        {
            tracing::warn!(
                jid = %msg.jid,
                registration_id = ?msg.registration_id,
                %error,
                "owner could not revoke exact remote-resource admission before unregister"
            );
            return RelayRemoteResourceUnregisterReply {
                status: RelayRemoteResourceUnregisterStatus::Retry,
            };
        }
        let active_registration = {
            let registrations = self.remote_owner_resources.lock().await;
            registrations
                .get(&msg.jid)
                .filter(|registration| {
                    registration.registration_id == msg.registration_id
                        && registration.admission_epoch == msg.admission_epoch
                        && registration.socket_generation == msg.socket_generation
                        && registration.socket_node == msg.socket_node
                        && registration.user_owner == msg.expected_user_owner
                        && registration.user_claim_epoch == msg.expected_user_claim_epoch
                })
                .cloned()
        };
        let registration = if let Some(registration) = active_registration {
            Some(registration)
        } else {
            self.remote_owner_pending_cleanup
                .lock()
                .await
                .get(&msg.registration_id)
                .filter(|(pending_jid, registration)| {
                    pending_jid == &msg.jid
                        && registration.admission_epoch == msg.admission_epoch
                        && registration.socket_generation == msg.socket_generation
                        && registration.socket_node == msg.socket_node
                        && registration.user_owner == msg.expected_user_owner
                        && registration.user_claim_epoch == msg.expected_user_claim_epoch
                })
                .map(|(_, registration)| registration.clone())
        };
        let Some(registration) = registration else {
            return RelayRemoteResourceUnregisterReply {
                status: RelayRemoteResourceUnregisterStatus::Terminal,
            };
        };
        match remote_owner_registration_is_current(&services, &msg.jid, &registration).await {
            Ok(()) => {}
            Err(RelayRemoteResourceRegistrationStatus::StaleRegistration) => {
                let terminal = self
                    .cleanup_remote_owner_resource_if_registration(
                        &msg.jid,
                        registration.registration_id,
                    )
                    .await;
                return RelayRemoteResourceUnregisterReply {
                    status: if terminal {
                        RelayRemoteResourceUnregisterStatus::Terminal
                    } else {
                        RelayRemoteResourceUnregisterStatus::Retry
                    },
                };
            }
            Err(_) => {
                return RelayRemoteResourceUnregisterReply {
                    status: RelayRemoteResourceUnregisterStatus::Retry,
                };
            }
        }
        match exact_remote_owner_authority_is_current(&services, &msg.jid, &registration).await {
            Ok(()) => {}
            Err(RemoteUserClaimValidationError::Stale) => {
                let terminal = self
                    .cleanup_remote_owner_resource_if_registration(
                        &msg.jid,
                        registration.registration_id,
                    )
                    .await;
                return RelayRemoteResourceUnregisterReply {
                    status: if terminal {
                        RelayRemoteResourceUnregisterStatus::Terminal
                    } else {
                        RelayRemoteResourceUnregisterStatus::Retry
                    },
                };
            }
            Err(RemoteUserClaimValidationError::Unavailable) => {
                return RelayRemoteResourceUnregisterReply {
                    status: RelayRemoteResourceUnregisterStatus::Retry,
                };
            }
        }
        let terminal = self
            .cleanup_remote_owner_resource_if_registration(&msg.jid, registration.registration_id)
            .await;
        RelayRemoteResourceUnregisterReply {
            status: if terminal {
                RelayRemoteResourceUnregisterStatus::Terminal
            } else {
                RelayRemoteResourceUnregisterStatus::Retry
            },
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
        let registration = {
            let registrations = self.remote_owner_resources.lock().await;
            registrations
                .get(&msg.jid)
                .filter(|registration| {
                    registration.registration_id == msg.registration_id
                        && registration.admission_epoch == msg.admission_epoch
                        && registration.socket_generation == msg.socket_generation
                        && registration.socket_node == msg.socket_node
                        && registration.user_owner == msg.expected_user_owner
                        && registration.user_claim_epoch == msg.expected_user_claim_epoch
                })
                .cloned()
        };
        let Some(registration) = registration else {
            return RelayRemoteResourceUpdateReply {
                status: RelayRemoteResourceUpdateStatus::StaleRegistration,
            };
        };
        match remote_owner_registration_is_current(&services, &msg.jid, &registration).await {
            Ok(()) => {}
            Err(RelayRemoteResourceRegistrationStatus::StaleRegistration) => {
                self.cleanup_remote_owner_resource_if_registration(
                    &msg.jid,
                    registration.registration_id,
                )
                .await;
                return RelayRemoteResourceUpdateReply {
                    status: RelayRemoteResourceUpdateStatus::StaleRegistration,
                };
            }
            Err(_) => {
                return RelayRemoteResourceUpdateReply {
                    status: RelayRemoteResourceUpdateStatus::Unavailable,
                };
            }
        }
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
        let entry = match owner_remote_entry_if_current(
            &actor,
            &services.connection_registry,
            &jid,
            &registration.owner,
        )
        .await
        {
            Ok(entry) => entry,
            Err(status) => return RelayRemoteResourceUpdateReply { status },
        };
        if let Err(error) =
            exact_remote_owner_authority_is_current(&services, &jid, &registration).await
        {
            return RelayRemoteResourceUpdateReply {
                status: match error {
                    RemoteUserClaimValidationError::Stale => {
                        RelayRemoteResourceUpdateStatus::StaleRegistration
                    }
                    RemoteUserClaimValidationError::Unavailable => {
                        RelayRemoteResourceUpdateStatus::Unavailable
                    }
                },
            };
        }
        let status = match msg.update {
            RemoteResourceStateUpdate::Presence {
                available,
                priority,
                state,
            } => {
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
            RemoteResourceStateUpdate::Carbons { enabled } => {
                entry.carbons_enabled.store(enabled, Ordering::Relaxed);
                RelayRemoteResourceUpdateStatus::Updated
            }
            RemoteResourceStateUpdate::RosterInterested => {
                entry.roster_interested.store(true, Ordering::Relaxed);
                RelayRemoteResourceUpdateStatus::Updated
            }
            RemoteResourceStateUpdate::BlocklistInterested => {
                entry.blocklist_interested.store(true, Ordering::Relaxed);
                RelayRemoteResourceUpdateStatus::Updated
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
        let registration = {
            let registrations = self.remote_owner_resources.lock().await;
            registrations
                .get(&msg.source_jid)
                .filter(|registration| {
                    registration.registration_id == msg.registration_id
                        && registration.admission_epoch == msg.admission_epoch
                        && registration.socket_generation == msg.socket_generation
                        && registration.socket_node == msg.socket_node
                        && registration.user_owner == msg.expected_user_owner
                        && registration.user_claim_epoch == msg.expected_user_claim_epoch
                })
                .cloned()
        };
        let Some(registration) = registration else {
            return RelayRemoteUserSideEffectReply {
                status: RelayRemoteUserSideEffectStatus::StaleRegistration,
            };
        };
        match remote_owner_registration_is_current(&services, &msg.source_jid, &registration).await
        {
            Ok(()) => {}
            Err(RelayRemoteResourceRegistrationStatus::StaleRegistration) => {
                self.cleanup_remote_owner_resource_if_registration(
                    &msg.source_jid,
                    registration.registration_id,
                )
                .await;
                return RelayRemoteUserSideEffectReply {
                    status: RelayRemoteUserSideEffectStatus::StaleRegistration,
                };
            }
            Err(_) => {
                return RelayRemoteUserSideEffectReply {
                    status: RelayRemoteUserSideEffectStatus::Unavailable,
                };
            }
        }
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
        if let Err(error) =
            exact_remote_owner_authority_is_current(&services, &msg.source_jid, &registration).await
        {
            return RelayRemoteUserSideEffectReply {
                status: match error {
                    RemoteUserClaimValidationError::Stale => {
                        RelayRemoteUserSideEffectStatus::StaleRegistration
                    }
                    RemoteUserClaimValidationError::Unavailable => {
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
        if let Some(handoff) = origin.handoff.clone() {
            if handoff.mark_deferred() {
                let bridge = Arc::clone(&self);
                let origin_stanza = origin_stanza.clone();
                tokio::spawn(async move {
                    let outcome = bridge
                        .route_remote_resource_origin_once(remote_origin, target)
                        .await
                        .unwrap_or(FullJidDeliveryOutcome::Dropped);
                    handoff.complete(replies_for_origin_handoff(&origin_stanza, outcome));
                });
                return Some(FullJidDeliveryOutcome::Delivered);
            }
        }
        self.route_remote_resource_origin_once(remote_origin, target)
            .await
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
        let mut handle = RelayHandle::new(
            NodeId::new(remote_origin.user_owner.node_id.clone()),
            self.stop_token.clone(),
        )
        .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        handle
            .route_remote_resource_stanza(RelayRouteRemoteResourceStanza {
                source_jid: remote_origin.jid.clone(),
                registration_id: remote_origin.registration_id,
                admission_epoch: remote_origin.admission_epoch,
                socket_generation: remote_origin.socket_generation,
                socket_node: remote_origin.socket_node.clone(),
                expected_user_owner: remote_origin.user_owner.clone(),
                expected_user_claim_epoch: remote_origin.user_claim_epoch,
                target,
            })
            .await
    }

    async fn refresh_remote_resource_origin(
        self: &Arc<Self>,
        remote_origin: &RemoteResourceOriginSnapshot,
    ) -> RemoteResourceOriginRefresh {
        let Some(services) = self.services.get().cloned() else {
            return RemoteResourceOriginRefresh::Failed;
        };
        let registration = {
            let registrations = self.remote_socket_resources.lock().await;
            let Some(registration) = registrations
                .get(&remote_origin.jid)
                .filter(|registration| {
                    registration.registration_id == remote_origin.registration_id
                        && registration.admission_epoch == remote_origin.admission_epoch
                        && registration.socket_generation == remote_origin.socket_generation
                        && registration.socket_node == remote_origin.socket_node
                        && registration.user_owner == remote_origin.user_owner
                        && registration.user_claim_epoch == remote_origin.user_claim_epoch
                })
                .cloned()
            else {
                return RemoteResourceOriginRefresh::Failed;
            };
            registration
        };
        let Some(entry) = services
            .connection_registry
            .entry_if_owner(&remote_origin.jid, &registration.owner)
        else {
            return RemoteResourceOriginRefresh::Failed;
        };
        let target_entity = user_entity(&remote_origin.jid.to_bare());
        let Some(snapshot) = current_claim(&services, &target_entity).await else {
            return RemoteResourceOriginRefresh::Failed;
        };
        let _ = entry;
        if !snapshot.owner_lease_fresh {
            return RemoteResourceOriginRefresh::Failed;
        }
        if snapshot.owner == registration.user_owner
            && snapshot.claim_epoch == registration.user_claim_epoch
        {
            return RemoteResourceOriginRefresh::Remote(remote_origin.clone());
        }
        // The physical admission remains exact, but its authoritative actor
        // claim changed. Reconnecting mints a fresh registration carrying the
        // new claim epoch; allowing this connection to rewrite that lineage in
        // place would let already-queued old-owner operations touch the new
        // actor incarnation.
        self.detach_remote_socket_resource(
            &remote_origin.jid,
            &registration,
            ForceDetachReason::OwnershipLost,
        )
        .await;
        RemoteResourceOriginRefresh::Failed
    }

    pub(crate) async fn route_remote_resource_stanza_on_owner(
        self: &Arc<Self>,
        msg: RelayRouteRemoteResourceStanza,
    ) -> RelayRouteRemoteResourceStanzaReply {
        let Some(services) = self.services.get().cloned() else {
            return remote_resource_route_reply(RemoteResourceRouteOutcome::Dropped);
        };
        let registration = {
            let registrations = self.remote_owner_resources.lock().await;
            registrations
                .get(&msg.source_jid)
                .filter(|registration| {
                    registration.registration_id == msg.registration_id
                        && registration.admission_epoch == msg.admission_epoch
                        && registration.socket_generation == msg.socket_generation
                        && registration.socket_node == msg.socket_node
                        && registration.user_owner == msg.expected_user_owner
                        && registration.user_claim_epoch == msg.expected_user_claim_epoch
                })
                .cloned()
        };
        let Some(registration) = registration else {
            return remote_resource_route_reply(RemoteResourceRouteOutcome::StaleRegistration);
        };
        match remote_owner_registration_is_current(&services, &msg.source_jid, &registration).await
        {
            Ok(()) => {}
            Err(RelayRemoteResourceRegistrationStatus::StaleRegistration) => {
                self.cleanup_remote_owner_resource_if_registration(
                    &msg.source_jid,
                    registration.registration_id,
                )
                .await;
                return remote_resource_route_reply(RemoteResourceRouteOutcome::StaleRegistration);
            }
            Err(_) => return remote_resource_route_reply(RemoteResourceRouteOutcome::Dropped),
        }
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
        if let Err(error) =
            exact_remote_owner_authority_is_current(&services, &msg.source_jid, &registration).await
        {
            return remote_resource_route_reply(match error {
                RemoteUserClaimValidationError::Stale => {
                    RemoteResourceRouteOutcome::StaleRegistration
                }
                RemoteUserClaimValidationError::Unavailable => RemoteResourceRouteOutcome::Dropped,
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
        let fence_generation = self.remote_resource_fence_generation.load(Ordering::SeqCst);
        if !self.remote_resource_registration_allowed(&services, fence_generation) {
            return RelayRemoteResourceFrameReply {
                status: RelayRemoteResourceFrameStatus::StaleRegistration,
            };
        }
        let registration = {
            let registrations = self.remote_socket_resources.lock().await;
            registrations
                .get(&msg.frame.jid)
                .filter(|registration| {
                    registration.registration_id == msg.frame.registration_id
                        && registration.admission_epoch == msg.frame.admission_epoch
                        && registration.socket_generation == msg.frame.socket_generation
                        && registration.socket_node == msg.frame.socket_node
                        && registration.user_owner == msg.frame.expected_user_owner
                        && registration.user_claim_epoch == msg.frame.expected_user_claim_epoch
                })
                .cloned()
        };
        let Some(registration) = registration else {
            return RelayRemoteResourceFrameReply {
                status: RelayRemoteResourceFrameStatus::StaleRegistration,
            };
        };
        if services.node_identity.current() != registration.socket_node {
            self.detach_stale_remote_socket_resource(&msg.frame.jid, &registration)
                .await;
            return RelayRemoteResourceFrameReply {
                status: RelayRemoteResourceFrameStatus::StaleRegistration,
            };
        }
        match exact_remote_user_claim_is_current(
            &services,
            &msg.frame.jid,
            &registration.user_owner,
            registration.user_claim_epoch,
        )
        .await
        {
            Ok(()) => {}
            Err(RemoteUserClaimValidationError::Stale) => {
                self.detach_stale_remote_socket_resource(&msg.frame.jid, &registration)
                    .await;
                return RelayRemoteResourceFrameReply {
                    status: RelayRemoteResourceFrameStatus::StaleRegistration,
                };
            }
            Err(RemoteUserClaimValidationError::Unavailable) => {
                return RelayRemoteResourceFrameReply {
                    status: RelayRemoteResourceFrameStatus::Unavailable,
                };
            }
        }
        match exact_remote_resource_admission_is_current(
            &services,
            &msg.frame.jid,
            registration.registration_id,
            registration.admission_epoch,
            &registration.socket_node,
        )
        .await
        {
            Ok(()) => {}
            Err(RemoteUserClaimValidationError::Stale) => {
                self.detach_stale_remote_socket_resource(&msg.frame.jid, &registration)
                    .await;
                return RelayRemoteResourceFrameReply {
                    status: RelayRemoteResourceFrameStatus::StaleRegistration,
                };
            }
            Err(RemoteUserClaimValidationError::Unavailable) => {
                return RelayRemoteResourceFrameReply {
                    status: RelayRemoteResourceFrameStatus::Unavailable,
                };
            }
        }
        let outbound = OutboundStanza {
            stanza: msg.frame.stanza.0,
            kind: msg.frame.kind,
            pending_row_id: None,
            pending_row_original_receipt_at: None,
        };
        let outcome = {
            let registrations = self.remote_socket_resources.lock().await;
            let authority_is_current = self
                .remote_resource_registration_allowed(&services, fence_generation)
                && services.node_identity.current() == registration.socket_node
                && registrations.get(&msg.frame.jid).is_some_and(|current| {
                    remote_socket_registration_matches(current, &registration)
                });
            authority_is_current.then(|| {
                services.connection_registry.try_send_outbound_if_owner(
                    &msg.frame.jid,
                    &registration.owner,
                    outbound,
                )
            })
        };
        let Some(outcome) = outcome else {
            self.detach_stale_remote_socket_resource(&msg.frame.jid, &registration)
                .await;
            return RelayRemoteResourceFrameReply {
                status: RelayRemoteResourceFrameStatus::StaleRegistration,
            };
        };
        let status = match outcome {
            BroadcastOutcome::Delivered => RelayRemoteResourceFrameStatus::Delivered,
            BroadcastOutcome::DroppedFull => RelayRemoteResourceFrameStatus::Backpressure,
            BroadcastOutcome::NotConnected | BroadcastOutcome::DroppedClosed => {
                RelayRemoteResourceFrameStatus::StaleRegistration
            }
        };
        if status == RelayRemoteResourceFrameStatus::StaleRegistration {
            self.detach_stale_remote_socket_resource(&msg.frame.jid, &registration)
                .await;
        }
        RelayRemoteResourceFrameReply { status }
    }

    async fn deliver_registered_remote_resource_with_registration(
        &self,
        target: &jid::FullJid,
        stanza: &Stanza,
        kind: DeliveryKind,
        registration: &RemoteOwnerRegistration,
    ) -> Option<FullJidDeliveryOutcome> {
        let mut handle = RelayHandle::new(
            NodeId::new(registration.socket_node.node_id.clone()),
            self.stop_token.clone(),
        )
        .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        match handle
            .deliver_remote_resource_frame(RelayDeliverRemoteResourceFrame {
                frame: RemoteResourceOutboundFrame {
                    jid: target.clone(),
                    registration_id: registration.registration_id,
                    admission_epoch: registration.admission_epoch,
                    socket_generation: registration.socket_generation,
                    socket_node: registration.socket_node.clone(),
                    expected_user_owner: registration.user_owner.clone(),
                    expected_user_claim_epoch: registration.user_claim_epoch,
                    stanza: RemoteStanza(stanza.clone()),
                    kind,
                },
            })
            .await
        {
            Ok(RelayRemoteResourceFrameReply {
                status: RelayRemoteResourceFrameStatus::Delivered,
            }) => Some(FullJidDeliveryOutcome::Delivered),
            Ok(RelayRemoteResourceFrameReply {
                status: RelayRemoteResourceFrameStatus::Backpressure,
            }) => Some(FullJidDeliveryOutcome::Dropped),
            Ok(RelayRemoteResourceFrameReply {
                status: RelayRemoteResourceFrameStatus::StaleRegistration,
            }) => {
                self.cleanup_remote_owner_resource_if_registration(
                    target,
                    registration.registration_id,
                )
                .await;
                None
            }
            Ok(RelayRemoteResourceFrameReply {
                status: RelayRemoteResourceFrameStatus::Unavailable,
            }) => Some(FullJidDeliveryOutcome::Unavailable),
            Err(error) => {
                tracing::warn!(
                    jid = %target,
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
                } else if matches!(
                    error,
                    RelayAskError::NotFound { .. }
                        | RelayAskError::Send {
                            effect: RelaySendEffect::NoEffect,
                            ..
                        }
                ) {
                    Some(FullJidDeliveryOutcome::Unavailable)
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
        let fence_generation = self.remote_resource_fence_generation.load(Ordering::SeqCst);
        if !self.remote_resource_registration_allowed(&services, fence_generation) {
            return RelayForceDetachRemoteUserResourceReply {
                outcome: ForceDetachOutcome::NotPersisted,
                status: RelayRemoteResourceForceDetachStatus::Unknown,
            };
        }
        let registration = {
            let registrations = self.remote_socket_resources.lock().await;
            registrations
                .get(&msg.jid)
                .filter(|registration| {
                    registration.registration_id == msg.registration_id
                        && registration.admission_epoch == msg.admission_epoch
                        && registration.socket_generation == msg.socket_generation
                        && registration.socket_node == msg.socket_node
                        && registration.user_owner == msg.expected_user_owner
                        && registration.user_claim_epoch == msg.expected_user_claim_epoch
                })
                .cloned()
        };
        let Some(registration) = registration else {
            return RelayForceDetachRemoteUserResourceReply {
                outcome: ForceDetachOutcome::NotPersisted,
                status: RelayRemoteResourceForceDetachStatus::NotLive,
            };
        };
        if services.node_identity.current() != registration.socket_node {
            self.detach_remote_socket_resource(&msg.jid, &registration, msg.reason)
                .await;
            return RelayForceDetachRemoteUserResourceReply {
                outcome: ForceDetachOutcome::NotPersisted,
                status: RelayRemoteResourceForceDetachStatus::Invalidated,
            };
        }
        match exact_remote_user_claim_is_current(
            &services,
            &msg.jid,
            &registration.user_owner,
            registration.user_claim_epoch,
        )
        .await
        {
            Ok(()) => {}
            Err(RemoteUserClaimValidationError::Stale) => {
                self.detach_remote_socket_resource(&msg.jid, &registration, msg.reason)
                    .await;
                return RelayForceDetachRemoteUserResourceReply {
                    outcome: ForceDetachOutcome::NotPersisted,
                    status: RelayRemoteResourceForceDetachStatus::Invalidated,
                };
            }
            Err(RemoteUserClaimValidationError::Unavailable) => {
                return RelayForceDetachRemoteUserResourceReply {
                    outcome: ForceDetachOutcome::NotPersisted,
                    status: RelayRemoteResourceForceDetachStatus::Unknown,
                };
            }
        }
        match exact_remote_resource_admission_is_current(
            &services,
            &msg.jid,
            registration.registration_id,
            registration.admission_epoch,
            &registration.socket_node,
        )
        .await
        {
            Ok(()) => {}
            Err(RemoteUserClaimValidationError::Stale) => {
                self.detach_remote_socket_resource(&msg.jid, &registration, msg.reason)
                    .await;
                return RelayForceDetachRemoteUserResourceReply {
                    outcome: ForceDetachOutcome::NotPersisted,
                    status: RelayRemoteResourceForceDetachStatus::Invalidated,
                };
            }
            Err(RemoteUserClaimValidationError::Unavailable) => {
                return RelayForceDetachRemoteUserResourceReply {
                    outcome: ForceDetachOutcome::NotPersisted,
                    status: RelayRemoteResourceForceDetachStatus::Unknown,
                };
            }
        }
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
            reason: msg.reason,
            ack,
        };
        let queued = {
            let registrations = self.remote_socket_resources.lock().await;
            let authority_is_current = self
                .remote_resource_registration_allowed(&services, fence_generation)
                && services.node_identity.current() == registration.socket_node
                && registrations.get(&msg.jid).is_some_and(|current| {
                    remote_socket_registration_matches(current, &registration)
                });
            authority_is_current.then(|| entry.force_detach_sender().try_send(request))
        };
        let Some(queued) = queued else {
            self.detach_remote_socket_resource(&msg.jid, &registration, msg.reason)
                .await;
            return RelayForceDetachRemoteUserResourceReply {
                outcome: ForceDetachOutcome::NotPersisted,
                status: RelayRemoteResourceForceDetachStatus::Invalidated,
            };
        };
        if queued.is_err() {
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
        self.detach_remote_socket_resource(
            jid,
            registration,
            ForceDetachReason::RemoteStateInvalidated,
        )
        .await;
    }

    async fn detach_remote_socket_resource(
        &self,
        jid: &jid::FullJid,
        registration: &RemoteSocketRegistration,
        reason: ForceDetachReason,
    ) {
        let Some(services) = self.services.get().cloned() else {
            return;
        };
        let removed = {
            let mut registrations = self.remote_socket_resources.lock().await;
            if registrations
                .get(jid)
                .is_some_and(|current| remote_socket_registration_matches(current, registration))
            {
                registrations.remove(jid);
                true
            } else {
                false
            }
        };
        if !removed {
            return;
        }
        self.cancel_remote_resource_admission(&services, jid, registration)
            .await;
        self.retain_remote_socket_cleanup(jid, registration).await;
        let Some(entry) = services
            .connection_registry
            .entry_if_owner(jid, &registration.owner)
        else {
            return;
        };
        let retirement = entry.retirement_handle();
        let (ack, ack_rx) = tokio::sync::oneshot::channel();
        let queued = entry.force_detach_sender().try_send(ForceDetachRequest {
            requester_bare_jid: jid.to_bare(),
            reason,
            ack,
        });
        let cooperative = if queued.is_ok() {
            matches!(
                tokio::time::timeout(REMOTE_RESOURCE_STALE_DETACH_TIMEOUT, ack_rx).await,
                Ok(Ok(
                    ForceDetachOutcome::Detached | ForceDetachOutcome::NotPersisted
                ))
            )
        } else {
            false
        };

        let terminated = if cooperative {
            match retirement.as_ref() {
                Some(handle) => {
                    tokio::time::timeout(REMOTE_RESOURCE_STALE_DETACH_TIMEOUT, handle.terminated())
                        .await
                        .is_ok()
                }
                None => true,
            }
        } else {
            false
        };
        if !terminated {
            if let Some(handle) = retirement {
                handle.abort();
                let _ =
                    tokio::time::timeout(REMOTE_RESOURCE_STALE_DETACH_TIMEOUT, handle.terminated())
                        .await;
            }
        }
        services
            .connection_registry
            .unregister_if_owner(jid, &registration.owner);
    }

    async fn retire_remote_owner_registration(
        &self,
        services: &OrderedRelayDeliveryServices,
        jid: &jid::FullJid,
        registration: &RemoteOwnerRegistration,
    ) -> bool {
        if registration.placement == ConnectionPlacement::LocalSocket {
            self.detach_remote_socket_resource(
                jid,
                &RemoteSocketRegistration {
                    registration_id: registration.registration_id,
                    admission_epoch: registration.admission_epoch,
                    socket_generation: registration.socket_generation,
                    socket_node: registration.socket_node.clone(),
                    owner: registration.owner.clone(),
                    user_owner: registration.user_owner.clone(),
                    user_claim_epoch: registration.user_claim_epoch,
                },
                ForceDetachReason::ResourceReplaced,
            )
            .await;
            if !unregister_remote_owner_actor_entry(services, jid, &registration.owner).await {
                self.retain_remote_owner_cleanup(jid, registration).await;
                return false;
            }
            services
                .connection_registry
                .unregister_if_owner(jid, &registration.owner);
            self.remove_pending_remote_owner_cleanup_if_current(jid, registration)
                .await;
            return true;
        }
        let mut handle = RelayHandle::new(
            NodeId::new(registration.socket_node.node_id.clone()),
            self.stop_token.clone(),
        )
        .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        let detach = handle
            .force_detach_remote_user_resource(RelayForceDetachRemoteUserResource {
                jid: jid.clone(),
                registration_id: registration.registration_id,
                admission_epoch: registration.admission_epoch,
                socket_generation: registration.socket_generation,
                socket_node: registration.socket_node.clone(),
                expected_user_owner: registration.user_owner.clone(),
                expected_user_claim_epoch: registration.user_claim_epoch,
                requester_bare_jid: jid.to_bare(),
                reason: ForceDetachReason::ResourceReplaced,
            })
            .await;
        let retired = self
            .finish_remote_owner_registration_retire(services, jid, registration, detach)
            .await;
        if retired {
            self.remove_pending_remote_owner_cleanup_if_current(jid, registration)
                .await;
        }
        retired
    }

    /// Bounded direct compensation when the owner-mirror control queue is
    /// full, closed, or wedged during ownership loss. The stored exact socket
    /// incarnation and UserActor claim evidence remain sufficient for the
    /// socket node to either perform the requested detach or recognize that
    /// the authority is stale and terminally invalidate its own registration.
    pub(crate) async fn terminally_force_detach_remote_mirror_if_owner(
        &self,
        jid: &jid::FullJid,
        owner: &Arc<AtomicBool>,
        reason: ForceDetachReason,
    ) -> bool {
        let active_registration = {
            let registrations = self.remote_owner_resources.lock().await;
            registrations
                .get(jid)
                .filter(|registration| Arc::ptr_eq(&registration.owner, owner))
                .cloned()
        };
        let registration = if let Some(registration) = active_registration {
            Some(registration)
        } else {
            self.remote_owner_pending_cleanup
                .lock()
                .await
                .values()
                .find(|(pending_jid, registration)| {
                    pending_jid == jid && Arc::ptr_eq(&registration.owner, owner)
                })
                .map(|(_, registration)| registration.clone())
        };
        let Some(registration) = registration else {
            return true;
        };
        let mut handle = RelayHandle::new(
            NodeId::new(registration.socket_node.node_id.clone()),
            self.stop_token.clone(),
        )
        .with_ask_timeouts(self.mailbox_timeout, self.reply_timeout);
        match handle
            .force_detach_remote_user_resource(RelayForceDetachRemoteUserResource {
                jid: jid.clone(),
                registration_id: registration.registration_id,
                admission_epoch: registration.admission_epoch,
                socket_generation: registration.socket_generation,
                socket_node: registration.socket_node,
                expected_user_owner: registration.user_owner,
                expected_user_claim_epoch: registration.user_claim_epoch,
                requester_bare_jid: jid.to_bare(),
                reason,
            })
            .await
        {
            Ok(reply) => matches!(
                reply.status,
                RelayRemoteResourceForceDetachStatus::Detached
                    | RelayRemoteResourceForceDetachStatus::Invalidated
                    | RelayRemoteResourceForceDetachStatus::NotLive
            ),
            Err(error) => {
                tracing::warn!(
                    jid = %jid,
                    registration_id = ?registration.registration_id,
                    %error,
                    "direct remote-mirror force-detach ask failed; physical socket retirement remains uncertain"
                );
                false
            }
        }
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
                    self.retain_remote_owner_cleanup(jid, registration).await;
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
                | RelayRemoteResourceForceDetachStatus::Invalidated
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
            self.retain_remote_owner_cleanup(jid, registration).await;
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
    ) -> bool {
        let Some(_cleanup_operation) = RemoteOwnerOperationGuard::begin(
            Arc::clone(&self.remote_owner_cleanup_inflight),
            registration_id,
        ) else {
            return false;
        };
        self.cleanup_remote_owner_resource_if_registration_inflight(jid, registration_id)
            .await
    }

    async fn cleanup_remote_owner_resource_if_registration_inflight(
        &self,
        jid: &jid::FullJid,
        registration_id: RemoteResourceRegistrationId,
    ) -> bool {
        let Some(services) = self.services.get().cloned() else {
            return false;
        };
        let active_registration = {
            let registrations = self.remote_owner_resources.lock().await;
            registrations
                .get(jid)
                .filter(|registration| registration.registration_id == registration_id)
                .cloned()
        };
        let registration = if let Some(registration) = active_registration {
            Some(registration)
        } else {
            self.remote_owner_pending_cleanup
                .lock()
                .await
                .get(&registration_id)
                .filter(|(pending_jid, _)| pending_jid == jid)
                .map(|(_, registration)| registration.clone())
        };
        let Some(registration) = registration else {
            return true;
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
            self.retain_remote_owner_cleanup(jid, &registration).await;
            return false;
        }
        services
            .connection_registry
            .unregister_if_owner(jid, &registration.owner);
        self.remove_remote_owner_registration_if_current(jid, &registration)
            .await;
        self.remove_pending_remote_owner_cleanup_if_current(jid, &registration)
            .await;
        true
    }

    fn spawn_remote_resource_forwarder(
        self: &Arc<Self>,
        jid: jid::FullJid,
        registration: RemoteOwnerRegistration,
        mut rx: mpsc::Receiver<OutboundStanza>,
        force_detach_rx: Option<mpsc::Receiver<ForceDetachRequest>>,
    ) {
        let outbound_bridge = Arc::clone(self);
        let outbound_jid = jid.clone();
        let outbound_registration = registration.clone();
        tokio::spawn(async move {
            while let Some(outbound) = rx.recv().await {
                forward_remote_resource_outbound(
                    &outbound_bridge,
                    &outbound_jid,
                    &outbound_registration,
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
                        &registration,
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
            asserted_origin_node: me.clone(),
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

async fn exact_remote_user_claim_is_current(
    services: &OrderedRelayDeliveryServices,
    jid: &jid::FullJid,
    expected_owner: &NodeIdentity,
    expected_claim_epoch: ClaimEpoch,
) -> Result<(), RemoteUserClaimValidationError> {
    let entity = user_entity(&jid.to_bare());
    let snapshot = services
        .claim_store
        .current_claim(&entity)
        .await
        .map_err(|error| {
            tracing::warn!(
                %jid,
                %error,
                "clustered remote-resource exact UserActor claim lookup failed"
            );
            RemoteUserClaimValidationError::Unavailable
        })?
        .ok_or(RemoteUserClaimValidationError::Stale)?;
    if !snapshot.owner_lease_fresh
        || snapshot.owner != *expected_owner
        || snapshot.claim_epoch != expected_claim_epoch
    {
        return Err(RemoteUserClaimValidationError::Stale);
    }
    Ok(())
}

async fn exact_remote_resource_admission_is_current(
    services: &OrderedRelayDeliveryServices,
    jid: &jid::FullJid,
    registration_id: RemoteResourceRegistrationId,
    admission_epoch: RemoteResourceAdmissionEpoch,
    socket_node: &NodeIdentity,
) -> Result<(), RemoteUserClaimValidationError> {
    match services
        .remote_resource_admission_store
        .is_current(jid, registration_id, admission_epoch, socket_node)
        .await
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(RemoteUserClaimValidationError::Stale),
        Err(error) => {
            tracing::warn!(
                %jid,
                registration_id = ?registration_id,
                %error,
                "clustered remote-resource exact admission lookup failed"
            );
            Err(RemoteUserClaimValidationError::Unavailable)
        }
    }
}

async fn exact_remote_socket_node_is_current(
    services: &OrderedRelayDeliveryServices,
    socket_node: &NodeIdentity,
) -> Result<(), RemoteUserClaimValidationError> {
    match services.node_lease.peer_id_for_node(socket_node).await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(RemoteUserClaimValidationError::Stale),
        Err(error) => {
            tracing::warn!(
                socket_node_id = %socket_node.node_id,
                socket_node_epoch = %socket_node.node_epoch,
                %error,
                "clustered remote-resource exact socket-node lookup failed"
            );
            Err(RemoteUserClaimValidationError::Unavailable)
        }
    }
}

async fn exact_remote_owner_authority_is_current(
    services: &OrderedRelayDeliveryServices,
    jid: &jid::FullJid,
    registration: &RemoteOwnerRegistration,
) -> Result<(), RemoteUserClaimValidationError> {
    if services.node_identity.current() != registration.user_owner {
        return Err(RemoteUserClaimValidationError::Stale);
    }
    exact_remote_user_claim_is_current(
        services,
        jid,
        &registration.user_owner,
        registration.user_claim_epoch,
    )
    .await?;
    exact_remote_socket_node_is_current(services, &registration.socket_node).await?;
    exact_remote_resource_admission_is_current(
        services,
        jid,
        registration.registration_id,
        registration.admission_epoch,
        &registration.socket_node,
    )
    .await
}

fn remote_registration_status_for_authority_error(
    error: RemoteUserClaimValidationError,
) -> RelayRemoteResourceRegistrationStatus {
    match error {
        RemoteUserClaimValidationError::Stale => {
            RelayRemoteResourceRegistrationStatus::StaleRegistration
        }
        RemoteUserClaimValidationError::Unavailable => {
            RelayRemoteResourceRegistrationStatus::Unavailable
        }
    }
}

async fn remote_owner_registration_is_current(
    services: &OrderedRelayDeliveryServices,
    jid: &jid::FullJid,
    registration: &RemoteOwnerRegistration,
) -> Result<(), RelayRemoteResourceRegistrationStatus> {
    exact_remote_owner_authority_is_current(services, jid, registration)
        .await
        .map_err(|error| match error {
            RemoteUserClaimValidationError::Stale => {
                RelayRemoteResourceRegistrationStatus::StaleRegistration
            }
            RemoteUserClaimValidationError::Unavailable => {
                RelayRemoteResourceRegistrationStatus::Unavailable
            }
        })?;
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
    })?;
    exact_remote_owner_authority_is_current(services, jid, registration)
        .await
        .map_err(remote_registration_status_for_authority_error)
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
        && left.admission_epoch == right.admission_epoch
        && left.socket_node == right.socket_node
        && left.socket_generation == right.socket_generation
        && left.user_owner == right.user_owner
        && left.user_claim_epoch == right.user_claim_epoch
        && Arc::ptr_eq(&left.owner, &right.owner)
        && left.placement == right.placement
}

fn remote_socket_and_owner_registration_match(
    socket: &RemoteSocketRegistration,
    owner: &RemoteOwnerRegistration,
) -> bool {
    socket.registration_id == owner.registration_id
        && socket.admission_epoch == owner.admission_epoch
        && socket.socket_generation == owner.socket_generation
        && socket.socket_node == owner.socket_node
        && socket.user_owner == owner.user_owner
        && socket.user_claim_epoch == owner.user_claim_epoch
        && Arc::ptr_eq(&socket.owner, &owner.owner)
}

fn remote_socket_registration_matches(
    left: &RemoteSocketRegistration,
    right: &RemoteSocketRegistration,
) -> bool {
    left.registration_id == right.registration_id
        && left.admission_epoch == right.admission_epoch
        && left.socket_generation == right.socket_generation
        && left.socket_node == right.socket_node
        && left.user_owner == right.user_owner
        && left.user_claim_epoch == right.user_claim_epoch
        && Arc::ptr_eq(&left.owner, &right.owner)
}

fn physical_token_matches_registration(
    token: &PhysicalResourceAdmissionToken,
    registration: &RemoteSocketRegistration,
) -> bool {
    token.registration_id == registration.registration_id
        && token.admission_epoch == registration.admission_epoch
        && token.socket_generation == registration.socket_generation
        && token.socket_node == registration.socket_node
}

async fn forward_remote_resource_outbound(
    bridge: &Arc<OrderedRelayDeliveryBridge>,
    jid: &jid::FullJid,
    registration: &RemoteOwnerRegistration,
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
        registration_id: registration.registration_id,
        admission_epoch: registration.admission_epoch,
        socket_generation: registration.socket_generation,
        socket_node: registration.socket_node.clone(),
        expected_user_owner: registration.user_owner.clone(),
        expected_user_claim_epoch: registration.user_claim_epoch,
        stanza: RemoteStanza(outbound.stanza),
        kind,
    };
    let mut handle = RelayHandle::new(
        NodeId::new(registration.socket_node.node_id.clone()),
        bridge.stop_token.clone(),
    )
    .with_ask_timeouts(bridge.mailbox_timeout, bridge.reply_timeout);
    match handle
        .deliver_remote_resource_frame(RelayDeliverRemoteResourceFrame { frame })
        .await
    {
        Ok(RelayRemoteResourceFrameReply {
            status: RelayRemoteResourceFrameStatus::Delivered,
        }) => {}
        Ok(RelayRemoteResourceFrameReply {
            status: RelayRemoteResourceFrameStatus::StaleRegistration,
        }) => {
            tracing::debug!(
                jid = %jid,
                "clustered remote-resource socket registration stale; cleaning owner mirror"
            );
            bridge
                .cleanup_remote_owner_resource_if_registration(jid, registration.registration_id)
                .await;
        }
        Ok(RelayRemoteResourceFrameReply {
            status: RelayRemoteResourceFrameStatus::Unavailable,
        }) => {
            tracing::warn!(
                jid = %jid,
                "clustered remote-resource delivery temporarily unavailable; retaining owner mirror"
            );
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
                    .cleanup_remote_owner_resource_if_registration(
                        jid,
                        registration.registration_id,
                    )
                    .await;
            }
        }
    }
}

async fn forward_remote_resource_force_detach(
    bridge: &Arc<OrderedRelayDeliveryBridge>,
    jid: &jid::FullJid,
    registration: &RemoteOwnerRegistration,
    request: ForceDetachRequest,
) {
    let mut handle = RelayHandle::new(
        NodeId::new(registration.socket_node.node_id.clone()),
        bridge.stop_token.clone(),
    )
    .with_ask_timeouts(bridge.mailbox_timeout, bridge.reply_timeout);
    let outcome = match handle
        .force_detach_remote_user_resource(RelayForceDetachRemoteUserResource {
            jid: jid.clone(),
            registration_id: registration.registration_id,
            admission_epoch: registration.admission_epoch,
            socket_generation: registration.socket_generation,
            socket_node: registration.socket_node.clone(),
            expected_user_owner: registration.user_owner.clone(),
            expected_user_claim_epoch: registration.user_claim_epoch,
            requester_bare_jid: request.requester_bare_jid,
            reason: request.reason,
        })
        .await
    {
        Ok(reply) => confirmed_remote_force_detach_outcome(reply),
        Err(error) => {
            tracing::warn!(
                jid = %jid,
                %error,
                "clustered remote-resource force-detach relay ask failed"
            );
            None
        }
    };
    if let Some(outcome) = outcome {
        let _ = request.ack.send(outcome);
    }
}

fn confirmed_remote_force_detach_outcome(
    reply: RelayForceDetachRemoteUserResourceReply,
) -> Option<ForceDetachOutcome> {
    match reply.status {
        RelayRemoteResourceForceDetachStatus::Detached
        | RelayRemoteResourceForceDetachStatus::Invalidated
        | RelayRemoteResourceForceDetachStatus::NotLive => Some(reply.outcome),
        RelayRemoteResourceForceDetachStatus::Refused => Some(ForceDetachOutcome::IdentityMismatch),
        RelayRemoteResourceForceDetachStatus::Unknown => None,
    }
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
        _ => Err(OrderedRelayNackReason::ParseFailure),
    }
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
        || origin.owner != envelope.asserted_origin_node
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
            asserted_origin_node = %envelope.asserted_origin_node.node_id,
            asserted_origin_epoch = %envelope.asserted_origin_node.node_epoch,
            "ordered relay: unsigned origin envelope rejected"
        );
        return Err(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        });
    };
    let public_key = PublicKey::try_decode_protobuf(&proof.public_key).map_err(|error| {
        tracing::warn!(
            %error,
            asserted_origin_node = %envelope.asserted_origin_node.node_id,
            asserted_origin_epoch = %envelope.asserted_origin_node.node_epoch,
            "ordered relay: origin proof public key did not decode"
        );
        OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        }
    })?;
    let signing_bytes = envelope.signing_bytes().map_err(|error| {
        tracing::warn!(
            %error,
            asserted_origin_node = %envelope.asserted_origin_node.node_id,
            asserted_origin_epoch = %envelope.asserted_origin_node.node_epoch,
            "ordered relay: failed to serialize origin verification bytes"
        );
        OrderedRelayNackReason::ParseFailure
    })?;
    if !public_key.verify(&signing_bytes, &proof.signature) {
        tracing::warn!(
            asserted_origin_node = %envelope.asserted_origin_node.node_id,
            asserted_origin_epoch = %envelope.asserted_origin_node.node_epoch,
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
        RelayAskError::Send {
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
    use crate::clustering::remote_resource_admission::{
        InMemoryRemoteResourceAdmissionStore, RemoteResourceAdmissionError,
        RemoteResourceAdmissionStore,
    };
    use kameo::actor::Spawn;
    use libp2p::PeerId;
    use std::collections::HashSet;
    use waddle_xmpp::ownership::{
        ClaimEpoch, ClaimError, InProcessClaimStore, NodeIdentity, StalePredicate,
    };
    use xmpp_parsers::message::{Lang, Message};

    struct DelayedCurrentClaimStore {
        inner: Arc<InProcessClaimStore>,
        delay_next_read: AtomicBool,
        read_started: tokio::sync::Notify,
        release_read: tokio::sync::Notify,
    }

    impl DelayedCurrentClaimStore {
        fn new(inner: Arc<InProcessClaimStore>) -> Arc<Self> {
            Arc::new(Self {
                inner,
                delay_next_read: AtomicBool::new(false),
                read_started: tokio::sync::Notify::new(),
                release_read: tokio::sync::Notify::new(),
            })
        }

        fn delay_next_read(&self) {
            self.delay_next_read.store(true, Ordering::SeqCst);
        }

        async fn wait_until_read_started(&self) {
            self.read_started.notified().await;
        }

        fn release_read(&self) {
            self.release_read.notify_one();
        }
    }

    #[async_trait::async_trait]
    impl ClaimStore for DelayedCurrentClaimStore {
        async fn ensure_schema(&self) -> Result<(), ClaimError> {
            self.inner.ensure_schema().await
        }

        async fn acquire(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner.acquire(entity, me).await
        }

        async fn ensure_claimed(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner.ensure_claimed(entity, me).await
        }

        async fn steal_stale(
            &self,
            entity: &Entity,
            observed: ClaimEpoch,
            staleness: StalePredicate,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner
                .steal_stale(entity, observed, staleness, me)
                .await
        }

        async fn reclaim_after_self_fence(
            &self,
            entity: &Entity,
            observed: ClaimEpoch,
            expected_owner: &NodeIdentity,
            me: &NodeIdentity,
            lease_ttl: Duration,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner
                .reclaim_after_self_fence(entity, observed, expected_owner, me, lease_ttl)
                .await
        }

        async fn steal_for_resume(
            &self,
            entity: &Entity,
            observed: ClaimEpoch,
            witness: waddle_xmpp::ownership::ResumeIdentityProof,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner
                .steal_for_resume(entity, observed, witness, me)
                .await
        }

        async fn current_claim(
            &self,
            entity: &Entity,
        ) -> Result<Option<waddle_xmpp::ownership::ClaimSnapshot>, ClaimError> {
            let snapshot = self.inner.current_claim(entity).await?;
            if self.delay_next_read.swap(false, Ordering::SeqCst) {
                self.read_started.notify_one();
                self.release_read.notified().await;
            }
            Ok(snapshot)
        }

        async fn fence(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
            mine: ClaimEpoch,
        ) -> Result<bool, ClaimError> {
            self.inner.fence(entity, me, mine).await
        }

        async fn release(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
            mine: ClaimEpoch,
        ) -> Result<(), ClaimError> {
            self.inner.release(entity, me, mine).await
        }

        async fn release_many(
            &self,
            grants: &[waddle_xmpp::ownership::ClaimGrant],
        ) -> Result<(), ClaimError> {
            self.inner.release_many(grants).await
        }
    }

    #[derive(Default)]
    struct PermissiveRemoteResourceAdmissionStore {
        next_epoch: std::sync::atomic::AtomicI64,
    }

    #[async_trait::async_trait]
    impl RemoteResourceAdmissionStore for PermissiveRemoteResourceAdmissionStore {
        async fn ensure_schema(&self) -> Result<(), RemoteResourceAdmissionError> {
            Ok(())
        }

        async fn reserve(
            &self,
            _jid: &jid::FullJid,
            _registration_id: RemoteResourceRegistrationId,
            _socket_node: &NodeIdentity,
        ) -> Result<RemoteResourceAdmissionEpoch, RemoteResourceAdmissionError> {
            let previous = self.next_epoch.fetch_add(1, Ordering::SeqCst);
            Ok(RemoteResourceAdmissionEpoch(previous + 1))
        }

        async fn is_current(
            &self,
            _jid: &jid::FullJid,
            _registration_id: RemoteResourceRegistrationId,
            _admission_epoch: RemoteResourceAdmissionEpoch,
            _socket_node: &NodeIdentity,
        ) -> Result<bool, RemoteResourceAdmissionError> {
            Ok(true)
        }

        async fn cancel(
            &self,
            _jid: &jid::FullJid,
            _registration_id: RemoteResourceRegistrationId,
            _admission_epoch: RemoteResourceAdmissionEpoch,
            _socket_node: &NodeIdentity,
        ) -> Result<bool, RemoteResourceAdmissionError> {
            Ok(true)
        }

        async fn prune_stale(&self, _limit: usize) -> Result<u64, RemoteResourceAdmissionError> {
            Ok(0)
        }
    }

    struct FailFirstCancelAdmissionStore {
        inner: InMemoryRemoteResourceAdmissionStore,
        fail_next_cancel: AtomicBool,
    }

    impl FailFirstCancelAdmissionStore {
        fn new() -> Self {
            Self {
                inner: InMemoryRemoteResourceAdmissionStore::default(),
                fail_next_cancel: AtomicBool::new(true),
            }
        }
    }

    #[async_trait::async_trait]
    impl RemoteResourceAdmissionStore for FailFirstCancelAdmissionStore {
        async fn ensure_schema(&self) -> Result<(), RemoteResourceAdmissionError> {
            Ok(())
        }

        async fn reserve(
            &self,
            jid: &jid::FullJid,
            registration_id: RemoteResourceRegistrationId,
            socket_node: &NodeIdentity,
        ) -> Result<RemoteResourceAdmissionEpoch, RemoteResourceAdmissionError> {
            self.inner.reserve(jid, registration_id, socket_node).await
        }

        async fn is_current(
            &self,
            jid: &jid::FullJid,
            registration_id: RemoteResourceRegistrationId,
            admission_epoch: RemoteResourceAdmissionEpoch,
            socket_node: &NodeIdentity,
        ) -> Result<bool, RemoteResourceAdmissionError> {
            self.inner
                .is_current(jid, registration_id, admission_epoch, socket_node)
                .await
        }

        async fn cancel(
            &self,
            jid: &jid::FullJid,
            registration_id: RemoteResourceRegistrationId,
            admission_epoch: RemoteResourceAdmissionEpoch,
            socket_node: &NodeIdentity,
        ) -> Result<bool, RemoteResourceAdmissionError> {
            if self.fail_next_cancel.swap(false, Ordering::SeqCst) {
                return Err(RemoteResourceAdmissionError::Backend(
                    "injected first cancellation failure".to_string(),
                ));
            }
            self.inner
                .cancel(jid, registration_id, admission_epoch, socket_node)
                .await
        }

        async fn prune_stale(&self, limit: usize) -> Result<u64, RemoteResourceAdmissionError> {
            self.inner.prune_stale(limit).await
        }
    }

    struct StaticNodeLease {
        origin: NodeIdentity,
        peer_id: String,
        live_socket: std::sync::RwLock<NodeIdentity>,
    }

    impl StaticNodeLease {
        fn rotate_socket(&self, identity: NodeIdentity) {
            *self
                .live_socket
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = identity;
        }
    }

    #[async_trait::async_trait]
    impl NodeLeaseStore for StaticNodeLease {
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
            let live_socket = self
                .live_socket
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if node == &self.origin || node == &*live_socket {
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
            _target_owner: &NodeIdentity,
            _target_epoch: ClaimEpoch,
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

    fn socket_identity() -> NodeIdentity {
        NodeIdentity::new("socket-node", "socket-epoch")
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

    fn remote_resource_registration(
        expected_user_owner: NodeIdentity,
    ) -> RelayRegisterRemoteUserResource {
        RelayRegisterRemoteUserResource {
            jid: target_full(),
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(1),
            socket_generation: RemoteResourceSocketGeneration::next(None),
            socket_node: socket_identity(),
            expected_user_owner,
            expected_user_claim_epoch: ClaimEpoch(2),
            state: RemoteResourceStateSnapshot {
                carbons_enabled: false,
                roster_interested: false,
                blocklist_interested: false,
                presence_available: false,
                presence_priority: 0,
                presence_state: None,
            },
        }
    }

    async fn steal_and_regrant_target_claim(
        store: &InProcessClaimStore,
        owner: &NodeIdentity,
    ) -> ClaimEpoch {
        let entity = target_entity();
        let original = store
            .current_claim(&entity)
            .await
            .expect("read original target claim")
            .expect("target claim exists");
        let stolen = store
            .steal_stale(
                &entity,
                original.claim_epoch,
                StalePredicate::OwnerStale,
                &other_identity(),
            )
            .await
            .expect("steal target claim");
        store
            .steal_stale(&entity, stolen, StalePredicate::OwnerStale, owner)
            .await
            .expect("regrant target claim to the original incarnation")
    }

    fn envelope_claims(target_epoch: i64) -> OrderedRelayEnvelopeClaims {
        OrderedRelayEnvelopeClaims::new(
            OrderedRelayClaim {
                entity: origin_entity(),
                epoch: ClaimEpoch(0),
            },
            OrderedRelayClaim {
                entity: sender_entity(),
                epoch: ClaimEpoch(1),
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
            asserted_origin_node: origin_identity(),
            channel: OrderedRelayChannel {
                origin: OrderedRelayOrigin::SmSession(
                    waddle_xmpp::pending_delivery::SmSessionId::new("stream-1"),
                ),
                recipient: OrderedRelayRecipient::FullJid(target_full()),
                target_epoch: waddle_xmpp::ownership::ClaimEpoch(2),
            },
            sequence: OrderedRelaySequence::FIRST,
            origin_inbound_sequence: OriginInboundSequence(1),
            origin_claim: OrderedRelayClaim {
                entity: origin_entity(),
                epoch: waddle_xmpp::ownership::ClaimEpoch(0),
            },
            sender_claim: OrderedRelayClaim {
                entity: sender_entity(),
                epoch: waddle_xmpp::ownership::ClaimEpoch(1),
            },
            target_claim: OrderedRelayClaim {
                entity: target_entity(),
                epoch: waddle_xmpp::ownership::ClaimEpoch(2),
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

    fn signed_envelope(keypair: &Keypair) -> RemoteStanzaEnvelope {
        sign_envelope(envelope(), keypair)
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
        services_with_claims_and_store(
            origin_owner,
            target_owner,
            receiver,
            origin_peer_id,
            blocking_storage,
        )
        .await
        .0
    }

    async fn services_with_claims_and_store(
        origin_owner: NodeIdentity,
        target_owner: NodeIdentity,
        receiver: NodeIdentity,
        origin_peer_id: String,
        blocking_storage: Arc<dyn BlockingStorage>,
    ) -> (OrderedRelayDeliveryServices, Arc<InProcessClaimStore>) {
        let (services, store, _) = services_with_claims_and_controls(
            origin_owner,
            target_owner,
            receiver,
            origin_peer_id,
            blocking_storage,
        )
        .await;
        (services, store)
    }

    async fn services_with_claims_and_controls(
        origin_owner: NodeIdentity,
        target_owner: NodeIdentity,
        receiver: NodeIdentity,
        origin_peer_id: String,
        blocking_storage: Arc<dyn BlockingStorage>,
    ) -> (
        OrderedRelayDeliveryServices,
        Arc<InProcessClaimStore>,
        Arc<StaticNodeLease>,
    ) {
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
        let node_lease = Arc::new(StaticNodeLease {
            origin: origin_owner,
            peer_id: origin_peer_id.clone(),
            live_socket: std::sync::RwLock::new(socket_identity()),
        });
        let services = OrderedRelayDeliveryServices {
            claim_store: store.clone(),
            allowlist_store: Arc::new(StaticAllowlist {
                peer_id: origin_peer_id.parse().expect("valid test peer id"),
            }),
            node_lease: node_lease.clone(),
            remote_resource_admission_store: Arc::new(
                PermissiveRemoteResourceAdmissionStore::default(),
            ),
            node_identity: SharedNodeIdentity::new(receiver),
            connection_registry: Arc::new(ConnectionRegistry::new()),
            user_registry: UserRegistryActor::spawn(UserRegistryActor::new()),
            sm_session_registry: Arc::new(InMemorySmSessionRegistry::new()),
            blocking_storage,
            web_socket_state: Weak::new(),
        };
        (services, store, node_lease)
    }

    #[tokio::test]
    async fn newer_local_physical_admission_retires_old_and_falls_through_local_delivery() {
        let me = receiver_identity();
        let (mut services, _claims, _node_lease) = services_with_claims_and_controls(
            me.clone(),
            me.clone(),
            me.clone(),
            test_peer_id(),
            Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new()),
        )
        .await;
        let admissions = Arc::new(InMemoryRemoteResourceAdmissionStore::default());
        services.remote_resource_admission_store = admissions.clone();
        let services = Arc::new(services);
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));
        let jid = target_full();

        let (old_tx, _old_rx) = mpsc::channel(1);
        let old_entry = ConnectionEntry::new(old_tx);
        let old_owner = old_entry.carbons_handle();
        let mut old_detach_rx = old_entry
            .take_force_detach_rx()
            .expect("old physical force-detach receiver");
        services
            .connection_registry
            .register_entry(jid.clone(), old_entry);
        let old_id = RemoteResourceRegistrationId::fresh();
        let old_epoch = admissions
            .reserve(&jid, old_id, &me)
            .await
            .expect("old durable admission");
        let old_registration = RemoteSocketRegistration {
            registration_id: old_id,
            admission_epoch: old_epoch,
            socket_generation: RemoteResourceSocketGeneration(1),
            socket_node: me.clone(),
            owner: Arc::clone(&old_owner),
            user_owner: me.clone(),
            user_claim_epoch: ClaimEpoch(2),
        };
        assert!(
            bridge
                .reserve_remote_socket_cleanup_capacity(&jid, &old_registration)
                .await
        );
        assert!(
            bridge
                .publish_pending_remote_socket_registration(&jid, old_registration.clone())
                .await
        );
        let old_token = PhysicalResourceAdmissionToken {
            registration_id: old_id,
            admission_epoch: old_epoch,
            socket_generation: old_registration.socket_generation,
            socket_node: me.clone(),
        };

        let (new_tx, _new_rx) = mpsc::channel(1);
        let new_entry = ConnectionEntry::new(new_tx);
        let new_owner = new_entry.carbons_handle();
        let begin = tokio::spawn({
            let bridge = Arc::clone(&bridge);
            let jid = jid.clone();
            let new_owner = Arc::clone(&new_owner);
            async move { bridge.begin_physical_user_resource(&jid, new_owner).await }
        });
        let request = tokio::time::timeout(Duration::from_secs(1), old_detach_rx.recv())
            .await
            .expect("new admission promptly retires old socket")
            .expect("old socket receives exact retirement");
        assert_eq!(request.reason, ForceDetachReason::ResourceReplaced);
        request
            .ack
            .send(ForceDetachOutcome::NotPersisted)
            .expect("ack old retirement");
        let guard = begin
            .await
            .expect("join begin")
            .expect("new physical admission");
        services
            .connection_registry
            .register_entry(jid.clone(), new_entry.clone());
        assert_eq!(
            bridge
                .publish_physical_user_resource(&guard, new_entry)
                .await,
            RemoteResourceRegisterOutcome::Registered
        );
        let new_token = bridge
            .finalize_physical_user_resource(guard)
            .await
            .expect("final exact admission");

        assert!(
            bridge
                .physical_resource_origin_if_owner(&jid, &old_owner, &old_token)
                .await
                .is_none(),
            "the old exact token must not observe or detach the winner"
        );
        bridge
            .forget_remote_resource_state_if_owner(
                &jid,
                &old_owner,
                ConnectionPlacement::LocalSocket,
            )
            .await;
        assert!(
            matches!(
                bridge
                    .physical_resource_origin_if_owner(&jid, &new_owner, &new_token)
                    .await,
                Some(PhysicalResourceRouteOrigin::LocalSocket)
            ),
            "the winner remains locally authorized after delayed old cleanup"
        );
        assert_eq!(
            bridge
                .remote_owner_resources
                .lock()
                .await
                .get(&jid)
                .map(|registration| registration.placement),
            Some(ConnectionPlacement::LocalSocket)
        );
        assert_eq!(
            bridge
                .try_deliver_registered_remote_resource(
                    &jid,
                    &Stanza::Message(Message::new(Some(jid::Jid::from(jid.clone())))),
                    DeliveryKind::DirectFrame,
                )
                .await,
            None,
            "an exact local-owner registration must fall through to local delivery"
        );
        assert!(services
            .connection_registry
            .entry_if_owner(&jid, &new_owner)
            .is_some());
        services
            .connection_registry
            .unregister_if_owner(&jid, &new_owner);
        bridge
            .unregister_remote_user_resource_if_owner(&jid, &new_owner)
            .await;
        assert!(!admissions
            .is_current(
                &jid,
                new_token.registration_id,
                new_token.admission_epoch,
                &new_token.socket_node,
            )
            .await
            .expect("read final admission cleanup"));
        assert!(bridge
            .remote_socket_resources
            .lock()
            .await
            .get(&jid)
            .is_none());
        assert!(bridge
            .remote_owner_resources
            .lock()
            .await
            .get(&jid)
            .is_none());
        assert!(!bridge
            .remote_socket_pending_cleanup
            .lock()
            .await
            .contains_key(&new_token.registration_id));
        assert!(!bridge
            .remote_owner_pending_cleanup
            .lock()
            .await
            .contains_key(&new_token.registration_id));
    }

    #[tokio::test]
    async fn abort_before_registry_publication_cancels_exact_local_admission() {
        let me = receiver_identity();
        let (mut services, _claims, _node_lease) = services_with_claims_and_controls(
            me.clone(),
            me.clone(),
            me,
            test_peer_id(),
            Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new()),
        )
        .await;
        let admissions = Arc::new(InMemoryRemoteResourceAdmissionStore::default());
        services.remote_resource_admission_store = admissions.clone();
        let services = Arc::new(services);
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));
        let jid = target_full();
        let (tx, _rx) = mpsc::channel(1);
        let owner = ConnectionEntry::new(tx).carbons_handle();

        let guard = bridge
            .begin_physical_user_resource(&jid, owner)
            .await
            .expect("reserve pre-publication physical admission");
        let token = guard.token();
        assert!(admissions
            .is_current(
                &jid,
                token.registration_id,
                token.admission_epoch,
                &token.socket_node,
            )
            .await
            .expect("read reserved admission"));

        bridge.abort_physical_user_resource(guard).await;

        assert!(!admissions
            .is_current(
                &jid,
                token.registration_id,
                token.admission_epoch,
                &token.socket_node,
            )
            .await
            .expect("read aborted admission"));
        assert!(bridge
            .remote_socket_resources
            .lock()
            .await
            .get(&jid)
            .is_none());
        assert!(!bridge
            .remote_socket_pending_cleanup
            .lock()
            .await
            .contains_key(&token.registration_id));
        assert!(bridge
            .remote_owner_resources
            .lock()
            .await
            .get(&jid)
            .is_none());
        assert!(!bridge
            .remote_owner_pending_cleanup
            .lock()
            .await
            .contains_key(&token.registration_id));
    }

    #[tokio::test]
    async fn final_sm_rollback_releases_guard_before_exact_cleanup() {
        let me = receiver_identity();
        let (mut services, _claims, _node_lease) = services_with_claims_and_controls(
            me.clone(),
            me.clone(),
            me,
            test_peer_id(),
            Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new()),
        )
        .await;
        let admissions = Arc::new(InMemoryRemoteResourceAdmissionStore::default());
        services.remote_resource_admission_store = admissions.clone();
        let services = Arc::new(services);
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));
        let jid = target_full();
        let (tx, _rx) = mpsc::channel(1);
        let entry = ConnectionEntry::new(tx);
        let owner = entry.carbons_handle();
        let guard = bridge
            .begin_physical_user_resource(&jid, Arc::clone(&owner))
            .await
            .expect("reserve physical admission");
        let token = guard.token();
        services
            .connection_registry
            .register_entry(jid.clone(), entry.clone());
        assert_eq!(
            bridge.publish_physical_user_resource(&guard, entry).await,
            RemoteResourceRegisterOutcome::Registered
        );

        // `complete_pending_resume_claim` rolls these views back while the
        // physical guard is still held. The finalizer must observe that loss,
        // drop the guard, and only then enter exact cleanup.
        services
            .connection_registry
            .unregister_if_owner(&jid, &owner);
        services
            .user_registry
            .ask(UnregisterUserResource {
                jid: jid.clone(),
                owner: Some(owner),
            })
            .await
            .expect("simulate final SM actor rollback");
        let finalized = tokio::time::timeout(
            Duration::from_secs(1),
            bridge.finalize_physical_user_resource(guard),
        )
        .await
        .expect("final SM rollback must not deadlock on the per-JID guard");
        assert!(finalized.is_none());
        assert!(!admissions
            .is_current(
                &jid,
                token.registration_id,
                token.admission_epoch,
                &token.socket_node,
            )
            .await
            .expect("read final SM rollback admission"));
        assert!(bridge
            .remote_socket_resources
            .lock()
            .await
            .get(&jid)
            .is_none());
        assert!(bridge
            .remote_owner_resources
            .lock()
            .await
            .get(&jid)
            .is_none());
        assert!(!bridge
            .remote_socket_pending_cleanup
            .lock()
            .await
            .contains_key(&token.registration_id));
        assert!(!bridge
            .remote_owner_pending_cleanup
            .lock()
            .await
            .contains_key(&token.registration_id));
    }

    #[tokio::test]
    async fn remote_resource_registration_rejects_pre_fence_owner_epoch() {
        let expected_owner = NodeIdentity::new("receiver-node", "pre-fence-epoch");
        let recovered_owner = NodeIdentity::new("receiver-node", "recovered-epoch");
        let services = Arc::new(
            services_with_claims(
                origin_identity(),
                recovered_owner.clone(),
                recovered_owner,
                test_peer_id(),
            )
            .await,
        );
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));

        let reply = bridge
            .register_remote_user_resource_on_owner(remote_resource_registration(expected_owner))
            .await;

        assert_eq!(
            reply.status,
            RelayRemoteResourceRegistrationStatus::NotOwner
        );
        assert!(!services.connection_registry.is_connected(&target_full()));
        assert!(bridge.remote_owner_resources.lock().await.is_empty());
    }

    #[tokio::test]
    async fn remote_resource_registration_rejects_non_local_expected_incarnation() {
        let expected_owner = NodeIdentity::new("receiver-node", "pre-fence-epoch");
        let recovered_identity = NodeIdentity::new("receiver-node", "recovered-epoch");
        let services = Arc::new(
            services_with_claims(
                origin_identity(),
                expected_owner.clone(),
                recovered_identity,
                test_peer_id(),
            )
            .await,
        );
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));

        let reply = bridge
            .register_remote_user_resource_on_owner(remote_resource_registration(expected_owner))
            .await;

        assert_eq!(
            reply.status,
            RelayRemoteResourceRegistrationStatus::NotOwner
        );
        assert!(!services.connection_registry.is_connected(&target_full()));
        assert!(bridge.remote_owner_resources.lock().await.is_empty());
    }

    #[tokio::test]
    async fn remote_resource_registration_rejects_old_claim_after_same_incarnation_regrant() {
        let owner = receiver_identity();
        let (services, store) = services_with_claims_and_store(
            origin_identity(),
            owner.clone(),
            owner.clone(),
            test_peer_id(),
            Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new()),
        )
        .await;
        let services = Arc::new(services);
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));
        let current_epoch = steal_and_regrant_target_claim(&store, &owner).await;
        assert_ne!(current_epoch, ClaimEpoch(2));

        let reply = bridge
            .register_remote_user_resource_on_owner(remote_resource_registration(owner))
            .await;

        assert_eq!(
            reply.status,
            RelayRemoteResourceRegistrationStatus::NotOwner
        );
        assert!(!services.connection_registry.is_connected(&target_full()));
        assert!(bridge.remote_owner_resources.lock().await.is_empty());
    }

    #[tokio::test]
    async fn delayed_remote_update_cannot_mutate_same_incarnation_after_claim_regrant() {
        let owner = receiver_identity();
        let (services, store) = services_with_claims_and_store(
            origin_identity(),
            owner.clone(),
            owner.clone(),
            test_peer_id(),
            Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new()),
        )
        .await;
        let services = Arc::new(services);
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));
        let registration = remote_resource_registration(owner.clone());
        let reply = bridge
            .register_remote_user_resource_on_owner(registration.clone())
            .await;
        assert_eq!(
            reply.status,
            RelayRemoteResourceRegistrationStatus::Registered
        );
        let current_epoch = steal_and_regrant_target_claim(&store, &owner).await;
        assert_ne!(current_epoch, registration.expected_user_claim_epoch);

        let reply = bridge
            .update_remote_user_resource_on_owner(RelayUpdateRemoteUserResource {
                jid: registration.jid.clone(),
                registration_id: registration.registration_id,
                admission_epoch: registration.admission_epoch,
                socket_generation: registration.socket_generation,
                socket_node: registration.socket_node.clone(),
                expected_user_owner: registration.expected_user_owner.clone(),
                expected_user_claim_epoch: registration.expected_user_claim_epoch,
                update: RemoteResourceStateUpdate::Carbons { enabled: true },
            })
            .await;

        assert_eq!(
            reply.status,
            RelayRemoteResourceUpdateStatus::StaleRegistration
        );
        assert!(
            !services.connection_registry.is_connected(&registration.jid),
            "the stale owner mirror must be cleaned without mutating a new claim generation"
        );
        assert!(bridge.remote_owner_resources.lock().await.is_empty());
    }

    #[tokio::test]
    async fn delayed_pre_fence_socket_epoch_cannot_update_owner_mirror() {
        let owner = receiver_identity();
        let (services, _store, node_lease) = services_with_claims_and_controls(
            origin_identity(),
            owner.clone(),
            owner,
            test_peer_id(),
            Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new()),
        )
        .await;
        let services = Arc::new(services);
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));
        let registration = remote_resource_registration(receiver_identity());
        let reply = bridge
            .register_remote_user_resource_on_owner(registration.clone())
            .await;
        assert_eq!(
            reply.status,
            RelayRemoteResourceRegistrationStatus::Registered
        );
        let mirror = services
            .connection_registry
            .get_entry(&registration.jid)
            .expect("owner mirror");
        node_lease.rotate_socket(NodeIdentity::new(
            registration.socket_node.node_id.clone(),
            "post-fence-socket-epoch",
        ));

        let reply = bridge
            .update_remote_user_resource_on_owner(RelayUpdateRemoteUserResource {
                jid: registration.jid.clone(),
                registration_id: registration.registration_id,
                admission_epoch: registration.admission_epoch,
                socket_generation: registration.socket_generation,
                socket_node: registration.socket_node.clone(),
                expected_user_owner: registration.expected_user_owner.clone(),
                expected_user_claim_epoch: registration.expected_user_claim_epoch,
                update: RemoteResourceStateUpdate::Carbons { enabled: true },
            })
            .await;

        assert_eq!(
            reply.status,
            RelayRemoteResourceUpdateStatus::StaleRegistration
        );
        assert!(!mirror.carbons_enabled.load(Ordering::Relaxed));
        assert!(!services.connection_registry.is_connected(&registration.jid));
        assert!(bridge.remote_owner_resources.lock().await.is_empty());
    }

    #[tokio::test]
    async fn compensating_unregister_waits_for_delayed_register_and_is_idempotent() {
        let owner = receiver_identity();
        let services = Arc::new(
            services_with_claims(origin_identity(), owner.clone(), owner, test_peer_id()).await,
        );
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));
        let registration = remote_resource_registration(receiver_identity());
        let compensation = RelayUnregisterRemoteUserResource {
            jid: registration.jid.clone(),
            registration_id: registration.registration_id,
            admission_epoch: registration.admission_epoch,
            socket_generation: registration.socket_generation,
            socket_node: registration.socket_node.clone(),
            expected_user_owner: registration.expected_user_owner.clone(),
            expected_user_claim_epoch: registration.expected_user_claim_epoch,
        };
        let lock = bridge
            .lock_for_remote_owner_registration(&registration.jid)
            .await
            .expect("registration lock");
        let guard = lock.lock().await;

        let delayed_register = tokio::spawn({
            let bridge = Arc::clone(&bridge);
            let registration = registration.clone();
            async move {
                bridge
                    .register_remote_user_resource_on_owner(registration)
                    .await
            }
        });
        tokio::task::yield_now().await;
        let compensating_unregister = tokio::spawn({
            let bridge = Arc::clone(&bridge);
            let compensation = compensation.clone();
            async move {
                bridge
                    .unregister_remote_user_resource_on_owner(compensation)
                    .await
            }
        });
        tokio::task::yield_now().await;
        drop(guard);

        let registered = delayed_register.await.expect("delayed register task");
        assert_eq!(
            registered.status,
            RelayRemoteResourceRegistrationStatus::Registered
        );
        let removed = compensating_unregister
            .await
            .expect("compensating unregister task");
        assert_eq!(
            removed.status,
            RelayRemoteResourceUnregisterStatus::Terminal
        );
        assert!(bridge.remote_owner_resources.lock().await.is_empty());
        assert!(!services.connection_registry.is_connected(&registration.jid));

        let duplicate = bridge
            .unregister_remote_user_resource_on_owner(compensation)
            .await;
        assert_eq!(
            duplicate.status,
            RelayRemoteResourceUnregisterStatus::Terminal,
            "a retried compensation must remain an idempotent terminal no-op"
        );
    }

    #[tokio::test]
    async fn retry_after_initial_admission_cancel_failure_prevents_delayed_register_revival() {
        let owner = receiver_identity();
        let (mut services, _claims, _node_lease) = services_with_claims_and_controls(
            origin_identity(),
            owner.clone(),
            owner.clone(),
            test_peer_id(),
            Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new()),
        )
        .await;
        let admission_store = Arc::new(FailFirstCancelAdmissionStore::new());
        services.remote_resource_admission_store = admission_store.clone();
        let services = Arc::new(services);
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));

        let mut delayed_register = remote_resource_registration(owner);
        delayed_register.admission_epoch = admission_store
            .reserve(
                &delayed_register.jid,
                delayed_register.registration_id,
                &delayed_register.socket_node,
            )
            .await
            .expect("reserve admission for delayed register");
        let local_registration = RemoteSocketRegistration {
            registration_id: delayed_register.registration_id,
            admission_epoch: delayed_register.admission_epoch,
            socket_generation: delayed_register.socket_generation,
            socket_node: delayed_register.socket_node.clone(),
            owner: Arc::new(AtomicBool::new(true)),
            user_owner: delayed_register.expected_user_owner.clone(),
            user_claim_epoch: delayed_register.expected_user_claim_epoch,
        };

        assert!(
            !bridge
                .cancel_remote_resource_admission(
                    &services,
                    &delayed_register.jid,
                    &local_registration,
                )
                .await,
            "the injected first cancellation must remain retryable"
        );
        let retry = bridge
            .unregister_remote_user_resource_on_owner(RelayUnregisterRemoteUserResource {
                jid: delayed_register.jid.clone(),
                registration_id: delayed_register.registration_id,
                admission_epoch: delayed_register.admission_epoch,
                socket_generation: delayed_register.socket_generation,
                socket_node: delayed_register.socket_node.clone(),
                expected_user_owner: delayed_register.expected_user_owner.clone(),
                expected_user_claim_epoch: delayed_register.expected_user_claim_epoch,
            })
            .await;
        assert_eq!(
            retry.status,
            RelayRemoteResourceUnregisterStatus::Terminal,
            "the retry must revoke admission before reporting terminal"
        );
        assert!(!admission_store
            .is_current(
                &delayed_register.jid,
                delayed_register.registration_id,
                delayed_register.admission_epoch,
                &delayed_register.socket_node,
            )
            .await
            .expect("check revoked admission"));

        let late_reply = bridge
            .register_remote_user_resource_on_owner(delayed_register)
            .await;
        assert_eq!(
            late_reply.status,
            RelayRemoteResourceRegistrationStatus::StaleRegistration,
            "a register delayed behind terminal cleanup must not recreate the owner mirror"
        );
        assert!(bridge.remote_owner_resources.lock().await.is_empty());
    }

    #[tokio::test]
    async fn terminal_unregister_from_pending_owner_cleanup_releases_exact_reservation() {
        let owner = receiver_identity();
        let services = Arc::new(
            services_with_claims(origin_identity(), owner.clone(), owner, test_peer_id()).await,
        );
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));
        let registration = remote_resource_registration(receiver_identity());
        let registered = bridge
            .register_remote_user_resource_on_owner(registration.clone())
            .await;
        assert_eq!(
            registered.status,
            RelayRemoteResourceRegistrationStatus::Registered
        );
        let owner_registration = bridge
            .remote_owner_resources
            .lock()
            .await
            .remove(&registration.jid)
            .expect("published owner registration");
        assert!(bridge
            .remote_owner_pending_cleanup
            .lock()
            .await
            .contains_key(&owner_registration.registration_id));

        let removed = bridge
            .unregister_remote_user_resource_on_owner(RelayUnregisterRemoteUserResource {
                jid: registration.jid.clone(),
                registration_id: registration.registration_id,
                admission_epoch: registration.admission_epoch,
                socket_generation: registration.socket_generation,
                socket_node: registration.socket_node,
                expected_user_owner: registration.expected_user_owner,
                expected_user_claim_epoch: registration.expected_user_claim_epoch,
            })
            .await;

        assert_eq!(
            removed.status,
            RelayRemoteResourceUnregisterStatus::Terminal
        );
        assert!(bridge.remote_owner_resources.lock().await.is_empty());
        assert!(bridge.remote_owner_pending_cleanup.lock().await.is_empty());
        assert!(bridge
            .remote_owner_cleanup_inflight
            .lock()
            .expect("owner operation lock")
            .is_empty());
        assert!(!services.connection_registry.is_connected(&registration.jid));
    }

    #[tokio::test]
    async fn owner_cleanup_budget_bounds_active_and_uncertain_entries_per_full_jid() {
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let jid = target_full();
        let other_jid: jid::FullJid = "juliet@example.test/tablet".parse().expect("full JID");
        let mut reserved = Vec::new();
        for generation in 1..=MAX_PENDING_REMOTE_OWNER_CLEANUPS_PER_JID {
            let registration = RemoteOwnerRegistration {
                registration_id: RemoteResourceRegistrationId::fresh(),
                admission_epoch: RemoteResourceAdmissionEpoch(generation as i64),
                socket_node: socket_identity(),
                socket_generation: RemoteResourceSocketGeneration(generation as u64),
                user_owner: receiver_identity(),
                user_claim_epoch: ClaimEpoch(generation as i64),
                owner: Arc::new(AtomicBool::new(true)),
                placement: ConnectionPlacement::RemoteMirror,
            };
            let operation = bridge
                .reserve_remote_owner_cleanup_capacity(&jid, &registration)
                .await
                .expect("bounded owner cleanup reservation");
            drop(operation);
            reserved.push(registration);
        }
        let unrelated = RemoteOwnerRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(1),
            socket_node: socket_identity(),
            socket_generation: RemoteResourceSocketGeneration(1),
            user_owner: receiver_identity(),
            user_claim_epoch: ClaimEpoch(1),
            owner: Arc::new(AtomicBool::new(true)),
            placement: ConnectionPlacement::RemoteMirror,
        };
        let unrelated_operation = bridge
            .reserve_remote_owner_cleanup_capacity(&other_jid, &unrelated)
            .await
            .expect("unrelated owner cleanup reservation");
        drop(unrelated_operation);
        let overflow = RemoteOwnerRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(9),
            socket_node: socket_identity(),
            socket_generation: RemoteResourceSocketGeneration(9),
            user_owner: receiver_identity(),
            user_claim_epoch: ClaimEpoch(9),
            owner: Arc::new(AtomicBool::new(true)),
            placement: ConnectionPlacement::RemoteMirror,
        };

        assert!(
            bridge
                .reserve_remote_owner_cleanup_capacity(&jid, &overflow)
                .await
                .is_none(),
            "active plus uncertain owner registrations must share the same per-JID cap"
        );
        assert!(bridge
            .remote_owner_cleanup_inflight
            .lock()
            .expect("owner operation lock")
            .is_empty());
        bridge
            .remove_pending_remote_owner_cleanup_if_current(&jid, &reserved[0])
            .await;
        assert!(
            bridge
                .reserve_remote_owner_cleanup_capacity(&jid, &overflow)
                .await
                .is_some(),
            "one terminal exact cleanup re-opens exactly one per-JID slot"
        );
    }

    #[tokio::test]
    async fn owner_cleanup_global_budget_skips_active_entries_and_survives_self_fence() {
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let mut active_registration_id = None;
        let mut first_inactive = None;
        for index in 0..MAX_REMOTE_OWNER_CLEANUP_REGISTRATIONS {
            let jid: jid::FullJid = format!("owner{index}@example.test/resource")
                .parse()
                .expect("generated full JID");
            let registration = RemoteOwnerRegistration {
                registration_id: RemoteResourceRegistrationId::fresh(),
                admission_epoch: RemoteResourceAdmissionEpoch(index as i64 + 1),
                socket_node: socket_identity(),
                socket_generation: RemoteResourceSocketGeneration(1),
                user_owner: receiver_identity(),
                user_claim_epoch: ClaimEpoch(1),
                owner: Arc::new(AtomicBool::new(true)),
                placement: ConnectionPlacement::RemoteMirror,
            };
            let operation = bridge
                .reserve_remote_owner_cleanup_capacity(&jid, &registration)
                .await
                .expect("bounded owner cleanup reservation");
            drop(operation);
            if index == 0 {
                bridge
                    .remote_owner_resources
                    .lock()
                    .await
                    .insert(jid, registration.clone());
                active_registration_id = Some(registration.registration_id);
            } else if first_inactive.is_none() {
                first_inactive = Some((jid, registration));
            }
        }
        let overflow_jid: jid::FullJid = "owner-overflow@example.test/resource"
            .parse()
            .expect("overflow full JID");
        let overflow = RemoteOwnerRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(1),
            socket_node: socket_identity(),
            socket_generation: RemoteResourceSocketGeneration(1),
            user_owner: receiver_identity(),
            user_claim_epoch: ClaimEpoch(1),
            owner: Arc::new(AtomicBool::new(true)),
            placement: ConnectionPlacement::RemoteMirror,
        };

        assert!(
            bridge
                .reserve_remote_owner_cleanup_capacity(&overflow_jid, &overflow)
                .await
                .is_none(),
            "the owner-side global cap must reject before a register ask can maybe commit"
        );
        let retry_batch = bridge
            .inactive_remote_owner_cleanup_ids(REMOTE_OWNER_GLOBAL_CLEANUP_RETRY_BATCH)
            .await;
        assert_eq!(retry_batch.len(), REMOTE_OWNER_GLOBAL_CLEANUP_RETRY_BATCH);
        assert!(retry_batch
            .iter()
            .all(|(_, registration_id)| { Some(*registration_id) != active_registration_id }));

        bridge.clear_remote_resource_state_on_self_fence().await;
        assert!(bridge.remote_owner_resources.lock().await.is_empty());
        assert_eq!(
            bridge.remote_owner_pending_cleanup.lock().await.len(),
            MAX_REMOTE_OWNER_CLEANUP_REGISTRATIONS,
            "moving an already-reserved active entry on self-fence must not grow the map"
        );

        let (first_jid, first_registration) = first_inactive.expect("inactive reservation");
        bridge
            .remove_pending_remote_owner_cleanup_if_current(&first_jid, &first_registration)
            .await;
        assert!(
            bridge
                .reserve_remote_owner_cleanup_capacity(&overflow_jid, &overflow)
                .await
                .is_some(),
            "one terminal exact cleanup re-opens one global owner slot"
        );
        assert_eq!(
            bridge.remote_owner_pending_cleanup.lock().await.len(),
            MAX_REMOTE_OWNER_CLEANUP_REGISTRATIONS
        );
    }

    #[tokio::test]
    async fn retry_unregister_reply_preserves_maybe_committed_cleanup_evidence() {
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let jid = target_full();
        let registration = RemoteSocketRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(1),
            socket_generation: RemoteResourceSocketGeneration(1),
            socket_node: origin_identity(),
            owner: Arc::new(AtomicBool::new(true)),
            user_owner: receiver_identity(),
            user_claim_epoch: ClaimEpoch(2),
        };
        assert!(
            bridge
                .reserve_remote_socket_cleanup_capacity(&jid, &registration)
                .await
        );

        // Model a register that may have committed followed by an unregister
        // reaching an owner bridge that cannot currently access its services.
        // A legacy `removed: false` reply was ambiguous here and the socket
        // side incorrectly discarded its only rollback evidence.
        let reply = bridge
            .unregister_remote_user_resource_on_owner(RelayUnregisterRemoteUserResource {
                jid: jid.clone(),
                registration_id: registration.registration_id,
                admission_epoch: registration.admission_epoch,
                socket_generation: registration.socket_generation,
                socket_node: registration.socket_node.clone(),
                expected_user_owner: registration.user_owner.clone(),
                expected_user_claim_epoch: registration.user_claim_epoch,
            })
            .await;
        assert_eq!(reply.status, RelayRemoteResourceUnregisterStatus::Retry);
        assert!(
            !bridge
                .finish_remote_socket_cleanup_attempt(
                    &jid,
                    &registration,
                    reply.status == RelayRemoteResourceUnregisterStatus::Terminal,
                )
                .await
        );
        assert!(
            bridge
                .remote_socket_pending_cleanup
                .lock()
                .await
                .contains_key(&registration.registration_id),
            "retryable owner cleanup must retain exact maybe-committed evidence"
        );
    }

    #[tokio::test]
    async fn pre_publication_owner_register_failure_retains_non_routable_cleanup_evidence() {
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
        services.user_registry.kill();
        tokio::task::yield_now().await;
        let registration = remote_resource_registration(receiver_identity());

        let reply = bridge
            .register_remote_user_resource_on_owner(registration.clone())
            .await;

        assert_eq!(
            reply.status,
            RelayRemoteResourceRegistrationStatus::Unavailable
        );
        assert!(bridge.remote_owner_resources.lock().await.is_empty());
        let pending = bridge.remote_owner_pending_cleanup.lock().await;
        let (pending_jid, pending_registration) = pending
            .get(&registration.registration_id)
            .expect("maybe-committed pre-publication owner cleanup evidence");
        assert_eq!(pending_jid, &registration.jid);
        assert_eq!(pending_registration.socket_node, registration.socket_node);
        assert_eq!(
            pending_registration.user_claim_epoch,
            registration.expected_user_claim_epoch
        );
    }

    #[tokio::test]
    async fn failed_post_publication_owner_cleanup_remains_non_routable_and_retryable() {
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
        let wire_registration = remote_resource_registration(receiver_identity());
        let reply = bridge
            .register_remote_user_resource_on_owner(wire_registration.clone())
            .await;
        assert_eq!(
            reply.status,
            RelayRemoteResourceRegistrationStatus::Registered
        );
        let owner_registration = bridge
            .remote_owner_resources
            .lock()
            .await
            .get(&wire_registration.jid)
            .cloned()
            .expect("published owner registration");

        services.user_registry.kill();
        tokio::task::yield_now().await;
        bridge
            .retain_remote_owner_cleanup(&wire_registration.jid, &owner_registration)
            .await;
        assert!(
            !bridge
                .cleanup_remote_owner_resource_if_registration(
                    &wire_registration.jid,
                    owner_registration.registration_id,
                )
                .await
        );
        assert!(bridge.remote_owner_resources.lock().await.is_empty());

        let compensation = bridge
            .unregister_remote_user_resource_on_owner(RelayUnregisterRemoteUserResource {
                jid: wire_registration.jid.clone(),
                registration_id: wire_registration.registration_id,
                admission_epoch: wire_registration.admission_epoch,
                socket_generation: wire_registration.socket_generation,
                socket_node: wire_registration.socket_node,
                expected_user_owner: wire_registration.expected_user_owner,
                expected_user_claim_epoch: wire_registration.expected_user_claim_epoch,
            })
            .await;
        assert_eq!(
            compensation.status,
            RelayRemoteResourceUnregisterStatus::Retry
        );
        assert!(
            bridge
                .remote_owner_pending_cleanup
                .lock()
                .await
                .contains_key(&owner_registration.registration_id),
            "failed actor cleanup must never become map-absent Terminal"
        );
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

        let err = validate_claims(&services, &signed_envelope(&keypair))
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

        validate_claims(&services, &signed_envelope(&keypair))
            .await
            .expect("claims match origin and receiver");
    }

    #[tokio::test]
    async fn receiver_rejects_pre_fence_origin_epoch_with_same_node_id_and_claim_epoch() {
        let keypair = Keypair::generate_ed25519();
        let recovered_origin = NodeIdentity::new("origin-node", "recovered-origin-epoch");
        let services = services_with_claims(
            recovered_origin,
            receiver_identity(),
            receiver_identity(),
            keypair.public().to_peer_id().to_string(),
        )
        .await;

        let err = validate_claims(&services, &signed_envelope(&keypair))
            .await
            .expect_err("a delayed envelope from the pre-fence epoch must be rejected");

        assert_eq!(
            err,
            OrderedRelayNackReason::NotOwner {
                role: OrderedRelayClaimRole::Origin
            }
        );
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
        let mut envelope = envelope();
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
        bridge.wire(Arc::new(services));
        let mut envelope = envelope();
        envelope.payload = iq_payload();
        let envelope = sign_envelope(envelope, &keypair);

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
        bridge.wire(Arc::new(services));
        let mut envelope = envelope();
        envelope.payload = iq_payload();
        let envelope = sign_envelope(envelope, &keypair);

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
    async fn absent_exact_socket_node_retires_registered_remote_resource() {
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
                admission_epoch: RemoteResourceAdmissionEpoch(1),
                socket_node: NodeIdentity::new("missing-socket-node", "missing-socket-epoch"),
                socket_generation: RemoteResourceSocketGeneration::next(None),
                user_owner: receiver_identity(),
                user_claim_epoch: ClaimEpoch(2),
                owner: Arc::clone(&owner),
                placement: ConnectionPlacement::RemoteMirror,
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
            bridge
                .remote_owner_resources
                .lock()
                .await
                .get(&target)
                .is_none(),
            "a proven-absent exact socket incarnation must retire the owner map entry"
        );
        assert!(
            services
                .connection_registry
                .entry_if_owner(&target, &owner)
                .is_none(),
            "a proven-absent exact socket incarnation must retire the mirror"
        );
    }

    #[tokio::test]
    async fn pending_socket_registration_is_detachable_before_owner_ack() {
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
        let (outbound_tx, _outbound_rx) = mpsc::channel(1);
        let owner = services
            .connection_registry
            .register(target.clone(), outbound_tx);
        let entry = services
            .connection_registry
            .entry_if_owner(&target, &owner)
            .expect("registered socket resource");
        let mut force_detach_rx = entry
            .take_force_detach_rx()
            .expect("socket task owns force-detach receiver");
        let registration_id = RemoteResourceRegistrationId::fresh();
        let admission_epoch = RemoteResourceAdmissionEpoch(1);
        let socket_generation = RemoteResourceSocketGeneration::next(None);
        bridge
            .publish_pending_remote_socket_registration(
                &target,
                RemoteSocketRegistration {
                    registration_id,
                    admission_epoch,
                    socket_generation,
                    socket_node: receiver_identity(),
                    owner,
                    user_owner: receiver_identity(),
                    user_claim_epoch: ClaimEpoch(2),
                },
            )
            .await;

        // No owner registration ACK has arrived. A concurrent owner self-fence
        // must nevertheless find this pending registration and reach the
        // physical socket instead of concluding that it is already gone.
        let detach = tokio::spawn({
            let bridge = Arc::clone(&bridge);
            let target = target.clone();
            async move {
                bridge
                    .force_detach_remote_user_resource_on_socket(
                        RelayForceDetachRemoteUserResource {
                            requester_bare_jid: target.to_bare(),
                            jid: target,
                            registration_id,
                            admission_epoch,
                            socket_generation,
                            socket_node: receiver_identity(),
                            expected_user_owner: receiver_identity(),
                            expected_user_claim_epoch: ClaimEpoch(2),
                            reason: ForceDetachReason::NodeSelfFenced,
                        },
                    )
                    .await
            }
        });

        let request = force_detach_rx.recv().await.expect("force-detach request");
        assert_eq!(request.reason, ForceDetachReason::NodeSelfFenced);
        request
            .ack
            .send(ForceDetachOutcome::NotPersisted)
            .expect("socket detach ack accepted");
        let reply = detach.await.expect("relay detach task");
        assert_eq!(reply.outcome, ForceDetachOutcome::NotPersisted);
        assert_eq!(reply.status, RelayRemoteResourceForceDetachStatus::Detached);
    }

    #[tokio::test]
    async fn hard_retired_local_socket_preserves_owner_mirror_cleanup_evidence() {
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let jid = target_full();
        let owner = Arc::new(AtomicBool::new(true));
        let registration = RemoteSocketRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(1),
            socket_generation: RemoteResourceSocketGeneration(1),
            socket_node: origin_identity(),
            owner: Arc::clone(&owner),
            user_owner: receiver_identity(),
            user_claim_epoch: ClaimEpoch(2),
        };
        assert!(
            bridge
                .reserve_remote_socket_cleanup_capacity(&jid, &registration)
                .await
        );
        assert!(
            bridge
                .publish_pending_remote_socket_registration(&jid, registration.clone())
                .await
        );

        // UserLocalClaims invokes this after hard-aborting a non-cooperative
        // physical socket. The local routing record is dead, but the remote
        // owner mirror may still exist and therefore still needs exact
        // compensation evidence.
        bridge
            .forget_remote_resource_state_if_owner(
                &jid,
                &owner,
                waddle_xmpp::registry::ConnectionPlacement::LocalSocket,
            )
            .await;

        assert!(bridge.remote_socket_resources.lock().await.is_empty());
        assert!(
            bridge
                .remote_socket_pending_cleanup
                .lock()
                .await
                .contains_key(&registration.registration_id),
            "physical retirement must not imply owner-mirror cleanup"
        );
    }

    #[tokio::test]
    async fn delayed_old_owner_frame_is_rejected_after_claim_regrant() {
        let user_owner = receiver_identity();
        let (services, store) = services_with_claims_and_store(
            origin_identity(),
            user_owner.clone(),
            origin_identity(),
            test_peer_id(),
            Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new()),
        )
        .await;
        let services = Arc::new(services);
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));
        let target = target_full();
        let (outbound_tx, mut outbound_rx) = mpsc::channel(1);
        let socket_owner = services
            .connection_registry
            .register(target.clone(), outbound_tx);
        let entry = services
            .connection_registry
            .entry_if_owner(&target, &socket_owner)
            .expect("registered physical socket");
        let mut force_detach_rx = entry
            .take_force_detach_rx()
            .expect("socket owns force-detach receiver");
        let registration = RemoteSocketRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(1),
            socket_generation: RemoteResourceSocketGeneration::next(None),
            socket_node: origin_identity(),
            owner: socket_owner,
            user_owner: user_owner.clone(),
            user_claim_epoch: ClaimEpoch(2),
        };
        bridge
            .publish_pending_remote_socket_registration(&target, registration.clone())
            .await;
        let current_epoch = steal_and_regrant_target_claim(&store, &user_owner).await;
        assert_ne!(current_epoch, registration.user_claim_epoch);

        let delivery = tokio::spawn({
            let bridge = Arc::clone(&bridge);
            let target = target.clone();
            let registration = registration.clone();
            async move {
                bridge
                    .deliver_remote_resource_frame_on_socket(RelayDeliverRemoteResourceFrame {
                        frame: RemoteResourceOutboundFrame {
                            jid: target.clone(),
                            registration_id: registration.registration_id,
                            admission_epoch: registration.admission_epoch,
                            socket_generation: registration.socket_generation,
                            socket_node: registration.socket_node,
                            expected_user_owner: registration.user_owner,
                            expected_user_claim_epoch: registration.user_claim_epoch,
                            stanza: RemoteStanza(Stanza::Message(Message::new(Some(
                                jid::Jid::from(target),
                            )))),
                            kind: DeliveryKind::PeerStanza,
                        },
                    })
                    .await
            }
        });

        assert!(
            outbound_rx.try_recv().is_err(),
            "stale frame must not reach socket"
        );
        let request = tokio::time::timeout(Duration::from_secs(1), force_detach_rx.recv())
            .await
            .expect("stale-registration detach must be prompt")
            .expect("stale registration invalidates the physical socket");
        assert_eq!(request.reason, ForceDetachReason::RemoteStateInvalidated);
        request
            .ack
            .send(ForceDetachOutcome::NotPersisted)
            .expect("cooperative stale detach accepts ack");
        let reply = delivery.await.expect("stale frame handler");
        assert_eq!(
            reply.status,
            RelayRemoteResourceFrameStatus::StaleRegistration
        );
        assert!(bridge.remote_socket_resources.lock().await.is_empty());
    }

    #[tokio::test]
    async fn delayed_pre_fence_socket_epoch_frame_cannot_reach_recovered_process() {
        let services = Arc::new(
            services_with_claims(
                origin_identity(),
                receiver_identity(),
                origin_identity(),
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
        let (outbound_tx, mut outbound_rx) = mpsc::channel(1);
        let socket_owner = services
            .connection_registry
            .register(target.clone(), outbound_tx);
        let entry = services
            .connection_registry
            .entry_if_owner(&target, &socket_owner)
            .expect("registered physical socket");
        let mut force_detach_rx = entry
            .take_force_detach_rx()
            .expect("socket owns force-detach receiver");
        let registration = RemoteSocketRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(1),
            socket_generation: RemoteResourceSocketGeneration::next(None),
            socket_node: origin_identity(),
            owner: socket_owner,
            user_owner: receiver_identity(),
            user_claim_epoch: ClaimEpoch(2),
        };
        assert!(
            bridge
                .publish_pending_remote_socket_registration(&target, registration.clone())
                .await
        );
        services.node_identity.set(NodeIdentity::new(
            registration.socket_node.node_id.clone(),
            "recovered-socket-epoch",
        ));

        let delivery = tokio::spawn({
            let bridge = Arc::clone(&bridge);
            let target = target.clone();
            let registration = registration.clone();
            async move {
                bridge
                    .deliver_remote_resource_frame_on_socket(RelayDeliverRemoteResourceFrame {
                        frame: RemoteResourceOutboundFrame {
                            jid: target.clone(),
                            registration_id: registration.registration_id,
                            admission_epoch: registration.admission_epoch,
                            socket_generation: registration.socket_generation,
                            socket_node: registration.socket_node,
                            expected_user_owner: registration.user_owner,
                            expected_user_claim_epoch: registration.user_claim_epoch,
                            stanza: RemoteStanza(Stanza::Message(Message::new(Some(
                                jid::Jid::from(target),
                            )))),
                            kind: DeliveryKind::PeerStanza,
                        },
                    })
                    .await
            }
        });
        let request = tokio::time::timeout(Duration::from_secs(1), force_detach_rx.recv())
            .await
            .expect("stale socket incarnation is retired promptly")
            .expect("stale socket receives terminal invalidation");
        assert_eq!(request.reason, ForceDetachReason::RemoteStateInvalidated);
        request
            .ack
            .send(ForceDetachOutcome::NotPersisted)
            .expect("cooperative stale detach accepts ack");
        let reply = delivery.await.expect("stale socket frame handler");
        assert_eq!(
            reply.status,
            RelayRemoteResourceFrameStatus::StaleRegistration
        );
        assert!(outbound_rx.try_recv().is_err());
        assert!(!services.connection_registry.is_connected(&target));
    }

    #[tokio::test]
    async fn self_fence_during_frame_claim_validation_prevents_post_fence_delivery() {
        let (mut services, claim_store, _) = services_with_claims_and_controls(
            origin_identity(),
            receiver_identity(),
            origin_identity(),
            test_peer_id(),
            Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new()),
        )
        .await;
        let delayed_claim_store = DelayedCurrentClaimStore::new(claim_store);
        services.claim_store = delayed_claim_store.clone();
        let services = Arc::new(services);
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));
        let target = target_full();
        let (outbound_tx, mut outbound_rx) = mpsc::channel(1);
        let socket_owner = services
            .connection_registry
            .register(target.clone(), outbound_tx);
        let registration = RemoteSocketRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(1),
            socket_generation: RemoteResourceSocketGeneration::next(None),
            socket_node: origin_identity(),
            owner: socket_owner,
            user_owner: receiver_identity(),
            user_claim_epoch: ClaimEpoch(2),
        };
        assert!(
            bridge
                .publish_pending_remote_socket_registration(&target, registration.clone())
                .await
        );
        delayed_claim_store.delay_next_read();

        let delivery = tokio::spawn({
            let bridge = Arc::clone(&bridge);
            let target = target.clone();
            let registration = registration.clone();
            async move {
                bridge
                    .deliver_remote_resource_frame_on_socket(RelayDeliverRemoteResourceFrame {
                        frame: RemoteResourceOutboundFrame {
                            jid: target.clone(),
                            registration_id: registration.registration_id,
                            admission_epoch: registration.admission_epoch,
                            socket_generation: registration.socket_generation,
                            socket_node: registration.socket_node,
                            expected_user_owner: registration.user_owner,
                            expected_user_claim_epoch: registration.user_claim_epoch,
                            stanza: RemoteStanza(Stanza::Message(Message::new(Some(
                                jid::Jid::from(target),
                            )))),
                            kind: DeliveryKind::PeerStanza,
                        },
                    })
                    .await
            }
        });
        tokio::time::timeout(
            Duration::from_secs(1),
            delayed_claim_store.wait_until_read_started(),
        )
        .await
        .expect("frame validation reaches delayed claim read");
        bridge.clear_remote_resource_state_on_self_fence().await;
        delayed_claim_store.release_read();

        let reply = delivery.await.expect("delayed frame handler");
        assert_eq!(
            reply.status,
            RelayRemoteResourceFrameStatus::StaleRegistration
        );
        assert!(
            outbound_rx.try_recv().is_err(),
            "a claim result read before the fence cannot authorize post-fence delivery"
        );
    }

    #[tokio::test]
    async fn delayed_old_frame_cannot_deliver_or_detach_a_superseding_registration() {
        let user_owner = receiver_identity();
        let (mut services, claim_store, _) = services_with_claims_and_controls(
            origin_identity(),
            user_owner.clone(),
            origin_identity(),
            test_peer_id(),
            Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new()),
        )
        .await;
        let delayed_claim_store = DelayedCurrentClaimStore::new(Arc::clone(&claim_store));
        services.claim_store = delayed_claim_store.clone();
        let services = Arc::new(services);
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));
        let target = target_full();
        let (outbound_tx, mut outbound_rx) = mpsc::channel(1);
        let socket_owner = services
            .connection_registry
            .register(target.clone(), outbound_tx);
        let entry = services
            .connection_registry
            .entry_if_owner(&target, &socket_owner)
            .expect("registered physical socket");
        let mut force_detach_rx = entry
            .take_force_detach_rx()
            .expect("socket owns force-detach receiver");
        let older = RemoteSocketRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(1),
            socket_generation: RemoteResourceSocketGeneration(1),
            socket_node: origin_identity(),
            owner: Arc::clone(&socket_owner),
            user_owner: user_owner.clone(),
            user_claim_epoch: ClaimEpoch(2),
        };
        assert!(
            bridge
                .publish_pending_remote_socket_registration(&target, older.clone())
                .await
        );
        delayed_claim_store.delay_next_read();

        let delivery = tokio::spawn({
            let bridge = Arc::clone(&bridge);
            let target = target.clone();
            let older = older.clone();
            async move {
                bridge
                    .deliver_remote_resource_frame_on_socket(RelayDeliverRemoteResourceFrame {
                        frame: RemoteResourceOutboundFrame {
                            jid: target.clone(),
                            registration_id: older.registration_id,
                            admission_epoch: older.admission_epoch,
                            socket_generation: older.socket_generation,
                            socket_node: older.socket_node,
                            expected_user_owner: older.user_owner,
                            expected_user_claim_epoch: older.user_claim_epoch,
                            stanza: RemoteStanza(Stanza::Message(Message::new(Some(
                                jid::Jid::from(target),
                            )))),
                            kind: DeliveryKind::PeerStanza,
                        },
                    })
                    .await
            }
        });
        tokio::time::timeout(
            Duration::from_secs(1),
            delayed_claim_store.wait_until_read_started(),
        )
        .await
        .expect("old frame reaches delayed claim read");

        let current_epoch = steal_and_regrant_target_claim(&claim_store, &user_owner).await;
        let newer = RemoteSocketRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(2),
            socket_generation: RemoteResourceSocketGeneration(2),
            socket_node: origin_identity(),
            owner: socket_owner,
            user_owner,
            user_claim_epoch: current_epoch,
        };
        assert!(
            bridge
                .publish_pending_remote_socket_registration(&target, newer.clone())
                .await
        );
        delayed_claim_store.release_read();

        let reply = delivery.await.expect("delayed old frame handler");
        assert_eq!(
            reply.status,
            RelayRemoteResourceFrameStatus::StaleRegistration
        );
        assert!(outbound_rx.try_recv().is_err());
        assert!(force_detach_rx.try_recv().is_err());
        assert!(services.connection_registry.is_connected(&target));
        assert!(
            bridge
                .remote_socket_registration_is_current(&target, &newer)
                .await,
            "stale invalidation must not remove or hard-detach a superseding registration"
        );
    }

    #[tokio::test]
    async fn self_fence_during_force_detach_claim_validation_prevents_post_fence_control_effect() {
        let (mut services, claim_store, _) = services_with_claims_and_controls(
            origin_identity(),
            receiver_identity(),
            origin_identity(),
            test_peer_id(),
            Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new()),
        )
        .await;
        let delayed_claim_store = DelayedCurrentClaimStore::new(claim_store);
        services.claim_store = delayed_claim_store.clone();
        let services = Arc::new(services);
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));
        let target = target_full();
        let (outbound_tx, _outbound_rx) = mpsc::channel(1);
        let socket_owner = services
            .connection_registry
            .register(target.clone(), outbound_tx);
        let entry = services
            .connection_registry
            .entry_if_owner(&target, &socket_owner)
            .expect("registered physical socket");
        let mut force_detach_rx = entry
            .take_force_detach_rx()
            .expect("socket owns force-detach receiver");
        let registration = RemoteSocketRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(1),
            socket_generation: RemoteResourceSocketGeneration::next(None),
            socket_node: origin_identity(),
            owner: socket_owner,
            user_owner: receiver_identity(),
            user_claim_epoch: ClaimEpoch(2),
        };
        assert!(
            bridge
                .publish_pending_remote_socket_registration(&target, registration.clone())
                .await
        );
        delayed_claim_store.delay_next_read();

        let detach = tokio::spawn({
            let bridge = Arc::clone(&bridge);
            let target = target.clone();
            let registration = registration.clone();
            async move {
                bridge
                    .force_detach_remote_user_resource_on_socket(
                        RelayForceDetachRemoteUserResource {
                            jid: target.clone(),
                            registration_id: registration.registration_id,
                            admission_epoch: registration.admission_epoch,
                            socket_generation: registration.socket_generation,
                            socket_node: registration.socket_node,
                            expected_user_owner: registration.user_owner,
                            expected_user_claim_epoch: registration.user_claim_epoch,
                            requester_bare_jid: target.to_bare(),
                            reason: ForceDetachReason::ResourceReplaced,
                        },
                    )
                    .await
            }
        });
        tokio::time::timeout(
            Duration::from_secs(1),
            delayed_claim_store.wait_until_read_started(),
        )
        .await
        .expect("force-detach validation reaches delayed claim read");
        bridge.clear_remote_resource_state_on_self_fence().await;
        delayed_claim_store.release_read();

        let reply = detach.await.expect("delayed force-detach handler");
        assert_eq!(reply.outcome, ForceDetachOutcome::NotPersisted);
        assert_eq!(
            reply.status,
            RelayRemoteResourceForceDetachStatus::Invalidated
        );
        assert!(
            force_detach_rx.try_recv().is_err(),
            "a claim result read before the fence cannot enqueue post-fence control"
        );
    }

    #[tokio::test]
    async fn stale_socket_invalidation_hard_retires_full_and_wedged_control_channels() {
        let services = Arc::new(
            services_with_claims(
                origin_identity(),
                receiver_identity(),
                origin_identity(),
                test_peer_id(),
            )
            .await,
        );
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));

        for (resource, fill_channel) in [("full", true), ("wedged", false)] {
            let jid: jid::FullJid = format!("juliet@example.test/{resource}")
                .parse()
                .expect("full jid");
            let (outbound_tx, _outbound_rx) = mpsc::channel(1);
            let owner = services
                .connection_registry
                .register(jid.clone(), outbound_tx);
            let entry = services
                .connection_registry
                .entry_if_owner(&jid, &owner)
                .expect("registered physical socket");
            let held_rx = entry
                .take_force_detach_rx()
                .expect("socket owns force-detach receiver");
            if fill_channel {
                loop {
                    let (ack, _ack_rx) = tokio::sync::oneshot::channel();
                    if entry
                        .force_detach_sender()
                        .try_send(ForceDetachRequest {
                            requester_bare_jid: jid.to_bare(),
                            reason: ForceDetachReason::RemoteStateInvalidated,
                            ack,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }

            let (abort, abort_registration) = futures::future::AbortHandle::new_pair();
            let terminated = CancellationToken::new();
            assert!(entry.install_retirement_handle(
                waddle_xmpp::registry::ConnectionRetirementHandle::new(abort, terminated.clone(),),
            ));
            let task_terminated = terminated.clone();
            let socket_task = tokio::spawn(async move {
                let _ = futures::future::Abortable::new(
                    std::future::pending::<()>(),
                    abort_registration,
                )
                .await;
                task_terminated.cancel();
            });
            let registration = RemoteSocketRegistration {
                registration_id: RemoteResourceRegistrationId::fresh(),
                admission_epoch: RemoteResourceAdmissionEpoch(1),
                socket_generation: bridge
                    .next_remote_socket_generation()
                    .expect("socket generation"),
                socket_node: origin_identity(),
                owner: owner.clone(),
                user_owner: receiver_identity(),
                user_claim_epoch: ClaimEpoch(2),
            };
            assert!(
                bridge
                    .publish_pending_remote_socket_registration(&jid, registration.clone())
                    .await
            );

            bridge
                .detach_stale_remote_socket_resource(&jid, &registration)
                .await;

            tokio::time::timeout(Duration::from_secs(1), socket_task)
                .await
                .expect("hard-retired socket task terminates")
                .expect("socket task join");
            assert!(terminated.is_cancelled());
            assert!(
                services
                    .connection_registry
                    .entry_if_owner(&jid, &owner)
                    .is_none(),
                "terminal invalidation owner-gates registry cleanup"
            );
            assert!(
                !bridge
                    .remote_socket_resources
                    .lock()
                    .await
                    .contains_key(&jid),
                "terminal invalidation removes routing metadata"
            );
            drop(held_rx);
        }
    }

    #[tokio::test]
    async fn delayed_old_owner_force_detach_is_refused_after_claim_regrant() {
        let user_owner = receiver_identity();
        let (services, store) = services_with_claims_and_store(
            origin_identity(),
            user_owner.clone(),
            origin_identity(),
            test_peer_id(),
            Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new()),
        )
        .await;
        let services = Arc::new(services);
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));
        let target = target_full();
        let (outbound_tx, _outbound_rx) = mpsc::channel(1);
        let socket_owner = services
            .connection_registry
            .register(target.clone(), outbound_tx);
        let entry = services
            .connection_registry
            .entry_if_owner(&target, &socket_owner)
            .expect("registered physical socket");
        let mut force_detach_rx = entry
            .take_force_detach_rx()
            .expect("socket owns force-detach receiver");
        let registration = RemoteSocketRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(1),
            socket_generation: RemoteResourceSocketGeneration::next(None),
            socket_node: origin_identity(),
            owner: socket_owner,
            user_owner: user_owner.clone(),
            user_claim_epoch: ClaimEpoch(2),
        };
        bridge
            .publish_pending_remote_socket_registration(&target, registration.clone())
            .await;
        let current_epoch = steal_and_regrant_target_claim(&store, &user_owner).await;
        assert_ne!(current_epoch, registration.user_claim_epoch);

        let detach = tokio::spawn({
            let bridge = Arc::clone(&bridge);
            let target = target.clone();
            let registration = registration.clone();
            async move {
                bridge
                    .force_detach_remote_user_resource_on_socket(
                        RelayForceDetachRemoteUserResource {
                            jid: target.clone(),
                            registration_id: registration.registration_id,
                            admission_epoch: registration.admission_epoch,
                            socket_generation: registration.socket_generation,
                            socket_node: registration.socket_node,
                            expected_user_owner: registration.user_owner,
                            expected_user_claim_epoch: registration.user_claim_epoch,
                            requester_bare_jid: target.to_bare(),
                            reason: ForceDetachReason::ResourceReplaced,
                        },
                    )
                    .await
            }
        });

        let request = tokio::time::timeout(Duration::from_secs(1), force_detach_rx.recv())
            .await
            .expect("stale-registration detach must be prompt")
            .expect("stale registration invalidates the physical socket");
        assert_eq!(request.reason, ForceDetachReason::ResourceReplaced);
        request
            .ack
            .send(ForceDetachOutcome::NotPersisted)
            .expect("cooperative stale detach accepts ack");
        let reply = detach.await.expect("stale force-detach handler");
        assert_eq!(reply.outcome, ForceDetachOutcome::NotPersisted);
        assert_eq!(
            reply.status,
            RelayRemoteResourceForceDetachStatus::Invalidated
        );
        assert!(bridge.remote_socket_resources.lock().await.is_empty());
    }

    #[tokio::test]
    async fn socket_generation_remains_monotonic_across_self_fence_cleanup() {
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let first = bridge
            .next_remote_socket_generation()
            .expect("first generation");

        bridge.clear_remote_resource_state_on_self_fence().await;

        let recovered = bridge
            .next_remote_socket_generation()
            .expect("post-fence generation");
        assert!(
            recovered > first,
            "a stable socket-node relay address must never reuse a pre-fence generation"
        );
    }

    #[tokio::test]
    async fn self_fence_moves_unretired_owner_mirrors_to_non_routable_cleanup_evidence() {
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let jid = target_full();
        let registration = RemoteOwnerRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(1),
            socket_node: origin_identity(),
            socket_generation: RemoteResourceSocketGeneration(1),
            user_owner: receiver_identity(),
            user_claim_epoch: ClaimEpoch(2),
            owner: Arc::new(AtomicBool::new(true)),
            placement: ConnectionPlacement::RemoteMirror,
        };
        bridge
            .remote_owner_resources
            .lock()
            .await
            .insert(jid.clone(), registration.clone());

        bridge.clear_remote_resource_state_on_self_fence().await;

        assert!(bridge.remote_owner_resources.lock().await.is_empty());
        let pending = bridge.remote_owner_pending_cleanup.lock().await;
        let (pending_jid, pending_registration) = pending
            .get(&registration.registration_id)
            .expect("self-fenced owner cleanup evidence");
        assert_eq!(pending_jid, &jid);
        assert!(remote_owner_registration_matches(
            pending_registration,
            &registration
        ));
    }

    #[tokio::test]
    async fn reverse_pending_publication_cannot_replace_newer_socket_generation() {
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let jid = target_full();
        let newer = RemoteSocketRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(2),
            socket_generation: RemoteResourceSocketGeneration(2),
            socket_node: origin_identity(),
            owner: Arc::new(AtomicBool::new(true)),
            user_owner: receiver_identity(),
            user_claim_epoch: ClaimEpoch(7),
        };
        let older = RemoteSocketRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(1),
            socket_generation: RemoteResourceSocketGeneration(1),
            socket_node: origin_identity(),
            owner: Arc::new(AtomicBool::new(true)),
            user_owner: receiver_identity(),
            user_claim_epoch: ClaimEpoch(6),
        };

        assert!(
            bridge
                .publish_pending_remote_socket_registration(&jid, newer.clone())
                .await
        );
        assert!(
            !bridge
                .publish_pending_remote_socket_registration(&jid, older)
                .await,
            "a delayed older publication must lose"
        );
        assert!(
            bridge
                .remote_socket_registration_is_current(&jid, &newer)
                .await,
            "the exact newer registration must survive reverse publication"
        );
    }

    #[tokio::test]
    async fn uncertain_old_compensation_survives_overlapping_newer_publication_failure() {
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let jid = target_full();
        let owner = Arc::new(AtomicBool::new(true));
        let older = RemoteSocketRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(1),
            socket_generation: RemoteResourceSocketGeneration(1),
            socket_node: origin_identity(),
            owner: Arc::clone(&owner),
            user_owner: receiver_identity(),
            user_claim_epoch: ClaimEpoch(6),
        };
        let newer = RemoteSocketRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(2),
            socket_generation: RemoteResourceSocketGeneration(2),
            socket_node: origin_identity(),
            owner,
            user_owner: receiver_identity(),
            user_claim_epoch: ClaimEpoch(7),
        };

        assert!(
            bridge
                .publish_pending_remote_socket_registration(&jid, older.clone())
                .await
        );
        assert!(
            bridge
                .publish_pending_remote_socket_registration(&jid, newer.clone())
                .await
        );

        // Model the older register committing while its reply is lost. Its
        // compensating unregister is also uncertain, after a newer attempt
        // has already replaced the JID-keyed publication slot.
        bridge.retain_remote_socket_cleanup(&jid, &older).await;

        // The newer attempt then fails before it can establish an owner
        // mirror. Removing it must not discard the older exact compensation
        // evidence just because both attempts used the same full JID.
        bridge
            .remove_remote_socket_registration_if_current(&jid, &newer)
            .await;
        assert!(bridge.remote_socket_resources.lock().await.is_empty());
        let pending = bridge.remote_socket_pending_cleanup.lock().await;
        let (pending_jid, pending_registration) = pending
            .get(&older.registration_id)
            .expect("uncertain older compensation metadata");
        assert_eq!(pending_jid, &jid);
        assert!(remote_socket_registration_matches(
            pending_registration,
            &older
        ));
        assert!(!pending.contains_key(&newer.registration_id));
    }

    #[tokio::test]
    async fn confirmed_opportunistic_cleanup_removes_only_the_exact_pending_registration() {
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let jid = target_full();
        let owner = Arc::new(AtomicBool::new(true));
        let obsolete = RemoteSocketRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(1),
            socket_generation: RemoteResourceSocketGeneration(1),
            socket_node: origin_identity(),
            owner: Arc::clone(&owner),
            user_owner: receiver_identity(),
            user_claim_epoch: ClaimEpoch(6),
        };
        let current = RemoteSocketRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(2),
            socket_generation: RemoteResourceSocketGeneration(2),
            socket_node: origin_identity(),
            owner,
            user_owner: receiver_identity(),
            user_claim_epoch: ClaimEpoch(7),
        };
        assert!(
            bridge
                .reserve_remote_socket_cleanup_capacity(&jid, &obsolete)
                .await
        );
        assert!(
            bridge
                .publish_pending_remote_socket_registration(&jid, current.clone())
                .await
        );

        assert!(
            !bridge
                .finish_remote_socket_cleanup_attempt(&jid, &obsolete, false)
                .await,
            "an uncertain retry must retain exact compensation metadata"
        );
        assert!(bridge
            .remote_socket_pending_cleanup
            .lock()
            .await
            .contains_key(&obsolete.registration_id));
        assert!(
            bridge
                .finish_remote_socket_cleanup_attempt(&jid, &obsolete, true)
                .await,
            "an acknowledged unregister retires the obsolete exact attempt"
        );
        assert!(bridge.remote_socket_pending_cleanup.lock().await.is_empty());
        assert!(
            bridge
                .remote_socket_registration_is_current(&jid, &current)
                .await,
            "cleanup for generation N must not remove current generation N+1"
        );
    }

    #[tokio::test]
    async fn pending_cleanup_budget_bounds_reconnect_storms_per_full_jid() {
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let jid = target_full();
        let other_jid: jid::FullJid = "juliet@example.test/tablet".parse().expect("full JID");
        let owner = Arc::new(AtomicBool::new(true));
        let mut pending = Vec::new();
        for generation in 1..=MAX_PENDING_REMOTE_SOCKET_CLEANUPS_PER_JID {
            let registration = RemoteSocketRegistration {
                registration_id: RemoteResourceRegistrationId::fresh(),
                admission_epoch: RemoteResourceAdmissionEpoch(generation as i64),
                socket_generation: RemoteResourceSocketGeneration(generation as u64),
                socket_node: origin_identity(),
                owner: Arc::clone(&owner),
                user_owner: receiver_identity(),
                user_claim_epoch: ClaimEpoch(generation as i64),
            };
            assert!(
                bridge
                    .reserve_remote_socket_cleanup_capacity(&jid, &registration)
                    .await
            );
            pending.push(registration);
        }
        let unrelated = RemoteSocketRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(1),
            socket_generation: RemoteResourceSocketGeneration(1),
            socket_node: origin_identity(),
            owner,
            user_owner: receiver_identity(),
            user_claim_epoch: ClaimEpoch(1),
        };
        assert!(
            bridge
                .reserve_remote_socket_cleanup_capacity(&other_jid, &unrelated)
                .await
        );
        let overflow = RemoteSocketRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(9),
            socket_generation: RemoteResourceSocketGeneration(9),
            socket_node: origin_identity(),
            owner: Arc::new(AtomicBool::new(true)),
            user_owner: receiver_identity(),
            user_claim_epoch: ClaimEpoch(9),
        };

        assert!(
            !bridge
                .reserve_remote_socket_cleanup_capacity(&jid, &overflow)
                .await,
            "the next same-JID registration must fail closed at the uncertainty cap"
        );
        let selected = bridge.pending_remote_socket_cleanups_for_jid(&jid).await;
        assert_eq!(selected.len(), MAX_PENDING_REMOTE_SOCKET_CLEANUPS_PER_JID);
        assert!(selected
            .iter()
            .all(|registration| registration.registration_id != unrelated.registration_id));

        assert!(
            bridge
                .finish_remote_socket_cleanup_attempt(&jid, &pending[0], true)
                .await
        );
        assert!(
            bridge
                .reserve_remote_socket_cleanup_capacity(&jid, &overflow)
                .await,
            "one confirmed retry re-opens exactly one bounded registration slot"
        );
    }

    #[tokio::test]
    async fn global_cleanup_capacity_recovers_from_inactive_entries_without_touching_live_ones() {
        let services = Arc::new(
            services_with_claims(
                origin_identity(),
                receiver_identity(),
                origin_identity(),
                test_peer_id(),
            )
            .await,
        );
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(services);
        let owner = Arc::new(AtomicBool::new(true));
        let mut first_cleanup = None;
        let mut active_registration_id = None;
        for index in 0..MAX_REMOTE_SOCKET_CLEANUP_REGISTRATIONS {
            let jid: jid::FullJid = format!("user{index}@example.test/resource")
                .parse()
                .expect("generated full JID");
            let registration = RemoteSocketRegistration {
                registration_id: RemoteResourceRegistrationId::fresh(),
                admission_epoch: RemoteResourceAdmissionEpoch(index as i64 + 1),
                socket_generation: RemoteResourceSocketGeneration(1),
                socket_node: origin_identity(),
                owner: Arc::clone(&owner),
                user_owner: receiver_identity(),
                user_claim_epoch: ClaimEpoch(1),
            };
            assert!(
                bridge
                    .reserve_remote_socket_cleanup_capacity(&jid, &registration)
                    .await
            );
            if index == 0 {
                assert!(
                    bridge
                        .publish_pending_remote_socket_registration(&jid, registration.clone())
                        .await
                );
                active_registration_id = Some(registration.registration_id);
            } else if first_cleanup.is_none() {
                first_cleanup = Some((jid, registration));
            }
        }
        let overflow_jid: jid::FullJid = "overflow@example.test/resource"
            .parse()
            .expect("overflow full JID");
        let overflow = RemoteSocketRegistration {
            registration_id: RemoteResourceRegistrationId::fresh(),
            admission_epoch: RemoteResourceAdmissionEpoch(1),
            socket_generation: RemoteResourceSocketGeneration(1),
            socket_node: origin_identity(),
            owner,
            user_owner: receiver_identity(),
            user_claim_epoch: ClaimEpoch(1),
        };
        assert!(
            !bridge
                .reserve_remote_socket_cleanup_capacity(&overflow_jid, &overflow)
                .await,
            "the global hard cap must reject a novel full JID without evicting uncertain evidence"
        );
        assert_eq!(
            bridge.remote_socket_pending_cleanup.lock().await.len(),
            MAX_REMOTE_SOCKET_CLEANUP_REGISTRATIONS
        );
        let retry_batch = bridge
            .inactive_remote_socket_cleanups(REMOTE_SOCKET_GLOBAL_CLEANUP_RETRY_BATCH)
            .await;
        assert_eq!(retry_batch.len(), REMOTE_SOCKET_GLOBAL_CLEANUP_RETRY_BATCH);
        assert!(retry_batch.iter().all(|(_, registration)| {
            Some(registration.registration_id) != active_registration_id
        }));

        // A terminal result from that bounded opportunistic batch releases
        // capacity; the active reservation remains protected from cleanup.
        let (first_jid, first_registration) = first_cleanup.expect("first cleanup registration");
        assert!(
            bridge
                .finish_remote_socket_cleanup_attempt(&first_jid, &first_registration, true)
                .await
        );
        assert!(
            bridge
                .reserve_remote_socket_cleanup_capacity(&overflow_jid, &overflow)
                .await,
            "a confirmed exact cleanup re-opens one global slot"
        );
        assert_eq!(
            bridge.remote_socket_pending_cleanup.lock().await.len(),
            MAX_REMOTE_SOCKET_CLEANUP_REGISTRATIONS
        );
        assert!(bridge
            .remote_socket_pending_cleanup
            .lock()
            .await
            .contains_key(&active_registration_id.expect("active registration")));
    }

    #[tokio::test]
    async fn exhausted_fence_generation_permanently_refuses_remote_registrations_without_panicking()
    {
        let services = Arc::new(
            services_with_claims(
                origin_identity(),
                receiver_identity(),
                origin_identity(),
                test_peer_id(),
            )
            .await,
        );
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::clone(&services));
        bridge
            .remote_resource_fence_generation
            .store(u64::MAX, Ordering::SeqCst);

        bridge.clear_remote_resource_state_on_self_fence().await;
        bridge.clear_remote_resource_state_on_self_fence().await;

        assert_eq!(
            bridge
                .remote_resource_fence_generation
                .load(Ordering::SeqCst),
            u64::MAX
        );
        assert!(
            !bridge.remote_resource_registration_allowed(&services, u64::MAX),
            "generation exhaustion must fail closed forever"
        );
    }

    #[tokio::test]
    async fn self_fence_keeps_live_per_jid_registration_lock_until_guard_retires() {
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let jid = target_full();
        let lock = bridge
            .lock_for_remote_owner_registration(&jid)
            .await
            .expect("registration lock");
        let guard = lock.lock().await;

        bridge.clear_remote_resource_state_on_self_fence().await;
        let overlapping = bridge
            .lock_for_remote_owner_registration(&jid)
            .await
            .expect("overlapping registration lock");
        assert!(
            Arc::ptr_eq(&lock, &overlapping),
            "self-fence must not create a second lock while an old guard lives"
        );

        drop(guard);
        drop(overlapping);
        bridge
            .remove_remote_owner_registration_lock_if_unused(&jid, &lock)
            .await;
        assert!(
            !bridge
                .remote_owner_registration_locks
                .lock()
                .await
                .contains_key(&jid),
            "strong-count cleanup retires the lock after overlap ends"
        );
    }

    #[tokio::test]
    async fn self_fence_generation_increment_waits_for_the_socket_effect_critical_section() {
        let bridge = OrderedRelayDeliveryBridge::new(
            CancellationToken::new(),
            &ClusteringMessagingConfig::default(),
        );
        let effect_guard = bridge.remote_socket_resources.lock().await;
        let clear_started = Arc::new(tokio::sync::Notify::new());
        let clear = tokio::spawn({
            let bridge = Arc::clone(&bridge);
            let clear_started = Arc::clone(&clear_started);
            async move {
                clear_started.notify_one();
                bridge.clear_remote_resource_state_on_self_fence().await;
            }
        });
        clear_started.notified().await;
        tokio::task::yield_now().await;

        assert_eq!(
            bridge
                .remote_resource_fence_generation
                .load(Ordering::SeqCst),
            0,
            "self-fence cannot linearize while a validated socket effect holds the shared map guard"
        );

        // Frame delivery and force-detach perform their synchronous enqueue
        // while holding this exact guard. Releasing it is therefore the
        // latest point at which an old-generation effect can linearize.
        drop(effect_guard);
        clear.await.expect("self-fence clear task");
        assert_eq!(
            bridge
                .remote_resource_fence_generation
                .load(Ordering::SeqCst),
            1,
            "the fence increments immediately after the old effect critical section"
        );
    }

    #[tokio::test]
    async fn discovery_not_found_retains_old_socket_mirror() {
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
            admission_epoch: RemoteResourceAdmissionEpoch(1),
            socket_node: NodeIdentity::new("missing-old-socket-node", "missing-old-socket-epoch"),
            socket_generation: old_generation,
            user_owner: receiver_identity(),
            user_claim_epoch: ClaimEpoch(2),
            owner: Arc::clone(&old_owner),
            placement: ConnectionPlacement::RemoteMirror,
        };

        assert!(
            !bridge
                .finish_remote_owner_registration_retire(
                    &services,
                    &target,
                    &registration,
                    Err(RelayAskError::NotFound {
                        node_id: NodeId::new(registration.socket_node.node_id.clone()),
                    }),
                )
                .await,
            "discovery NotFound is transient and cannot prove the exact socket registration stale"
        );
        assert!(
            services
                .connection_registry
                .entry_if_owner(&target, &old_owner)
                .is_some(),
            "transient discovery failure must retain the live owner mirror"
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
                    .is_some(),
                "transient discovery failure must retain the live user-actor mirror"
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
        bridge.wire(Arc::new(services));
        let envelope = sign_envelope(envelope(), &keypair);

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

    #[test]
    fn uncertain_remote_force_detach_is_not_acknowledged_as_retired() {
        assert_eq!(
            confirmed_remote_force_detach_outcome(RelayForceDetachRemoteUserResourceReply {
                outcome: ForceDetachOutcome::NotPersisted,
                status: RelayRemoteResourceForceDetachStatus::Unknown,
            }),
            None,
            "an unknown remote outcome must close the ack channel so terminal teardown stays incomplete"
        );
        assert_eq!(
            confirmed_remote_force_detach_outcome(RelayForceDetachRemoteUserResourceReply {
                outcome: ForceDetachOutcome::NotPersisted,
                status: RelayRemoteResourceForceDetachStatus::NotLive,
            }),
            Some(ForceDetachOutcome::NotPersisted),
            "a proven-absent remote socket is safe to retire locally"
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
                    origin_identity(),
                    channel.clone(),
                    OriginInboundSequence(1),
                    envelope_claims(2),
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
                origin_identity(),
                channel,
                OriginInboundSequence(2),
                envelope_claims(2),
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
                    origin_identity(),
                    channel.clone(),
                    OriginInboundSequence(1),
                    envelope_claims(2),
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
                origin_identity(),
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
                    origin_identity(),
                    channel.clone(),
                    OriginInboundSequence(1),
                    envelope_claims(2),
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
                origin_identity(),
                channel,
                OriginInboundSequence(2),
                envelope_claims(2),
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
                origin_identity(),
                channel.clone(),
                OriginInboundSequence(1),
                envelope_claims(2),
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
                origin_identity(),
                channel.clone(),
                OriginInboundSequence(2),
                envelope_claims(2),
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
                origin_identity(),
                channel,
                OriginInboundSequence(3),
                envelope_claims(2),
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
                origin_identity(),
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
