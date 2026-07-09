//! MUC Room Registry Actor
//!
//! Kameo actor that manages all MUC room actors. Replaces the DashMap-based
//! `MucRoomRegistry` with a single-writer actor that owns the room map and
//! spawns per-room `RoomActor` instances on demand.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use jid::BareJid;
use kameo::actor::{ActorRef, Spawn};
use kameo::message::Context;
use kameo::Actor;
use thiserror::Error;
use tracing::{debug, error, info, warn};

use super::affiliation::DurableMembershipSource;
use super::durable::MucDurableStore;
use super::room_actor::{
    BindRoomClaimFence, HydrateDurableRecipients, IsSealed, RestoreDurableRoomState, RoomActor,
    RoomSnapshot, SealForOwnerDestroy, SealGuard, SealIfInactive,
};
use super::{MucRoom, RoomClaimFenceContext, RoomConfig};
use crate::metrics;
use crate::ownership::{
    ClaimEpoch, ClaimError, ClaimGrant, ClaimStore, Entity, EntityType, InProcessClaimStore,
    NodeIdentity, RolloutBackoff, SharedNodeIdentity, StalePredicate,
};
use crate::xep::xep0421::OccupantIdSecret;

/// A locally-spawned room's actor ref plus the exact Postgres claim grant
/// this actor incarnation was spawned under. The owner identity must travel
/// with the epoch: [`SharedNodeIdentity`] can rotate after self-fencing, and
/// using its later value to release this actor's claim would turn an intended
/// release into a silent epoch-gated no-op.
#[derive(Clone)]
struct RoomEntry {
    actor_ref: ActorRef<RoomActor>,
    claim_grant: ClaimGrant,
}

/// Actor that owns the mapping from room JIDs to per-room actors.
///
/// All room creation, lookup, and destruction flows through this actor,
/// so no external synchronisation is needed.
#[derive(Actor)]
pub struct RoomRegistryActor {
    rooms: HashMap<BareJid, RoomEntry>,
    poisoned_rooms: HashSet<BareJid>,
    /// Exact grants whose terminal release returned backend uncertainty.
    /// A room cannot be acquired or respawned while an entry remains here:
    /// otherwise `ensure_claimed` could self-reacquire the surviving claim
    /// and spawn E2 under E1's fencing epoch.
    pending_claim_releases: HashMap<BareJid, ClaimGrant>,
    muc_domain: String,
    /// Per-deployment XEP-0421 occupant-id HMAC key. Forwarded to every
    /// `RoomActor` at spawn so all rooms in this deployment share the
    /// same keying material.
    occupant_id_secret: OccupantIdSecret,
    /// Durable membership source used to hydrate each freshly spawned
    /// `RoomActor`'s durable-recipient set (#1135). `None` in
    /// deployments/tests without a durable membership store; such
    /// rooms fall back to session-observed affiliations only.
    membership_source: Option<Arc<dyn DurableMembershipSource>>,
    /// Entity-ownership claim store (ADR-0017 Phase 3 Slice 7). Defaults
    /// to [`InProcessClaimStore`] — the single-node fallback — so every
    /// existing construction site (tests, single-node deployments)
    /// behaves exactly as before: a `GetOrCreateRoom` on the only node
    /// that could ever contend for the entity always succeeds
    /// immediately. Replaced with the real `PostgresClaimStore`-backed
    /// handle by [`WireClusteringClaims`] once clustering is configured
    /// (construction-order note: the room registry is spawned before
    /// `clustering::start_if_enabled` runs, mirroring the `local_claims`/
    /// `resume_bridge` fill-in-later cell pattern).
    claim_store: Arc<dyn ClaimStore>,
    /// This node's claim identity. Defaults to [`NodeIdentity::local`] —
    /// meaningless but harmless for [`InProcessClaimStore`], which never
    /// checks cross-node identity. Replaced by [`WireClusteringClaims`].
    node_identity: SharedNodeIdentity,
    /// Durable room-state store (ADR-0017 Phase 3 Slice 7): `None` in
    /// single-node/non-clustering deployments, matching today's purely
    /// in-memory room behavior exactly. Wired by [`WireClusteringClaims`].
    durable_store: Option<Arc<dyn MucDurableStore>>,
    /// Rollout-aware claim-acquisition backoff (ADR-0017 Phase 3 Slice 10):
    /// `None` (the default) in single-node/non-clustering deployments and
    /// every existing test — correct, since there is only ever one
    /// generation to place. Wired by [`WireClusteringClaims`].
    rollout_backoff: Option<Arc<dyn RolloutBackoff>>,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RoomRegistryError {
    #[error("room {0} already exists")]
    RoomAlreadyExists(BareJid),
    #[error("room actor state for {0} was lost; explicit destroy/recreate is required")]
    RoomActorStateLost(BareJid),
    #[error("room {0}'s previous ownership claim release is still unresolved")]
    ClaimReleasePending(BareJid),
    /// A request to the registry actor exceeded
    /// [`ROOM_REGISTRY_REPLY_TIMEOUT`](crate::muc::room_registry_handle::ROOM_REGISTRY_REPLY_TIMEOUT)
    /// without a reply. Surfaced (instead of hanging the caller indefinitely)
    /// so a wedged registry produces a visible, typed failure — the #757
    /// production incident class.
    #[error("room registry request timed out")]
    Timeout,
    /// The registry actor could not be reached (stopped, or its mailbox is
    /// saturated/closed). Distinct from [`RoomRegistryError::Timeout`]: the
    /// request never entered processing.
    #[error("room registry unavailable")]
    Unavailable,
    /// ADR-0017 Phase 3 Slice 7: `entity`'s Postgres claim is held by
    /// another, currently-live node, and this slice does not wire
    /// cross-node MUC message/join proxying (the ADR's own text names it
    /// as part of the design, but Phase 3's Non-goals exclude cross-node
    /// stanza routing GA — that lands in Phase 4). A live foreign owner
    /// therefore cannot be joined/created from this node yet; a dead
    /// owner's claim is instead stolen automatically (re-election) and
    /// never surfaces this variant.
    #[error("room {0}'s ownership claim is held by another live node")]
    ClaimHeldByAnotherNode(BareJid),
}

impl RoomRegistryActor {
    /// Create a new registry for the given MUC service domain.
    pub fn new(muc_domain: String, occupant_id_secret: OccupantIdSecret) -> Self {
        info!(domain = %muc_domain, "Creating RoomRegistryActor");
        Self {
            rooms: HashMap::new(),
            poisoned_rooms: HashSet::new(),
            pending_claim_releases: HashMap::new(),
            muc_domain,
            occupant_id_secret,
            membership_source: None,
            claim_store: Arc::new(InProcessClaimStore::new()),
            node_identity: SharedNodeIdentity::new(NodeIdentity::local()),
            durable_store: None,
            rollout_backoff: None,
        }
    }

    /// Attach a durable membership source so every spawned `RoomActor`
    /// hydrates its durable-recipient set before serving snapshots (#1135).
    #[must_use]
    pub fn with_membership_source(mut self, source: Arc<dyn DurableMembershipSource>) -> Self {
        self.membership_source = Some(source);
        self
    }

    /// Acquire this room's Postgres claim (ADR-0017 Phase 3 Slice 7),
    /// stealing from a dead owner (re-election) when the current owner's
    /// own node lease is no longer fresh. Returns the exact entity, owner
    /// incarnation, and epoch this actor must remain bound to across every
    /// later identity rotation.
    ///
    /// A live foreign owner (steal not applicable) is reported as
    /// [`RoomRegistryError::ClaimHeldByAnotherNode`] rather than
    /// attempted via any cross-node proxy — see that variant's doc
    /// comment for why that is out of this slice's scope.
    async fn acquire_room_claim(
        &self,
        room_jid: &BareJid,
    ) -> Result<ClaimGrant, RoomRegistryError> {
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let identity = self.node_identity.current();
        let epoch = match self.claim_store.ensure_claimed(&entity, &identity).await {
            Ok(epoch) => epoch,
            Err(ClaimError::AlreadyClaimed) => {
                self.steal_from_dead_owner(&entity, room_jid, &identity)
                    .await?
            }
            Err(error) => {
                warn!(room = %room_jid, %error, "room claim acquisition failed");
                return Err(RoomRegistryError::ClaimHeldByAnotherNode(room_jid.clone()));
            }
        };
        Ok(ClaimGrant::new(entity, identity, epoch))
    }

    /// The re-election path: `entity`'s claim is held by another node —
    /// steal it if (and only if) that node's own liveness lease is no
    /// longer fresh (element 7's "steal after owner death").
    async fn steal_from_dead_owner(
        &self,
        entity: &Entity,
        room_jid: &BareJid,
        identity: &NodeIdentity,
    ) -> Result<ClaimEpoch, RoomRegistryError> {
        let snapshot = match self.claim_store.current_claim(entity).await {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) | Err(_) => {
                return Err(RoomRegistryError::ClaimHeldByAnotherNode(room_jid.clone()))
            }
        };
        if snapshot.owner_lease_fresh {
            return Err(RoomRegistryError::ClaimHeldByAnotherNode(room_jid.clone()));
        }
        // ADR-0017 Phase 3 Slice 10 (Q5's rollout-aware placement rule): an
        // old-generation node backs off before racing a matching/newer
        // -generation node for a dead owner's claim, so each room moves
        // approximately once per deploy instead of up to N times. Purely a
        // placement heuristic — never affects correctness (the epoch CAS
        // below remains the sole authority over who actually wins).
        if let Some(backoff) = &self.rollout_backoff {
            let delay = backoff.acquire_delay().await;
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
        }
        match self
            .claim_store
            .steal_stale(
                entity,
                snapshot.claim_epoch,
                StalePredicate::OwnerStale,
                identity,
            )
            .await
        {
            Ok(new_epoch) => {
                info!(
                    room = %room_jid,
                    previous_owner = %snapshot.owner.node_id,
                    "re-elected room ownership from a dead owner"
                );
                self.notify_previous_owner_demoted(room_jid, &snapshot.owner, new_epoch);
                Ok(new_epoch)
            }
            Err(_) => Err(RoomRegistryError::ClaimHeldByAnotherNode(room_jid.clone())),
        }
    }

    /// Two-part demotion protocol, part (a) (element 7): fire a
    /// best-effort, detached Demote notification at the node this room
    /// was just stolen from. Never awaited by the caller — the
    /// guaranteed backstop is the fenced pre-fan-out check
    /// `waddle-server`'s `dispatch_to_room` runs independently.
    fn notify_previous_owner_demoted(
        &self,
        room_jid: &BareJid,
        previous_owner: &NodeIdentity,
        new_epoch: ClaimEpoch,
    ) {
        let Some(store) = self.durable_store.clone() else {
            return;
        };
        let room_jid = room_jid.clone();
        let previous_owner = previous_owner.clone();
        tokio::spawn(async move {
            if let Err(error) = store
                .notify_previous_owner_demoted(
                    &room_jid,
                    &previous_owner.node_id,
                    &previous_owner.node_epoch,
                    new_epoch,
                )
                .await
            {
                warn!(
                    room = %room_jid,
                    %error,
                    "best-effort Demote notification to the previous owner failed \
                     (the guaranteed fenced pre-fan-out backstop is unaffected)"
                );
            }
        });
    }

    /// Spawn a `RoomActor` for the given room and insert it into the map.
    ///
    /// When a [`MucDurableStore`] is configured, a
    /// [`RestoreDurableRoomState`] message is enqueued first (element 7:
    /// restore configuration/affiliations/subject before accepting any
    /// join), followed by [`HydrateDurableRecipients`] when a
    /// [`DurableMembershipSource`] is configured — both before the actor
    /// ref is handed to any caller, so every later message this
    /// incarnation processes observes fully-hydrated state (#1135's
    /// established FIFO-mailbox ordering guarantee, extended here).
    ///
    /// Returns a reference to the spawned actor.
    async fn spawn_room(
        &mut self,
        room_jid: BareJid,
        waddle_id: String,
        channel_id: String,
        config: RoomConfig,
        claim_grant: ClaimGrant,
    ) -> ActorRef<RoomActor> {
        debug_assert_eq!(
            claim_grant.entity,
            Entity::new(EntityType::RoomActor, room_jid.to_string())
        );
        let room = MucRoom::new(room_jid.clone(), waddle_id, channel_id, config);
        let actor_ref = RoomActor::spawn(RoomActor::new(room, self.occupant_id_secret.clone()));
        if let Some(store) = &self.durable_store {
            store.record_claim_epoch(&room_jid, claim_grant.epoch);
            let fence = RoomClaimFenceContext {
                entity: claim_grant.entity.clone(),
                epoch: claim_grant.epoch,
                owner: claim_grant.owner.clone(),
            };
            if let Err(error) = actor_ref.tell(BindRoomClaimFence { fence }).await {
                warn!(
                    room = %room_jid,
                    %error,
                    "failed to bind exact room-actor claim fence"
                );
            }
            if let Err(error) = actor_ref
                .tell(RestoreDurableRoomState {
                    store: Arc::clone(store),
                })
                .await
            {
                warn!(
                    room = %room_jid,
                    %error,
                    "failed to enqueue durable room-state restore for freshly \
                     spawned/re-claimed room actor"
                );
            }
        }
        if let Some(source) = &self.membership_source {
            if let Err(error) = actor_ref
                .tell(HydrateDurableRecipients {
                    source: Arc::clone(source),
                })
                .await
            {
                warn!(
                    room = %room_jid,
                    %error,
                    "failed to enqueue durable-recipient hydration for \
                     freshly spawned room actor"
                );
            }
        }
        self.rooms.insert(
            room_jid,
            RoomEntry {
                actor_ref: actor_ref.clone(),
                claim_grant,
            },
        );
        actor_ref
    }

    /// ADR-0017 Phase 3 Slice 7 FIX 3 (council-adjudicated): `async` (not
    /// sync) so the dead-actor branch can release the Postgres claim
    /// before returning. Previously this removed the dead entry from
    /// `self.rooms` WITHOUT releasing its Postgres claim at all — an
    /// orphaned claim: Postgres kept attributing the room to this node
    /// (which no longer has a live actor for it, or any record of the
    /// epoch needed to release it, once `self.rooms.remove` ran) until
    /// this node's own liveness lease eventually looked stale to another
    /// node's `OwnerStale` steal. This capture-then-release closes that
    /// gap: the full claim grant is captured BEFORE the entry is removed,
    /// and [`Self::release_room_claim`] retains it across backend failure —
    /// the exact same fail-closed release [`DestroyRoom`]'s handler uses for
    /// the graceful-destroy path.
    async fn live_room(
        &mut self,
        room_jid: &BareJid,
    ) -> Result<Option<ActorRef<RoomActor>>, RoomRegistryError> {
        self.retry_pending_claim_release(room_jid).await?;
        if self.poisoned_rooms.contains(room_jid) {
            return Err(RoomRegistryError::RoomActorStateLost(room_jid.clone()));
        }
        if let Some(entry) = self.rooms.get(room_jid) {
            if entry.actor_ref.is_alive() {
                return Ok(Some(entry.actor_ref.clone()));
            }
            let claim_grant = entry.claim_grant.clone();
            self.rooms.remove(room_jid);
            self.poisoned_rooms.insert(room_jid.clone());
            warn!(
                room = %room_jid,
                "Detected dead RoomActor; failing fast to avoid silent room state loss"
            );
            metrics::record_actor_restart("room_actor", "detected_dead_actor_fail_fast");
            // FIX 3: release BEFORE returning the error — a dead actor
            // whose claim is never released is a genuinely orphaned
            // claim (this node holds it in Postgres but has no way left
            // to act on it), not merely a "fail fast and let the caller
            // retry" situation.
            self.release_room_claim(room_jid, claim_grant).await;
            return Err(RoomRegistryError::RoomActorStateLost(room_jid.clone()));
        }
        Ok(None)
    }

    /// Queue and attempt an exact room-claim release. A backend error is
    /// uncertainty, not permission to forget the grant: the surviving row
    /// may still belong to the removed actor incarnation. Keeping it blocks
    /// replacement until a retry proves the exact release completed (or was
    /// already made irrelevant by a newer exact grant).
    async fn release_room_claim(&mut self, room_jid: &BareJid, claim_grant: ClaimGrant) {
        debug_assert_eq!(
            claim_grant.entity,
            Entity::new(EntityType::RoomActor, room_jid.to_string())
        );
        if let Some(existing) = self.pending_claim_releases.get(room_jid) {
            if existing != &claim_grant {
                error!(
                    room = %room_jid,
                    pending_owner = %existing.owner.node_id,
                    pending_owner_epoch = %existing.owner.node_epoch,
                    pending_claim_epoch = existing.epoch.0,
                    rejected_owner = %claim_grant.owner.node_id,
                    rejected_owner_epoch = %claim_grant.owner.node_epoch,
                    rejected_claim_epoch = claim_grant.epoch.0,
                    "refused to overwrite unresolved room-claim release evidence"
                );
            }
        } else {
            self.pending_claim_releases
                .insert(room_jid.clone(), claim_grant);
        }
        let _ = self.retry_pending_claim_release(room_jid).await;
    }

    /// Retry the exact unresolved release before any room lookup can proceed
    /// to acquisition. The captured owner is deliberate: the registry's
    /// shared current identity may have rotated since E1 acquired the claim.
    async fn retry_pending_claim_release(
        &mut self,
        room_jid: &BareJid,
    ) -> Result<(), RoomRegistryError> {
        let Some(claim_grant) = self.pending_claim_releases.get(room_jid).cloned() else {
            return Ok(());
        };
        match self
            .claim_store
            .release(&claim_grant.entity, &claim_grant.owner, claim_grant.epoch)
            .await
        {
            Ok(()) => {
                self.pending_claim_releases.remove(room_jid);
                if !self.rooms.contains_key(room_jid) {
                    if let Some(store) = &self.durable_store {
                        store.forget_claim_epoch(room_jid);
                    }
                }
                Ok(())
            }
            Err(error) => {
                warn!(
                    room = %room_jid,
                    owner = %claim_grant.owner.node_id,
                    owner_epoch = %claim_grant.owner.node_epoch,
                    claim_epoch = claim_grant.epoch.0,
                    %error,
                    "room ownership claim release remains unresolved; replacement is fenced"
                );
                Err(RoomRegistryError::ClaimReleasePending(room_jid.clone()))
            }
        }
    }
}

/// Wire the real, clustering-backed claim store/identity/durable store
/// into an already-spawned registry (ADR-0017 Phase 3 Slice 7).
///
/// Construction-order note: `clustering::start_if_enabled` (which
/// produces the real `ClaimStore`/`MucDurableStore`) runs *after* the
/// room registry is spawned in `server/mod.rs`, mirroring the exact
/// chicken-and-egg the `local_claims`/`resume_bridge` fill-in-later cells
/// already solve for Slices 5/6 — here realized as a message instead of
/// an `OnceLock`, since a kameo actor's state is only mutable through its
/// own mailbox. Sent once, before any client traffic can reach
/// `GetOrCreateRoom` (the HTTP/WebSocket listeners start after this
/// point in `server/mod.rs`).
pub struct WireClusteringClaims {
    pub claim_store: Arc<dyn ClaimStore>,
    pub node_identity: SharedNodeIdentity,
    pub durable_store: Option<Arc<dyn MucDurableStore>>,
    /// ADR-0017 Phase 3 Slice 10: `None` when clustering wiring predates
    /// this field's introduction — every existing/backward call site simply
    /// omits it via struct-update syntax at its own call site, which is a
    /// no-op behavior change (no backoff, exactly today's default).
    pub rollout_backoff: Option<Arc<dyn RolloutBackoff>>,
}

impl kameo::message::Message<WireClusteringClaims> for RoomRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: WireClusteringClaims,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.claim_store = msg.claim_store;
        self.node_identity = msg.node_identity;
        self.durable_store = msg.durable_store;
        self.rollout_backoff = msg.rollout_backoff;
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Look up a room actor by JID.
pub struct GetRoom {
    pub room_jid: BareJid,
}

impl kameo::message::Message<GetRoom> for RoomRegistryActor {
    type Reply = Result<Option<ActorRef<RoomActor>>, RoomRegistryError>;

    async fn handle(&mut self, msg: GetRoom, _ctx: &mut Context<Self, Self::Reply>) -> Self::Reply {
        self.live_room(&msg.room_jid).await
    }
}

/// Whether a get-or-create request spawned the room or found it
/// already registered (#1134). The registry actor's serialized
/// mailbox guarantees exactly one caller per room lifetime observes
/// [`RoomCreation::Created`] — that caller is the XEP-0045 §10.1.1
/// room creator and the only one entitled to the creator Owner grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomCreation {
    /// This request spawned the room actor: the caller created the room.
    Created,
    /// The room actor already existed.
    Existing,
}

/// Reply to [`GetOrCreateRoom`] / [`CreateInstantRoom`]: the room
/// actor plus the authoritative created-bit (#1134).
#[derive(Debug, Clone, kameo::Reply)]
pub struct RoomAcquisition {
    pub actor_ref: ActorRef<RoomActor>,
    pub creation: RoomCreation,
}

/// Get an existing room or create one if it does not exist.
pub struct GetOrCreateRoom {
    pub room_jid: BareJid,
    pub waddle_id: String,
    pub channel_id: String,
    pub config: RoomConfig,
}

impl kameo::message::Message<GetOrCreateRoom> for RoomRegistryActor {
    type Reply = Result<RoomAcquisition, RoomRegistryError>;

    async fn handle(
        &mut self,
        msg: GetOrCreateRoom,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if let Some(actor_ref) = self.live_room(&msg.room_jid).await? {
            debug!(room = %msg.room_jid, "Room already exists");
            return Ok(RoomAcquisition {
                actor_ref,
                creation: RoomCreation::Existing,
            });
        }

        let claim_grant = self.acquire_room_claim(&msg.room_jid).await?;
        info!(room = %msg.room_jid, "Creating new room via GetOrCreateRoom");
        self.poisoned_rooms.remove(&msg.room_jid);
        let actor_ref = self
            .spawn_room(
                msg.room_jid,
                msg.waddle_id,
                msg.channel_id,
                msg.config,
                claim_grant,
            )
            .await;
        Ok(RoomAcquisition {
            actor_ref,
            creation: RoomCreation::Created,
        })
    }
}

/// Create an instant room per XEP-0045.
pub struct CreateInstantRoom {
    pub room_jid: BareJid,
}

impl kameo::message::Message<CreateInstantRoom> for RoomRegistryActor {
    type Reply = Result<RoomAcquisition, RoomRegistryError>;

    async fn handle(
        &mut self,
        msg: CreateInstantRoom,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if let Some(actor_ref) = self.live_room(&msg.room_jid).await? {
            return Ok(RoomAcquisition {
                actor_ref,
                creation: RoomCreation::Existing,
            });
        }

        let room_local = msg
            .room_jid
            .node()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "instant".to_string());
        let waddle_id = format!("instant:{}", room_local);
        let channel_id = room_local.clone();
        let config = RoomConfig {
            name: room_local,
            members_only: false,
            persistent: false,
            ..RoomConfig::default()
        };

        let claim_grant = self.acquire_room_claim(&msg.room_jid).await?;
        self.poisoned_rooms.remove(&msg.room_jid);
        let actor_ref = self
            .spawn_room(msg.room_jid, waddle_id, channel_id, config, claim_grant)
            .await;
        Ok(RoomAcquisition {
            actor_ref,
            creation: RoomCreation::Created,
        })
    }
}

/// Create a room. Fails if a room with the same JID already exists.
pub struct CreateRoom {
    pub room_jid: BareJid,
    pub waddle_id: String,
    pub channel_id: String,
    pub config: RoomConfig,
}

impl kameo::message::Message<CreateRoom> for RoomRegistryActor {
    type Reply = Result<ActorRef<RoomActor>, RoomRegistryError>;

    async fn handle(
        &mut self,
        msg: CreateRoom,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.live_room(&msg.room_jid).await?.is_some() {
            return Err(RoomRegistryError::RoomAlreadyExists(msg.room_jid));
        }

        let claim_grant = self.acquire_room_claim(&msg.room_jid).await?;
        info!(room = %msg.room_jid, "Creating new room");
        self.poisoned_rooms.remove(&msg.room_jid);
        let actor_ref = self
            .spawn_room(
                msg.room_jid,
                msg.waddle_id,
                msg.channel_id,
                msg.config,
                claim_grant,
            )
            .await;
        Ok(actor_ref)
    }
}

/// Destroy a room, removing it from the registry.
///
/// Returns `true` if the room existed and was removed.
pub struct DestroyRoom {
    pub room_jid: BareJid,
}

impl kameo::message::Message<DestroyRoom> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: DestroyRoom,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let removed_entry = self.rooms.remove(&msg.room_jid);
        let removed_room = removed_entry.is_some();
        let removed_poison = self.poisoned_rooms.remove(&msg.room_jid);
        // Release the exact grant on every terminal path. A poisoned-only
        // removal keeps any unresolved grant in `pending_claim_releases`;
        // clearing the poison is not allowed to discard that evidence.
        if let Some(entry) = removed_entry {
            entry.actor_ref.kill();
            self.release_room_claim(&msg.room_jid, entry.claim_grant)
                .await;
        }
        if removed_room || removed_poison {
            info!(room = %msg.room_jid, "Destroyed room");
            true
        } else {
            warn!(room = %msg.room_jid, "Attempted to destroy non-existent room");
            false
        }
    }
}

/// Destroy only the exact actor incarnation observed by the caller.
///
/// Long-running owner/moderation flows may retain E1 across snapshot or
/// storage awaits while the registry advances the same room to E2. This
/// actor-ref CAS prevents the stale flow from removing E2 or releasing E2's
/// claim when it eventually reaches its destroy step.
pub struct DestroyRoomExact {
    pub room_jid: BareJid,
    pub expected_actor: ActorRef<RoomActor>,
}

impl kameo::message::Message<DestroyRoomExact> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: DestroyRoomExact,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let is_expected_incarnation = self
            .rooms
            .get(&msg.room_jid)
            .is_some_and(|entry| entry.actor_ref == msg.expected_actor);
        if !is_expected_incarnation {
            warn!(
                room = %msg.room_jid,
                "Refused exact room destroy because the registry now holds a different actor incarnation"
            );
            return false;
        }

        let Some(entry) = self.rooms.remove(&msg.room_jid) else {
            return false;
        };
        entry.actor_ref.kill();
        self.poisoned_rooms.remove(&msg.room_jid);
        self.release_room_claim(&msg.room_jid, entry.claim_grant)
            .await;
        info!(room = %msg.room_jid, "Destroyed exact room actor incarnation");
        true
    }
}

/// Typed signal that the registry removed and killed the exact actor named by
/// an owner-destroy flow while retaining its claim grant.
///
/// The registry does not process another room message until it receives
/// [`RoomDestroyEffectsDone`] (or observes that signal's sender was dropped),
/// so no replacement actor can appear while the caller emits the final
/// XEP-0045 unavailable-presence and SFU effects.
#[derive(Debug, Clone)]
pub struct RoomDestroyEffectsReserved {
    pub snapshot: RoomSnapshot,
}

/// Typed acknowledgement that the exact owner-destroy effects were emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoomDestroyEffectsDone;

/// Destroy one exact room actor while serializing its external effects with
/// replacement admission.
///
/// A plain check-then-destroy leaves a gap: E1 can pass its ownership check,
/// another request can replace it with E2, and E1 can still emit unavailable
/// presence or tear down E2's SFU participant before its final actor-ref CAS
/// loses. This message makes the registry itself the barrier:
///
/// 1. actor-ref-CAS and re-prove E1's exact claim;
/// 2. seal E1's admission mailbox and capture its final occupant snapshot;
/// 3. remove and kill E1 while retaining its exact claim grant;
/// 4. notify the caller that it alone may emit the snapshot's effects;
/// 5. keep the registry mailbox blocked until the caller acknowledges the
///    effects (dropping the acknowledgement sender also unblocks it);
/// 6. release E1's exact claim before serving any queued E2 admission.
pub struct DestroyRoomExactAfterEffects {
    pub room_jid: BareJid,
    pub expected_actor: ActorRef<RoomActor>,
    pub effects_reserved: tokio::sync::oneshot::Sender<RoomDestroyEffectsReserved>,
    pub effects_done: tokio::sync::oneshot::Receiver<RoomDestroyEffectsDone>,
}

impl kameo::message::Message<DestroyRoomExactAfterEffects> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: DestroyRoomExactAfterEffects,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(entry) = self.rooms.get(&msg.room_jid).cloned() else {
            return false;
        };
        if entry.actor_ref != msg.expected_actor {
            warn!(
                room = %msg.room_jid,
                "Refused effect-serialized room destroy because the registry now holds a different actor incarnation"
            );
            return false;
        }

        // First re-prove the registry entry's exact grant against the claim
        // store. Backend uncertainty leaves E1 untouched and unsealed; a
        // definitive loss demotes it without effects.
        match self
            .claim_store
            .fence(
                &entry.claim_grant.entity,
                &entry.claim_grant.owner,
                entry.claim_grant.epoch,
            )
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                warn!(
                    room = %msg.room_jid,
                    "Refused effect-serialized room destroy because its exact claim is no longer authoritative; demoting E1 without effects"
                );
                let Some(entry) = self.rooms.remove(&msg.room_jid) else {
                    return false;
                };
                entry.actor_ref.kill();
                self.poisoned_rooms.remove(&msg.room_jid);
                self.release_room_claim(&msg.room_jid, entry.claim_grant)
                    .await;
                return false;
            }
            Err(error) => {
                warn!(
                    room = %msg.room_jid,
                    %error,
                    "Refused effect-serialized room destroy because exact claim authority could not be verified"
                );
                return false;
            }
        }

        // The actor's mutation gate is the final cross-node proof and its
        // mailbox is the final admission boundary. Joins queued before this
        // message are included in the returned snapshot; later joins fail
        // with RoomSealed. After this succeeds there is no fallible authority
        // check before E1 is removed and the effect reservation is granted.
        let snapshot = match entry.actor_ref.ask(SealForOwnerDestroy).await {
            Ok(snapshot) => snapshot,
            Err(kameo::error::SendError::HandlerError(
                super::room_actor::RoomActorError::NotOwner,
            )) => {
                warn!(
                    room = %msg.room_jid,
                    "Owner-destroy seal proved E1 lost ownership; demoting without effects"
                );
                let Some(entry) = self.rooms.remove(&msg.room_jid) else {
                    return false;
                };
                entry.actor_ref.kill();
                self.poisoned_rooms.remove(&msg.room_jid);
                self.release_room_claim(&msg.room_jid, entry.claim_grant)
                    .await;
                return false;
            }
            Err(error) => {
                warn!(
                    room = %msg.room_jid,
                    ?error,
                    "Owner-destroy could not seal E1 and capture its final occupant snapshot"
                );
                return false;
            }
        };

        let Some(entry) = self.rooms.remove(&msg.room_jid) else {
            return false;
        };
        entry.actor_ref.kill();
        self.poisoned_rooms.remove(&msg.room_jid);

        if msg
            .effects_reserved
            .send(RoomDestroyEffectsReserved { snapshot })
            .is_ok()
        {
            match msg.effects_done.await {
                Ok(RoomDestroyEffectsDone) => {}
                Err(_) => debug!(
                    room = %msg.room_jid,
                    "Owner-destroy caller ended before acknowledging effects; completing exact claim release"
                ),
            }
        } else {
            debug!(
                room = %msg.room_jid,
                "Owner-destroy caller ended before accepting its effect reservation"
            );
        }

        self.release_room_claim(&msg.room_jid, entry.claim_grant)
            .await;
        info!(
            room = %msg.room_jid,
            "Destroyed exact room actor after serialized owner-destroy effects"
        );
        true
    }
}

/// Destroy a room only if it is still inactive (#1108).
///
/// Replaces the janitor's unconditional [`DestroyRoom`] for eviction
/// paths. Inside this serialized registry handler, the room actor is
/// asked to seal itself if it is still inactive at
/// `expected_occupancy_revision` ([`SealIfInactive`]); the room
/// actor's mailbox serializes that check against joins, so a join that
/// raced the caller's dormancy probe either bumped the revision
/// (→ seal refused) or is queued behind the seal and gets the typed
/// [`RoomActorError::RoomSealed`](super::room_actor::RoomActorError::RoomSealed)
/// refusal, which the join path retries through the registry.
///
/// Returns `true` when the room was sealed and removed.
pub struct DestroyRoomIfInactive {
    pub room_jid: BareJid,
    pub expected_occupancy_revision: u64,
    pub guard: SealGuard,
}

/// Bound for the in-handler seal ask so a wedged room actor cannot
/// wedge the whole registry: shorter than
/// [`ROOM_REGISTRY_REPLY_TIMEOUT`](super::room_registry_handle::ROOM_REGISTRY_REPLY_TIMEOUT)
/// so the outer registry ask still gets a reply.
const SEAL_ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

impl kameo::message::Message<DestroyRoomIfInactive> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: DestroyRoomIfInactive,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(entry) = self.rooms.get(&msg.room_jid).cloned() else {
            return false;
        };
        let sealed = entry
            .actor_ref
            .ask(SealIfInactive {
                expected_occupancy_revision: msg.expected_occupancy_revision,
                guard: msg.guard,
            })
            .mailbox_timeout(SEAL_ASK_TIMEOUT)
            .reply_timeout(SEAL_ASK_TIMEOUT)
            .await;
        match sealed {
            Ok(true) => {
                self.rooms.remove(&msg.room_jid);
                self.poisoned_rooms.remove(&msg.room_jid);
                entry.actor_ref.kill();
                // ADR-0017 Phase 3 Slice 7: this is a terminal removal from
                // `self.rooms` exactly like `DestroyRoom` — release the
                // Postgres claim here too, or every guarded dormancy-evicted
                // room leaks its claim until this node's own liveness lease
                // looks stale to another node's `OwnerStale` steal.
                self.release_room_claim(&msg.room_jid, entry.claim_grant)
                    .await;
                info!(room = %msg.room_jid, "Destroyed inactive room (guarded)");
                true
            }
            Ok(false) => {
                debug!(
                    room = %msg.room_jid,
                    "Guarded destroy refused: room no longer inactive at expected revision"
                );
                false
            }
            Err(error) => {
                // Never remove on uncertainty. If the seal actually
                // landed but the reply was lost, the seal is idempotent
                // and the next sweep converges (a sealed room reports
                // dormant and re-confirms the seal).
                warn!(
                    room = %msg.room_jid,
                    error = ?error,
                    "Guarded destroy seal ask failed; keeping the room"
                );
                false
            }
        }
    }
}

/// Purge a sealed room actor that is still registered (#1108
/// follow-up): when [`DestroyRoomIfInactive`]'s seal ask times out but
/// the queued [`SealIfInactive`] lands anyway, the actor stays in the
/// map, sealed, refusing every join. The join retry path sends this
/// before re-running get-or-create so the room respawns immediately
/// instead of waiting for the next janitor sweep.
///
/// Returns `true` when a sealed (or dead) actor was removed. Never
/// removes on uncertainty: a timeout or an unsealed reply keeps the
/// room.
pub struct ReapSealedRoom {
    pub room_jid: BareJid,
}

impl kameo::message::Message<ReapSealedRoom> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: ReapSealedRoom,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(entry) = self.rooms.get(&msg.room_jid).cloned() else {
            return false;
        };
        if !entry.actor_ref.is_alive() {
            self.rooms.remove(&msg.room_jid);
            self.poisoned_rooms.remove(&msg.room_jid);
            // ADR-0017 Phase 3 Slice 7: same terminal-removal claim release
            // as the `live_room` dead-actor path and `DestroyRoom`.
            self.release_room_claim(&msg.room_jid, entry.claim_grant)
                .await;
            info!(room = %msg.room_jid, "Reaped dead room actor during sealed-room purge");
            return true;
        }
        let sealed = entry
            .actor_ref
            .ask(IsSealed)
            .mailbox_timeout(SEAL_ASK_TIMEOUT)
            .reply_timeout(SEAL_ASK_TIMEOUT)
            .await;
        match sealed {
            Ok(true) => {
                self.rooms.remove(&msg.room_jid);
                self.poisoned_rooms.remove(&msg.room_jid);
                entry.actor_ref.kill();
                // ADR-0017 Phase 3 Slice 7: same terminal-removal claim
                // release as the guarded-destroy path above.
                self.release_room_claim(&msg.room_jid, entry.claim_grant)
                    .await;
                info!(
                    room = %msg.room_jid,
                    "Reaped sealed room actor left by a timed-out guarded destroy"
                );
                true
            }
            Ok(false) => false,
            Err(error) => {
                warn!(
                    room = %msg.room_jid,
                    error = ?error,
                    "Sealed-room probe failed; keeping the room"
                );
                false
            }
        }
    }
}

/// Check whether a room exists.
pub struct RoomExists {
    pub room_jid: BareJid,
}

impl kameo::message::Message<RoomExists> for RoomRegistryActor {
    type Reply = Result<bool, RoomRegistryError>;

    async fn handle(
        &mut self,
        msg: RoomExists,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        Ok(self.live_room(&msg.room_jid).await?.is_some())
    }
}

/// Check whether a bare JID belongs to this MUC service domain.
pub struct IsMucJid {
    pub jid: BareJid,
}

impl kameo::message::Message<IsMucJid> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: IsMucJid,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        msg.jid.domain().as_str() == self.muc_domain
    }
}

/// List all room JIDs.
pub struct ListRooms;

impl kameo::message::Message<ListRooms> for RoomRegistryActor {
    type Reply = Result<Vec<BareJid>, RoomRegistryError>;

    async fn handle(
        &mut self,
        _msg: ListRooms,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let room_ids: Vec<BareJid> = self.rooms.keys().cloned().collect();
        let mut live_rooms = Vec::with_capacity(room_ids.len());
        for room_jid in room_ids {
            match self.live_room(&room_jid).await {
                Ok(Some(_)) => live_rooms.push(room_jid),
                Ok(None) | Err(RoomRegistryError::RoomActorStateLost(_)) => {
                    // Ignore stale/dead rooms in discovery listing; per-room
                    // operations still fail fast with RoomActorStateLost.
                }
                Err(error) => return Err(error),
            }
        }
        Ok(live_rooms)
    }
}

/// Forget and hard-kill every locally held room actor after this entire node
/// loses its cluster lease. The registry map is drained atomically in one
/// mailbox message so terminal fencing is not serialized into one bounded ask
/// per room. Claims are intentionally not released under the fenced identity.
pub struct DemoteAllRooms;

impl kameo::message::Message<DemoteAllRooms> for RoomRegistryActor {
    type Reply = usize;

    async fn handle(
        &mut self,
        _msg: DemoteAllRooms,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let rooms = std::mem::take(&mut self.rooms);
        let count = rooms.len();
        self.poisoned_rooms.clear();
        // Do not clear `pending_claim_releases`: those grants already belong
        // to removed actors and retain the exact pre-rotation owner needed
        // for a safe retry after this self-fence changes node identity.
        for entry in rooms.into_values() {
            entry.actor_ref.kill();
        }
        debug!(count, "Demoted every local RoomActor after node self-fence");
        count
    }
}

/// Demote one room only when the relay request names the exact local node
/// incarnation that held the superseded claim and the local actor has not
/// already advanced to the carried winning claim epoch (or beyond).
pub struct DemoteRoomIfSuperseded {
    pub room_jid: BareJid,
    pub expected_owner: NodeIdentity,
    pub new_epoch: ClaimEpoch,
}

impl kameo::message::Message<DemoteRoomIfSuperseded> for RoomRegistryActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: DemoteRoomIfSuperseded,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let should_demote = self.rooms.get(&msg.room_jid).is_some_and(|entry| {
            entry.claim_grant.owner == msg.expected_owner && entry.claim_grant.epoch < msg.new_epoch
        });
        if !should_demote {
            return false;
        }
        let Some(entry) = self.rooms.remove(&msg.room_jid) else {
            return false;
        };
        entry.actor_ref.kill();
        debug!(
            room = %msg.room_jid,
            winning_epoch = msg.new_epoch.0,
            "Demoted superseded local RoomActor"
        );
        true
    }
}

/// Return the number of active rooms.
pub struct RoomCount;

impl kameo::message::Message<RoomCount> for RoomRegistryActor {
    type Reply = usize;

    async fn handle(
        &mut self,
        _msg: RoomCount,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.rooms.len()
    }
}

/// Test-only message whose handler never returns, used to deterministically
/// exercise the [`RoomRegistry`](crate::muc::room_registry_handle::RoomRegistry)
/// reply-timeout path (the #757 wedge) under `tokio::time` pause/advance.
#[cfg(test)]
pub(crate) struct HangForever;

#[cfg(test)]
impl kameo::message::Message<HangForever> for RoomRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        _msg: HangForever,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        // Park forever: the registry's single-consumer loop is blocked here,
        // mirroring a wedged handler. The caller must rely on its reply timeout.
        std::future::pending::<()>().await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
