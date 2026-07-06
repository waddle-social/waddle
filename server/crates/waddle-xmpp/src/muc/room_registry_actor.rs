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
use super::room_actor::{HydrateDurableRecipients, RoomActor, SealGuard, SealIfInactive};
use super::{MucRoom, RoomConfig};
use crate::metrics;
use crate::xep::xep0421::OccupantIdSecret;

/// Actor that owns the mapping from room JIDs to per-room actors.
///
/// All room creation, lookup, and destruction flows through this actor,
/// so no external synchronisation is needed.
#[derive(Actor)]
pub struct RoomRegistryActor {
    rooms: HashMap<BareJid, ActorRef<RoomActor>>,
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
        }
    }

    /// Attach a durable membership source so every spawned `RoomActor`
    /// hydrates its durable-recipient set before serving snapshots (#1135).
    #[must_use]
    pub fn with_membership_source(mut self, source: Arc<dyn DurableMembershipSource>) -> Self {
        self.membership_source = Some(source);
        self
    }

    /// Spawn a `RoomActor` for the given room and insert it into the map.
    ///
    /// When a [`DurableMembershipSource`] is configured, a
    /// [`HydrateDurableRecipients`] message is enqueued as the very
    /// first item in the fresh actor's FIFO mailbox *before* the actor
    /// ref is handed to any caller, so every later `GetRoomSnapshot`
    /// observes the hydrated durable-recipient set (#1135).
    ///
    /// Returns a reference to the spawned actor.
    async fn spawn_room(
        &mut self,
        room_jid: BareJid,
        waddle_id: String,
        channel_id: String,
        config: RoomConfig,
    ) -> ActorRef<RoomActor> {
        let room = MucRoom::new(room_jid.clone(), waddle_id, channel_id, config);
        let actor_ref = RoomActor::spawn(RoomActor::new(room, self.occupant_id_secret.clone()));
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
        self.rooms.insert(room_jid, actor_ref.clone());
        actor_ref
    }

    fn live_room(
        &mut self,
        room_jid: &BareJid,
    ) -> Result<Option<ActorRef<RoomActor>>, RoomRegistryError> {
        if self.poisoned_rooms.contains(room_jid) {
            return Err(RoomRegistryError::RoomActorStateLost(room_jid.clone()));
        }
        if let Some(actor_ref) = self.rooms.get(room_jid) {
            if actor_ref.is_alive() {
                return Ok(Some(actor_ref.clone()));
            }
            self.rooms.remove(room_jid);
            self.poisoned_rooms.insert(room_jid.clone());
            warn!(
                room = %room_jid,
                "Detected dead RoomActor; failing fast to avoid silent room state loss"
            );
            metrics::record_actor_restart("room_actor", "detected_dead_actor_fail_fast");
            return Err(RoomRegistryError::RoomActorStateLost(room_jid.clone()));
        }
        Ok(None)
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
        self.live_room(&msg.room_jid)
    }
}

/// Get an existing room or create one if it does not exist.
pub struct GetOrCreateRoom {
    pub room_jid: BareJid,
    pub waddle_id: String,
    pub channel_id: String,
    pub config: RoomConfig,
}

impl kameo::message::Message<GetOrCreateRoom> for RoomRegistryActor {
    type Reply = Result<ActorRef<RoomActor>, RoomRegistryError>;

    async fn handle(
        &mut self,
        msg: GetOrCreateRoom,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if let Some(actor_ref) = self.live_room(&msg.room_jid)? {
            debug!(room = %msg.room_jid, "Room already exists");
            return Ok(actor_ref);
        }

        info!(room = %msg.room_jid, "Creating new room via GetOrCreateRoom");
        self.poisoned_rooms.remove(&msg.room_jid);
        Ok(self
            .spawn_room(msg.room_jid, msg.waddle_id, msg.channel_id, msg.config)
            .await)
    }
}

/// Create an instant room per XEP-0045.
pub struct CreateInstantRoom {
    pub room_jid: BareJid,
}

impl kameo::message::Message<CreateInstantRoom> for RoomRegistryActor {
    type Reply = Result<ActorRef<RoomActor>, RoomRegistryError>;

    async fn handle(
        &mut self,
        msg: CreateInstantRoom,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if let Some(actor_ref) = self.live_room(&msg.room_jid)? {
            return Ok(actor_ref);
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

        self.poisoned_rooms.remove(&msg.room_jid);
        Ok(self
            .spawn_room(msg.room_jid, waddle_id, channel_id, config)
            .await)
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
        if self.live_room(&msg.room_jid)?.is_some() {
            return Err(RoomRegistryError::RoomAlreadyExists(msg.room_jid));
        }

        info!(room = %msg.room_jid, "Creating new room");
        self.poisoned_rooms.remove(&msg.room_jid);
        let actor_ref = self
            .spawn_room(msg.room_jid, msg.waddle_id, msg.channel_id, msg.config)
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
        let removed_room = self.rooms.remove(&msg.room_jid).is_some();
        let removed_poison = self.poisoned_rooms.remove(&msg.room_jid);
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
        let Some(actor_ref) = self.rooms.get(&msg.room_jid).cloned() else {
            return false;
        };
        let sealed = actor_ref
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
        Ok(self.live_room(&msg.room_jid)?.is_some())
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
            match self.live_room(&room_jid) {
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
