//! MUC Room Registry Actor
//!
//! Kameo actor that manages all MUC room actors. Replaces the DashMap-based
//! `MucRoomRegistry` with a single-writer actor that owns the room map and
//! spawns per-room `RoomActor` instances on demand.

use std::collections::{HashMap, HashSet};

use jid::BareJid;
use kameo::Actor;
use kameo::actor::ActorRef;
use kameo::message::Context;
use thiserror::Error;
use tracing::{debug, info, warn};

use super::room_actor::RoomActor;
use super::{MucRoom, RoomConfig};
use crate::metrics;
use crate::xep::xep0421::OccupantIdSecret;

/// Actor that owns the mapping from room JIDs to per-room actors.
///
/// All room creation, lookup, and destruction flows through this actor,
/// so no external synchronisation is needed.
#[derive(Actor)]
#[actor(mailbox = bounded(1024))]
pub struct RoomRegistryActor {
    rooms: HashMap<BareJid, ActorRef<RoomActor>>,
    poisoned_rooms: HashSet<BareJid>,
    muc_domain: String,
    /// Per-deployment XEP-0421 occupant-id HMAC key. Forwarded to every
    /// `RoomActor` at spawn so all rooms in this deployment share the
    /// same keying material.
    occupant_id_secret: OccupantIdSecret,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RoomRegistryError {
    #[error("room {0} already exists")]
    RoomAlreadyExists(BareJid),
    #[error("room actor state for {0} was lost; explicit destroy/recreate is required")]
    RoomActorStateLost(BareJid),
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
        }
    }

    /// Spawn a `RoomActor` for the given room and insert it into the map.
    ///
    /// Returns a reference to the spawned actor.
    fn spawn_room(
        &mut self,
        room_jid: BareJid,
        waddle_id: String,
        channel_id: String,
        config: RoomConfig,
    ) -> ActorRef<RoomActor> {
        let room = MucRoom::new(room_jid.clone(), waddle_id, channel_id, config);
        let actor_ref = kameo::spawn(RoomActor::new(room, self.occupant_id_secret.clone()));
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
        Ok(self.spawn_room(msg.room_jid, msg.waddle_id, msg.channel_id, msg.config))
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
        Ok(self.spawn_room(msg.room_jid, waddle_id, channel_id, config))
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
        let actor_ref = self.spawn_room(msg.room_jid, msg.waddle_id, msg.channel_id, msg.config);
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
