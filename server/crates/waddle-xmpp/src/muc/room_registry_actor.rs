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
use tracing::{debug, info, warn};

use super::affiliation::DurableMembershipSource;
use super::durable::MucDurableStore;
use super::room_actor::{
    HydrateDurableRecipients, IsSealed, RestoreDurableRoomState, RoomActor, SealGuard,
    SealIfInactive,
};
use super::{MucRoom, RoomConfig};
use crate::metrics;
use crate::ownership::{
    ClaimEpoch, ClaimError, ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity,
    RolloutBackoff, SharedNodeIdentity, StalePredicate,
};
use crate::xep::xep0421::OccupantIdSecret;

/// A locally-spawned room's actor ref plus the Postgres claim epoch this
/// node acquired/won it under (ADR-0017 Phase 3 Slice 7). The epoch
/// travels with the actor ref so [`RoomRegistryActor::DestroyRoom`] can
/// release the exact claim this incarnation holds.
#[derive(Clone)]
struct RoomEntry {
    actor_ref: ActorRef<RoomActor>,
    claim_epoch: ClaimEpoch,
}

/// Actor that owns the mapping from room JIDs to per-room actors.
///
/// All room creation, lookup, and destruction flows through this actor,
/// so no external synchronisation is needed.
#[derive(Actor)]
pub struct RoomRegistryActor {
    rooms: HashMap<BareJid, RoomEntry>,
    poisoned_rooms: HashSet<BareJid>,
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
    /// own node lease is no longer fresh. Returns the epoch this node now
    /// holds the claim under.
    ///
    /// A live foreign owner (steal not applicable) is reported as
    /// [`RoomRegistryError::ClaimHeldByAnotherNode`] rather than
    /// attempted via any cross-node proxy — see that variant's doc
    /// comment for why that is out of this slice's scope.
    async fn acquire_room_claim(
        &self,
        room_jid: &BareJid,
    ) -> Result<ClaimEpoch, RoomRegistryError> {
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let identity = self.node_identity.current();
        match self.claim_store.ensure_claimed(&entity, &identity).await {
            Ok(epoch) => Ok(epoch),
            Err(ClaimError::AlreadyClaimed) => {
                self.steal_from_dead_owner(&entity, room_jid, &identity)
                    .await
            }
            Err(error) => {
                warn!(room = %room_jid, %error, "room claim acquisition failed");
                Err(RoomRegistryError::ClaimHeldByAnotherNode(room_jid.clone()))
            }
        }
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
        claim_epoch: ClaimEpoch,
    ) -> ActorRef<RoomActor> {
        let room = MucRoom::new(room_jid.clone(), waddle_id, channel_id, config);
        let actor_ref = RoomActor::spawn(RoomActor::new(room, self.occupant_id_secret.clone()));
        if let Some(store) = &self.durable_store {
            store.record_claim_epoch(&room_jid, claim_epoch);
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
                claim_epoch,
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
    /// gap: the claim epoch is read BEFORE the entry is removed, and
    /// [`Self::release_room_claim`] runs on it — the exact same
    /// best-effort, epoch-gated release [`DestroyRoom`]'s handler already
    /// uses for the graceful-destroy path.
    async fn live_room(
        &mut self,
        room_jid: &BareJid,
    ) -> Result<Option<ActorRef<RoomActor>>, RoomRegistryError> {
        if self.poisoned_rooms.contains(room_jid) {
            return Err(RoomRegistryError::RoomActorStateLost(room_jid.clone()));
        }
        if let Some(entry) = self.rooms.get(room_jid) {
            if entry.actor_ref.is_alive() {
                return Ok(Some(entry.actor_ref.clone()));
            }
            let claim_epoch = entry.claim_epoch;
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
            self.release_room_claim(room_jid, claim_epoch).await;
            return Err(RoomRegistryError::RoomActorStateLost(room_jid.clone()));
        }
        Ok(None)
    }

    /// Best-effort release of `room_jid`'s Postgres claim (dormancy
    /// eviction / explicit destroy, element 7's "graceful release").
    /// Epoch-gated and best-effort per [`ClaimStore::release`]'s own
    /// contract — a claim already stolen out from under this node is a
    /// no-op, not an error.
    async fn release_room_claim(&self, room_jid: &BareJid, claim_epoch: ClaimEpoch) {
        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let identity = self.node_identity.current();
        if let Err(error) = self
            .claim_store
            .release(&entity, &identity, claim_epoch)
            .await
        {
            warn!(room = %room_jid, %error, "failed to release room ownership claim");
        }
        if let Some(store) = &self.durable_store {
            store.forget_claim_epoch(room_jid);
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

        let claim_epoch = self.acquire_room_claim(&msg.room_jid).await?;
        info!(room = %msg.room_jid, "Creating new room via GetOrCreateRoom");
        self.poisoned_rooms.remove(&msg.room_jid);
        let actor_ref = self
            .spawn_room(
                msg.room_jid,
                msg.waddle_id,
                msg.channel_id,
                msg.config,
                claim_epoch,
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

        let claim_epoch = self.acquire_room_claim(&msg.room_jid).await?;
        self.poisoned_rooms.remove(&msg.room_jid);
        let actor_ref = self
            .spawn_room(msg.room_jid, waddle_id, channel_id, config, claim_epoch)
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

        let claim_epoch = self.acquire_room_claim(&msg.room_jid).await?;
        info!(room = %msg.room_jid, "Creating new room");
        self.poisoned_rooms.remove(&msg.room_jid);
        let actor_ref = self
            .spawn_room(
                msg.room_jid,
                msg.waddle_id,
                msg.channel_id,
                msg.config,
                claim_epoch,
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
        // XEP-0045 §10.9 (#1261): destroy removes the room "even if it
        // was defined as persistent" — wipe the durable rows (config,
        // subject, affiliations incl. bans) so the room cannot
        // resurrect from storage on the next join. Runs BEFORE the
        // claim release below: the delete is epoch-fenced against this
        // node's still-held claim. Best-effort — a fencing loss means
        // another node owns the room now and this node must not wipe
        // the new owner's rows.
        if removed_room || removed_poison {
            if let Some(store) = &self.durable_store {
                if let Err(error) = store.delete_room_state(&msg.room_jid).await {
                    warn!(
                        room = %msg.room_jid,
                        %error,
                        "Failed to delete durable room state on destroy; \
                         the room may resurrect from storage"
                    );
                }
            }
        }
        // ADR-0017 Phase 3 Slice 7: release the Postgres claim on every
        // terminal path (explicit destroy, dormancy-eviction sweep) —
        // "graceful release" per element 7. A poisoned-only removal (the
        // actor died and was already detected by `live_room`) has no
        // known epoch to release; the claim is instead reclaimed by
        // another node's `OwnerStale` steal once this node's own liveness
        // lease is what it takes to look stale (bounded residual gap,
        // same class as FIX 6e's fail-open-detach gap).
        if let Some(entry) = removed_entry {
            self.release_room_claim(&msg.room_jid, entry.claim_epoch)
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
                // ADR-0017 Phase 3 Slice 7: this is a terminal removal from
                // `self.rooms` exactly like `DestroyRoom` — release the
                // Postgres claim here too, or every guarded dormancy-evicted
                // room leaks its claim until this node's own liveness lease
                // looks stale to another node's `OwnerStale` steal.
                self.release_room_claim(&msg.room_jid, entry.claim_epoch)
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
            self.release_room_claim(&msg.room_jid, entry.claim_epoch)
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
                // ADR-0017 Phase 3 Slice 7: same terminal-removal claim
                // release as the guarded-destroy path above.
                self.release_room_claim(&msg.room_jid, entry.claim_epoch)
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
