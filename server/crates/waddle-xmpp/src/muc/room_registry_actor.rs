//! MUC Room Registry Actor
//!
//! Kameo actor that manages all MUC room actors. Replaces the DashMap-based
//! `MucRoomRegistry` with a single-writer actor that owns the room map and
//! spawns per-room `RoomActor` instances on demand.

use std::collections::HashMap;

use jid::BareJid;
use kameo::actor::ActorRef;
use kameo::message::Context;
use kameo::Actor;
use tracing::{debug, info, warn};

use super::room_actor::RoomActor;
use super::{MucRoom, RoomConfig};

/// Actor that owns the mapping from room JIDs to per-room actors.
///
/// All room creation, lookup, and destruction flows through this actor,
/// so no external synchronisation is needed.
#[derive(Actor)]
pub struct RoomRegistryActor {
    rooms: HashMap<BareJid, ActorRef<RoomActor>>,
    muc_domain: String,
}

impl RoomRegistryActor {
    /// Create a new registry for the given MUC service domain.
    pub fn new(muc_domain: String) -> Self {
        info!(domain = %muc_domain, "Creating RoomRegistryActor");
        Self {
            rooms: HashMap::new(),
            muc_domain,
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
        let actor_ref = kameo::spawn(RoomActor::new(room));
        self.rooms.insert(room_jid, actor_ref.clone());
        actor_ref
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
    type Reply = Result<Option<ActorRef<RoomActor>>, String>;

    async fn handle(
        &mut self,
        msg: GetRoom,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        Ok(self.rooms.get(&msg.room_jid).cloned())
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
    type Reply = Result<ActorRef<RoomActor>, String>;

    async fn handle(
        &mut self,
        msg: GetOrCreateRoom,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if let Some(actor_ref) = self.rooms.get(&msg.room_jid) {
            debug!(room = %msg.room_jid, "Room already exists");
            return Ok(actor_ref.clone());
        }

        info!(room = %msg.room_jid, "Creating new room via GetOrCreateRoom");
        let actor_ref = self.spawn_room(msg.room_jid, msg.waddle_id, msg.channel_id, msg.config);
        Ok(actor_ref)
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
    type Reply = Result<ActorRef<RoomActor>, String>;

    async fn handle(
        &mut self,
        msg: CreateRoom,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.rooms.contains_key(&msg.room_jid) {
            return Err(format!("Room {} already exists", msg.room_jid));
        }

        info!(room = %msg.room_jid, "Creating new room");
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
        match self.rooms.remove(&msg.room_jid) {
            Some(_actor_ref) => {
                info!(room = %msg.room_jid, "Destroyed room");
                true
            }
            None => {
                warn!(room = %msg.room_jid, "Attempted to destroy non-existent room");
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
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: RoomExists,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.rooms.contains_key(&msg.room_jid)
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
    type Reply = Result<Vec<BareJid>, String>;

    async fn handle(
        &mut self,
        _msg: ListRooms,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        Ok(self.rooms.keys().cloned().collect())
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
mod tests {
    use super::*;

    fn test_room_jid(name: &str) -> BareJid {
        format!("{}@muc.example.com", name)
            .parse()
            .expect("valid test JID")
    }

    async fn spawn_registry() -> ActorRef<RoomRegistryActor> {
        kameo::spawn(RoomRegistryActor::new("muc.example.com".to_string()))
    }

    #[tokio::test]
    async fn test_room_count_starts_at_zero() {
        let registry = spawn_registry().await;
        let count: usize = registry.ask(RoomCount).await.expect("ask");
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_create_room() {
        let registry = spawn_registry().await;
        let jid = test_room_jid("general");

        // Kameo flattens Result replies: ask() returns T directly
        let _actor_ref: ActorRef<RoomActor> = registry
            .ask(CreateRoom {
                room_jid: jid.clone(),
                waddle_id: "w-1".to_string(),
                channel_id: "c-1".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create room");

        let exists: bool = registry
            .ask(RoomExists { room_jid: jid })
            .await
            .expect("exists");
        assert!(exists);

        let count: usize = registry.ask(RoomCount).await.expect("count");
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_create_duplicate_room_fails() {
        let registry = spawn_registry().await;
        let jid = test_room_jid("dup");

        registry
            .ask(CreateRoom {
                room_jid: jid.clone(),
                waddle_id: "w-1".to_string(),
                channel_id: "c-1".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("first create");

        let result = registry
            .ask(CreateRoom {
                room_jid: jid,
                waddle_id: "w-1".to_string(),
                channel_id: "c-1".to_string(),
                config: RoomConfig::default(),
            })
            .await;

        // Should fail with HandlerError (duplicate)
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_room() {
        let registry = spawn_registry().await;
        let jid = test_room_jid("lookup");

        // Non-existent room returns None
        let got: Option<ActorRef<RoomActor>> = registry
            .ask(GetRoom {
                room_jid: jid.clone(),
            })
            .await
            .expect("get room");
        assert!(got.is_none());

        // Create it
        registry
            .ask(CreateRoom {
                room_jid: jid.clone(),
                waddle_id: "w-1".to_string(),
                channel_id: "c-1".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create");

        // Now it should be found
        let got: Option<ActorRef<RoomActor>> = registry
            .ask(GetRoom { room_jid: jid })
            .await
            .expect("get room");
        assert!(got.is_some());
    }

    #[tokio::test]
    async fn test_get_or_create_room_idempotent() {
        let registry = spawn_registry().await;
        let jid = test_room_jid("idempotent");

        let first: ActorRef<RoomActor> = registry
            .ask(GetOrCreateRoom {
                room_jid: jid.clone(),
                waddle_id: "w-1".to_string(),
                channel_id: "c-1".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("first get_or_create");

        let second: ActorRef<RoomActor> = registry
            .ask(GetOrCreateRoom {
                room_jid: jid,
                waddle_id: "w-1".to_string(),
                channel_id: "c-1".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("second get_or_create");

        assert_eq!(first.id(), second.id());

        let count: usize = registry.ask(RoomCount).await.expect("count");
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_destroy_room() {
        let registry = spawn_registry().await;
        let jid = test_room_jid("doomed");

        registry
            .ask(CreateRoom {
                room_jid: jid.clone(),
                waddle_id: "w-1".to_string(),
                channel_id: "c-1".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create");

        let removed: bool = registry
            .ask(DestroyRoom {
                room_jid: jid.clone(),
            })
            .await
            .expect("destroy");
        assert!(removed);

        let exists: bool = registry
            .ask(RoomExists { room_jid: jid })
            .await
            .expect("exists");
        assert!(!exists);
    }

    #[tokio::test]
    async fn test_destroy_non_existent_room_returns_false() {
        let registry = spawn_registry().await;
        let jid = test_room_jid("ghost");

        let removed: bool = registry
            .ask(DestroyRoom { room_jid: jid })
            .await
            .expect("destroy");
        assert!(!removed);
    }

    #[tokio::test]
    async fn test_is_muc_jid() {
        let registry = spawn_registry().await;

        let muc_jid: BareJid = "room@muc.example.com".parse().expect("valid JID");
        let other_jid: BareJid = "user@example.com".parse().expect("valid JID");

        let is_muc: bool = registry
            .ask(IsMucJid { jid: muc_jid })
            .await
            .expect("is_muc");
        assert!(is_muc);

        let is_muc: bool = registry
            .ask(IsMucJid { jid: other_jid })
            .await
            .expect("is_muc");
        assert!(!is_muc);
    }

    #[tokio::test]
    async fn test_list_rooms() {
        let registry = spawn_registry().await;

        registry
            .ask(CreateRoom {
                room_jid: test_room_jid("alpha"),
                waddle_id: "w-1".to_string(),
                channel_id: "c-1".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create alpha");

        registry
            .ask(CreateRoom {
                room_jid: test_room_jid("beta"),
                waddle_id: "w-2".to_string(),
                channel_id: "c-2".to_string(),
                config: RoomConfig::default(),
            })
            .await
            .expect("create beta");

        let mut rooms: Vec<BareJid> = registry.ask(ListRooms).await.expect("list");
        rooms.sort_by(|a, b| a.to_string().cmp(&b.to_string()));

        assert_eq!(rooms.len(), 2);
        assert_eq!(rooms[0].to_string(), "alpha@muc.example.com");
        assert_eq!(rooms[1].to_string(), "beta@muc.example.com");
    }
}
