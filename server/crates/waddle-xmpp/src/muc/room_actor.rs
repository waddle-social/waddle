//! Kameo actor wrapping a single MUC room.
//!
//! Each `RoomActor` owns a [`MucRoom`] and processes all operations
//! sequentially, removing the need for external `RwLock` synchronisation.
//! This is part of the Phase 3 actor-model migration.

use jid::{BareJid, FullJid};
use kameo::message::Context;
use kameo::Actor;
use std::convert::Infallible;
use thiserror::Error;

use super::room_registry::RoomInfo;
use super::{MucRoom, RoomConfig};
use crate::types::{Affiliation, Role};

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

/// Snapshot of occupant data, safe to send across actor boundaries.
#[derive(Debug, Clone)]
pub struct OccupantInfo {
    pub nick: String,
    pub real_jid: FullJid,
    pub role: Role,
    pub affiliation: Affiliation,
}

impl OccupantInfo {
    fn from_occupant(o: &super::Occupant) -> Self {
        Self {
            nick: o.nick.clone(),
            real_jid: o.real_jid.clone(),
            role: o.role,
            affiliation: o.affiliation,
        }
    }
}

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

/// Actor that owns a single [`MucRoom`] and handles all room operations.
///
/// Because Kameo processes messages one at a time, the actor holds a
/// `MucRoom` directly with no external synchronisation required.
#[derive(Actor)]
pub struct RoomActor {
    room: MucRoom,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RoomActorError {
    #[error("room is full")]
    RoomFull,
    #[error("nick '{0}' already in use")]
    NickAlreadyInUse(String),
    #[error("no occupant with nick '{0}'")]
    OccupantNotFound(String),
}

impl RoomActor {
    /// Create a new `RoomActor` wrapping the given room.
    pub fn new(room: MucRoom) -> Self {
        Self { room }
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Add an occupant to the room.
pub struct Join {
    pub nick: String,
    pub real_jid: FullJid,
    pub role: Role,
    pub affiliation: Affiliation,
}

impl kameo::message::Message<Join> for RoomActor {
    type Reply = Result<(), RoomActorError>;

    async fn handle(&mut self, msg: Join, _ctx: &mut Context<Self, Self::Reply>) -> Self::Reply {
        if self.room.is_full() {
            return Err(RoomActorError::RoomFull);
        }
        if self.room.get_occupant(&msg.nick).is_some() {
            return Err(RoomActorError::NickAlreadyInUse(msg.nick));
        }
        self.room.add_occupant(super::Occupant {
            real_jid: msg.real_jid,
            nick: msg.nick,
            role: msg.role,
            affiliation: msg.affiliation,
            is_remote: false,
            home_server: None,
        });
        Ok(())
    }
}

/// Remove an occupant from the room.
pub struct Leave {
    pub nick: String,
}

impl kameo::message::Message<Leave> for RoomActor {
    type Reply = Result<(), RoomActorError>;

    async fn handle(&mut self, msg: Leave, _ctx: &mut Context<Self, Self::Reply>) -> Self::Reply {
        self.room
            .remove_occupant(&msg.nick)
            .map(|_| ())
            .ok_or(RoomActorError::OccupantNotFound(msg.nick))
    }
}

/// Look up an occupant by their real JID.
pub struct GetOccupantByJid {
    pub jid: FullJid,
}

impl kameo::message::Message<GetOccupantByJid> for RoomActor {
    type Reply = Option<OccupantInfo>;

    async fn handle(
        &mut self,
        msg: GetOccupantByJid,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.room
            .find_occupant_by_real_jid(&msg.jid)
            .map(OccupantInfo::from_occupant)
    }
}

/// Look up an occupant by their nickname.
pub struct GetOccupantByNick {
    pub nick: String,
}

impl kameo::message::Message<GetOccupantByNick> for RoomActor {
    type Reply = Option<OccupantInfo>;

    async fn handle(
        &mut self,
        msg: GetOccupantByNick,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.room
            .get_occupant(&msg.nick)
            .map(OccupantInfo::from_occupant)
    }
}

/// Get basic room information.
pub struct GetInfo;

impl kameo::message::Message<GetInfo> for RoomActor {
    type Reply = Result<RoomInfo, Infallible>;

    async fn handle(
        &mut self,
        _msg: GetInfo,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        Ok(RoomInfo {
            room_jid: self.room.room_jid.clone(),
            occupant_count: self.room.occupant_count(),
            name: self.room.config.name.clone(),
        })
    }
}

/// Get the current room configuration.
pub struct GetConfig;

impl kameo::message::Message<GetConfig> for RoomActor {
    type Reply = Result<RoomConfig, Infallible>;

    async fn handle(
        &mut self,
        _msg: GetConfig,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        Ok(self.room.config.clone())
    }
}

/// Replace the room configuration.
pub struct UpdateConfig {
    pub config: RoomConfig,
}

impl kameo::message::Message<UpdateConfig> for RoomActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: UpdateConfig,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.room.config = msg.config;
    }
}

/// Change the persistent affiliation for a JID.
pub struct ChangeAffiliation {
    pub jid: BareJid,
    pub affiliation: Affiliation,
}

impl kameo::message::Message<ChangeAffiliation> for RoomActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: ChangeAffiliation,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.room.set_affiliation(msg.jid, msg.affiliation);
    }
}

/// Query the persistent affiliation for a JID.
pub struct GetAffiliation {
    pub jid: BareJid,
}

impl kameo::message::Message<GetAffiliation> for RoomActor {
    type Reply = Result<Affiliation, Infallible>;

    async fn handle(
        &mut self,
        msg: GetAffiliation,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        Ok(self.room.get_affiliation(&msg.jid))
    }
}

/// List all current occupants.
pub struct ListOccupants;

impl kameo::message::Message<ListOccupants> for RoomActor {
    type Reply = Vec<OccupantInfo>;

    async fn handle(
        &mut self,
        _msg: ListOccupants,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.room
            .occupants
            .values()
            .map(OccupantInfo::from_occupant)
            .collect()
    }
}

/// Get the number of occupants currently in the room.
pub struct OccupantCount;

impl kameo::message::Message<OccupantCount> for RoomActor {
    type Reply = usize;

    async fn handle(
        &mut self,
        _msg: OccupantCount,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.room.occupant_count()
    }
}

/// Destroy the room (clears all occupants).
pub struct Destroy;

impl kameo::message::Message<Destroy> for RoomActor {
    type Reply = ();

    async fn handle(
        &mut self,
        _msg: Destroy,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.room.occupants.clear();
    }
}

/// Get the room's bare JID.
pub struct GetRoomJid;

impl kameo::message::Message<GetRoomJid> for RoomActor {
    type Reply = Result<BareJid, Infallible>;

    async fn handle(
        &mut self,
        _msg: GetRoomJid,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        Ok(self.room.room_jid.clone())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use kameo::actor::ActorRef;
    use kameo::error::SendError;

    fn test_room() -> MucRoom {
        let room_jid: BareJid = "testroom@muc.example.com".parse().expect("valid jid");
        MucRoom::new(
            room_jid,
            "waddle-1".to_string(),
            "channel-1".to_string(),
            RoomConfig::default(),
        )
    }

    fn test_full_jid(user: &str) -> FullJid {
        format!("{}@example.com/res", user)
            .parse()
            .expect("valid jid")
    }

    async fn spawn_room_actor() -> ActorRef<RoomActor> {
        kameo::spawn(RoomActor::new(test_room()))
    }

    async fn spawn_room_actor_with_config(mut config: RoomConfig) -> ActorRef<RoomActor> {
        let room_jid: BareJid = "testroom@muc.example.com".parse().expect("valid jid");
        config.name = "Test Room".to_string();
        kameo::spawn(RoomActor::new(MucRoom::new(
            room_jid,
            "waddle-1".to_string(),
            "channel-1".to_string(),
            config,
        )))
    }

    #[tokio::test]
    async fn test_join_and_occupant_count() {
        let actor = spawn_room_actor().await;

        let count = actor.ask(OccupantCount).await.expect("ask");
        assert_eq!(count, 0);

        actor
            .ask(Join {
                nick: "alice".to_string(),
                real_jid: test_full_jid("alice"),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join should succeed");

        let count = actor.ask(OccupantCount).await.expect("ask");
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_join_duplicate_nick_rejected() {
        let actor = spawn_room_actor().await;

        actor
            .ask(Join {
                nick: "alice".to_string(),
                real_jid: test_full_jid("alice"),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("first join");

        let result = actor
            .ask(Join {
                nick: "alice".to_string(),
                real_jid: test_full_jid("bob"),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await;
        assert!(matches!(
            result,
            Err(SendError::HandlerError(RoomActorError::NickAlreadyInUse(nick)))
                if nick == "alice"
        ));
    }

    #[tokio::test]
    async fn test_join_rejected_when_room_full() {
        let actor = spawn_room_actor_with_config(RoomConfig {
            max_occupants: 1,
            ..RoomConfig::default()
        })
        .await;

        actor
            .ask(Join {
                nick: "alice".to_string(),
                real_jid: test_full_jid("alice"),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("first join");

        let result = actor
            .ask(Join {
                nick: "bob".to_string(),
                real_jid: test_full_jid("bob"),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await;
        assert!(matches!(
            result,
            Err(SendError::HandlerError(RoomActorError::RoomFull))
        ));
    }

    #[tokio::test]
    async fn test_leave() {
        let actor = spawn_room_actor().await;

        actor
            .ask(Join {
                nick: "alice".to_string(),
                real_jid: test_full_jid("alice"),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join");

        actor
            .ask(Leave {
                nick: "alice".to_string(),
            })
            .await
            .expect("leave should succeed");

        let count = actor.ask(OccupantCount).await.expect("ask");
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_leave_unknown_nick() {
        let actor = spawn_room_actor().await;

        let result = actor
            .ask(Leave {
                nick: "ghost".to_string(),
            })
            .await;
        assert!(matches!(
            result,
            Err(SendError::HandlerError(RoomActorError::OccupantNotFound(nick)))
                if nick == "ghost"
        ));
    }

    #[tokio::test]
    async fn test_get_occupant_by_nick() {
        let actor = spawn_room_actor().await;

        actor
            .ask(Join {
                nick: "alice".to_string(),
                real_jid: test_full_jid("alice"),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join");

        let info = actor
            .ask(GetOccupantByNick {
                nick: "alice".to_string(),
            })
            .await
            .expect("ask");
        assert!(info.is_some());
        let info = info.expect("occupant present");
        assert_eq!(info.nick, "alice");
        assert_eq!(info.role, Role::Participant);
    }

    #[tokio::test]
    async fn test_get_occupant_by_jid() {
        let actor = spawn_room_actor().await;
        let jid = test_full_jid("alice");

        actor
            .ask(Join {
                nick: "alice".to_string(),
                real_jid: jid.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join");

        let info = actor.ask(GetOccupantByJid { jid }).await.expect("ask");
        assert!(info.is_some());
    }

    #[tokio::test]
    async fn test_get_info() {
        let actor = spawn_room_actor().await;

        let info = actor.ask(GetInfo).await.expect("ask");
        assert_eq!(info.occupant_count, 0);
        assert_eq!(
            info.room_jid,
            "testroom@muc.example.com".parse::<BareJid>().expect("jid")
        );
    }

    #[tokio::test]
    async fn test_get_and_update_config() {
        let actor = spawn_room_actor().await;

        let config = actor.ask(GetConfig).await.expect("ask");
        assert!(config.members_only); // default

        let mut new_config = config;
        new_config.members_only = false;
        actor
            .ask(UpdateConfig { config: new_config })
            .await
            .expect("ask");

        let config = actor.ask(GetConfig).await.expect("ask");
        assert!(!config.members_only);
    }

    #[tokio::test]
    async fn test_change_and_get_affiliation() {
        let actor = spawn_room_actor().await;
        let jid: BareJid = "alice@example.com".parse().expect("jid");

        let aff = actor
            .ask(GetAffiliation { jid: jid.clone() })
            .await
            .expect("ask");
        assert_eq!(aff, Affiliation::None);

        actor
            .ask(ChangeAffiliation {
                jid: jid.clone(),
                affiliation: Affiliation::Admin,
            })
            .await
            .expect("ask");

        let aff = actor.ask(GetAffiliation { jid }).await.expect("ask");
        assert_eq!(aff, Affiliation::Admin);
    }

    #[tokio::test]
    async fn test_list_occupants() {
        let actor = spawn_room_actor().await;

        actor
            .ask(Join {
                nick: "alice".to_string(),
                real_jid: test_full_jid("alice"),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join alice");

        actor
            .ask(Join {
                nick: "bob".to_string(),
                real_jid: test_full_jid("bob"),
                role: Role::Moderator,
                affiliation: Affiliation::Admin,
            })
            .await
            .expect("join bob");

        let list = actor.ask(ListOccupants).await.expect("ask");
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn test_destroy() {
        let actor = spawn_room_actor().await;

        actor
            .ask(Join {
                nick: "alice".to_string(),
                real_jid: test_full_jid("alice"),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join");

        actor.ask(Destroy).await.expect("ask");

        let count = actor.ask(OccupantCount).await.expect("ask");
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_get_room_jid() {
        let actor = spawn_room_actor().await;

        let jid = actor.ask(GetRoomJid).await.expect("ask");
        assert_eq!(
            jid,
            "testroom@muc.example.com".parse::<BareJid>().expect("jid")
        );
    }
}
