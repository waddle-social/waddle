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
use xmpp_parsers::message::Message;
use xmpp_parsers::presence::Presence;

use super::admin::{is_role_change_query, AdminItem};
use super::owner::{apply_config_form, build_destroy_notification, ConfigFormData, DestroyRequest};
use super::room_registry::RoomInfo;
use super::{
    build_affiliation_change_presence, build_ban_presence, build_kick_presence,
    build_role_change_presence, MucRoom, OutboundMucMessage, RoomConfig,
};
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

#[derive(Debug, Clone)]
pub struct RoomSnapshot {
    pub room: MucRoom,
}

#[derive(Debug, Clone)]
pub struct GroupchatBroadcastResult {
    pub sender_nick: String,
    pub messages: Vec<OutboundMucMessage>,
    pub occupant_bare_jids: Vec<String>,
    /// Per-XEP-0308 §3 occupancy generation for the sender's nickname
    /// at the moment this broadcast was built. Stored alongside the
    /// archive row so that later corrections can verify the sender is
    /// still in the same occupancy session (i.e. the nickname has not
    /// been left and re-claimed in the meantime).
    pub sender_nickname_generation: u64,
}

#[derive(Debug, Clone)]
pub struct JoinExistingOccupant {
    pub jid: FullJid,
    pub nick: String,
    pub affiliation: Affiliation,
    pub role: Role,
}

#[derive(Debug, Clone)]
pub struct JoinOutcome {
    pub existing_occupants: Vec<JoinExistingOccupant>,
    pub new_occupant_affiliation: Affiliation,
    pub new_occupant_role: Role,
    pub occupant_count: usize,
    pub room_jid: BareJid,
    pub is_same_bare_multi_session_join: bool,
}

#[derive(Debug, Clone)]
pub struct LeaveOutcome {
    pub nick: String,
    pub affiliation: Affiliation,
    pub leaving_room_jid: FullJid,
    pub remaining_occupants: Vec<FullJid>,
    pub removed_last_session: bool,
    pub occupant_count: usize,
}

#[derive(Debug, Clone)]
pub struct PresenceUpdateOutcome {
    pub sender_nick: String,
    pub sender_real_jid: FullJid,
    pub sender_role: Role,
    pub sender_affiliation: Affiliation,
    pub room_jid: BareJid,
    pub recipients: Vec<FullJid>,
}

#[derive(Debug, Clone)]
pub struct AdminContext {
    pub affiliation: Affiliation,
    pub role: Role,
    pub nick: Option<String>,
}

#[derive(Debug, Clone, Error)]
pub enum AdminApplyError {
    #[error("occupant '{0}' not found in room")]
    OccupantNotFound(String),
    #[error("cannot remove the last owner from a room")]
    CannotRemoveLastOwner,
    #[error("{0}")]
    PermissionDenied(String),
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
#[actor(mailbox = bounded(2048))]
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
    #[error("join is forbidden (members_only={members_only})")]
    JoinForbidden { members_only: bool },
    /// XEP-0045 §7.4: sender is not an occupant of this room. Maps to
    /// the typed stanza error `<error type='cancel'><not-acceptable/></error>`.
    #[error("sender '{0}' is not an occupant of this room")]
    SenderNotOccupant(FullJid),
    /// XEP-0045 §7.5: a visitor in a moderated room may not speak.
    /// Maps to the typed stanza error `<error type='auth'><forbidden/></error>`.
    #[error("visitor '{0}' may not speak in a moderated room")]
    VisitorMayNotSpeak(FullJid),
    /// Broadcast prep failed inside the room (occupant lookup, role
    /// check, etc.). Maps to `<error type='wait'><internal-server-error/></error>`
    /// because it represents a transient server-side fault — clients
    /// may retry. Matches RFC 6120 §8.3.2 guidance and other
    /// repo-wide `<internal-server-error/>` emission sites.
    #[error("groupchat broadcast preparation failed: {0}")]
    BroadcastFailed(String),
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

pub struct GetSnapshot;

impl kameo::message::Message<GetSnapshot> for RoomActor {
    type Reply = Result<RoomSnapshot, Infallible>;

    async fn handle(
        &mut self,
        _msg: GetSnapshot,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        Ok(RoomSnapshot {
            room: self.room.clone(),
        })
    }
}

pub struct BuildGroupchatBroadcast {
    pub sender_jid: FullJid,
    pub message: Message,
}

impl kameo::message::Message<BuildGroupchatBroadcast> for RoomActor {
    type Reply = Result<GroupchatBroadcastResult, RoomActorError>;

    async fn handle(
        &mut self,
        msg: BuildGroupchatBroadcast,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let sender_occupant = self
            .room
            .find_occupant_by_real_jid(&msg.sender_jid)
            .ok_or_else(|| RoomActorError::SenderNotOccupant(msg.sender_jid.clone()))?;
        let sender_nick = sender_occupant.nick.clone();

        if self.room.config.moderated && sender_occupant.role == Role::Visitor {
            return Err(RoomActorError::VisitorMayNotSpeak(msg.sender_jid.clone()));
        }

        let messages = self
            .room
            .broadcast_message(&sender_nick, &msg.message)
            .map_err(|error| RoomActorError::BroadcastFailed(error.to_string()))?;

        let sender_jid_for_filter = msg.sender_jid;
        let occupant_bare_jids: Vec<String> = self
            .room
            .occupants
            .values()
            .flat_map(|o| {
                self.room
                    .get_occupant_sessions(&o.nick)
                    .into_iter()
                    .filter(|jid| *jid != sender_jid_for_filter)
            })
            .map(|jid| jid.to_bare().to_string())
            .collect();

        let sender_nickname_generation = self
            .room
            .current_nickname_generation(&sender_nick)
            .unwrap_or(0);

        Ok(GroupchatBroadcastResult {
            sender_nick,
            messages,
            occupant_bare_jids,
            sender_nickname_generation,
        })
    }
}

/// Query the current per-nickname occupancy generation. Returns 0 when
/// the nickname has never been observed by this actor (e.g. after
/// server restart, which closes the correction window for prior
/// archive entries per XEP-0308 §3).
pub struct GetNicknameGeneration {
    pub nick: String,
}

impl kameo::message::Message<GetNicknameGeneration> for RoomActor {
    type Reply = u64;

    async fn handle(
        &mut self,
        msg: GetNicknameGeneration,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.room
            .current_nickname_generation(&msg.nick)
            .unwrap_or(0)
    }
}

pub struct ReconcileChannelBackedRoom {
    pub room_jid: BareJid,
    pub waddle_id: String,
    pub channel_id: String,
    pub desired_config: RoomConfig,
}

impl kameo::message::Message<ReconcileChannelBackedRoom> for RoomActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: ReconcileChannelBackedRoom,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let instant_name = msg.room_jid.node().map(|node| node.to_string());
        let mut desired_config = msg.desired_config;
        desired_config.description = self.room.config.description.clone();
        desired_config.subject = self.room.config.subject.clone();
        if !self.room.config.name.is_empty()
            && instant_name.as_deref() != Some(self.room.config.name.as_str())
        {
            desired_config.name = self.room.config.name.clone();
        }
        self.room.waddle_id = msg.waddle_id;
        self.room.channel_id = msg.channel_id;
        self.room.config = desired_config;
    }
}

pub struct JoinWithAffiliation {
    pub sender_jid: FullJid,
    pub nick: String,
    pub effective_affiliation: Affiliation,
    pub local_domain: String,
}

impl kameo::message::Message<JoinWithAffiliation> for RoomActor {
    type Reply = Result<JoinOutcome, RoomActorError>;

    async fn handle(
        &mut self,
        msg: JoinWithAffiliation,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if msg.effective_affiliation != Affiliation::None {
            self.room.update_affiliation_from_resolver(
                msg.sender_jid.to_bare(),
                msg.effective_affiliation,
            );
        }

        if !self.room.can_user_join(&msg.sender_jid.to_bare()) {
            return Err(RoomActorError::JoinForbidden {
                members_only: self.room.config.members_only,
            });
        }
        if self.room.is_full() {
            return Err(RoomActorError::RoomFull);
        }

        let mut is_same_bare_multi_session_join = false;
        if let Some(existing) = self.room.get_occupant(&msg.nick) {
            if existing.real_jid != msg.sender_jid {
                if existing.real_jid.to_bare() == msg.sender_jid.to_bare() {
                    is_same_bare_multi_session_join = true;
                } else {
                    return Err(RoomActorError::NickAlreadyInUse(msg.nick));
                }
            }
        }

        let existing_occupants: Vec<JoinExistingOccupant> = self
            .room
            .occupants
            .values()
            .flat_map(|o| {
                self.room
                    .get_occupant_sessions(&o.nick)
                    .into_iter()
                    .map(move |jid| JoinExistingOccupant {
                        jid,
                        nick: o.nick.clone(),
                        affiliation: o.affiliation,
                        role: o.role,
                    })
            })
            .collect();

        let new_occupant = self.room.add_occupant_with_affiliation(
            msg.sender_jid,
            msg.nick.clone(),
            Some(msg.local_domain.as_str()),
        );
        let new_occupant_affiliation = new_occupant.affiliation;
        let new_occupant_role = new_occupant.role;
        let occupant_count = self.room.occupant_count();
        let room_jid = self.room.room_jid.clone();

        Ok(JoinOutcome {
            existing_occupants,
            new_occupant_affiliation,
            new_occupant_role,
            occupant_count,
            room_jid,
            is_same_bare_multi_session_join,
        })
    }
}

pub struct LeaveByRealJid {
    pub sender_jid: FullJid,
}

impl kameo::message::Message<LeaveByRealJid> for RoomActor {
    type Reply = Result<Option<LeaveOutcome>, Infallible>;

    async fn handle(
        &mut self,
        msg: LeaveByRealJid,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(nick) = self
            .room
            .find_nick_by_real_jid(&msg.sender_jid)
            .map(ToOwned::to_owned)
        else {
            return Ok(None);
        };
        let Some(occupant) = self.room.get_occupant(&nick) else {
            return Ok(None);
        };
        let affiliation = occupant.affiliation;
        let leaving_room_jid = self
            .room
            .room_jid
            .clone()
            .with_resource_str(&nick)
            .expect("nick was previously accepted as resource");
        let remaining_occupants: Vec<FullJid> = self
            .room
            .occupants
            .values()
            .flat_map(|o| self.room.get_occupant_sessions(&o.nick))
            .filter(|jid| *jid != msg.sender_jid)
            .collect();
        let removed_last_session = self
            .room
            .remove_occupant_session(&nick, &msg.sender_jid)
            .unwrap_or(false);
        let occupant_count = self.room.occupant_count();
        Ok(Some(LeaveOutcome {
            nick,
            affiliation,
            leaving_room_jid,
            remaining_occupants,
            removed_last_session,
            occupant_count,
        }))
    }
}

pub struct PresenceUpdateData {
    pub sender_jid: FullJid,
}

impl kameo::message::Message<PresenceUpdateData> for RoomActor {
    type Reply = Result<Option<PresenceUpdateOutcome>, Infallible>;

    async fn handle(
        &mut self,
        msg: PresenceUpdateData,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let Some(sender_occupant) = self.room.find_occupant_by_real_jid(&msg.sender_jid) else {
            return Ok(None);
        };
        let sender_nick = sender_occupant.nick.clone();
        let sender_real_jid = sender_occupant.real_jid.clone();
        let sender_role = sender_occupant.role;
        let sender_affiliation = sender_occupant.affiliation;
        let room_jid = self.room.room_jid.clone();
        let recipients = self
            .room
            .occupants
            .values()
            .flat_map(|o| self.room.get_occupant_sessions(&o.nick))
            .collect();
        Ok(Some(PresenceUpdateOutcome {
            sender_nick,
            sender_real_jid,
            sender_role,
            sender_affiliation,
            room_jid,
            recipients,
        }))
    }
}

pub struct PingSelfCheck {
    pub nick: String,
    pub sender_jid: FullJid,
}

impl kameo::message::Message<PingSelfCheck> for RoomActor {
    type Reply = Result<(), RoomActorError>;

    async fn handle(
        &mut self,
        msg: PingSelfCheck,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let occupant = self
            .room
            .get_occupant(&msg.nick)
            .ok_or_else(|| RoomActorError::OccupantNotFound(msg.nick.clone()))?;
        if occupant.real_jid != msg.sender_jid {
            return Err(RoomActorError::OccupantNotFound(
                "Self-ping only allowed for own occupant".to_string(),
            ));
        }
        Ok(())
    }
}

pub struct GetAdminContext {
    pub sender_jid: FullJid,
}

impl kameo::message::Message<GetAdminContext> for RoomActor {
    type Reply = Result<AdminContext, Infallible>;

    async fn handle(
        &mut self,
        msg: GetAdminContext,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let sender_affiliation = self.room.get_affiliation(&msg.sender_jid.to_bare());
        let sender_occupant = self.room.find_occupant_by_real_jid(&msg.sender_jid);
        let sender_role = sender_occupant.map(|o| o.role).unwrap_or(Role::None);
        Ok(AdminContext {
            affiliation: sender_affiliation,
            role: sender_role,
            nick: sender_occupant.map(|occupant| occupant.nick.clone()),
        })
    }
}

pub struct ApplyAdminItems {
    pub sender_jid: FullJid,
    pub sender_affiliation: Affiliation,
    pub sender_role: Role,
    pub items: Vec<AdminItem>,
}

impl kameo::message::Message<ApplyAdminItems> for RoomActor {
    type Reply = Result<Vec<(jid::FullJid, Presence)>, AdminApplyError>;

    async fn handle(
        &mut self,
        msg: ApplyAdminItems,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let mut presence_updates: Vec<(jid::FullJid, Presence)> = Vec::new();
        let mut occupants_to_kick: Vec<String> = Vec::new();
        if is_role_change_query(&msg.items) {
            for item in &msg.items {
                let Some(target_nick) = item.nick.clone() else {
                    continue;
                };
                let Some(new_role) = item.role else {
                    continue;
                };
                let target_occupant = self
                    .room
                    .get_occupant(&target_nick)
                    .cloned()
                    .ok_or_else(|| AdminApplyError::OccupantNotFound(target_nick.clone()))?;
                let can_modify = match (
                    msg.sender_affiliation,
                    msg.sender_role,
                    target_occupant.affiliation,
                    new_role,
                ) {
                    (Affiliation::Owner, _, _, _) => true,
                    (Affiliation::Admin, _, target_aff, _) if target_aff != Affiliation::Owner => {
                        true
                    }
                    (_, Role::Moderator, target_aff, _)
                        if !matches!(target_aff, Affiliation::Owner | Affiliation::Admin) =>
                    {
                        true
                    }
                    _ => false,
                };
                if !can_modify {
                    return Err(AdminApplyError::PermissionDenied(
                        "You don't have permission to change this user's role".to_string(),
                    ));
                }
                let from_room_jid = self
                    .room
                    .room_jid
                    .with_resource_str(&target_nick)
                    .expect("nick was previously accepted as resource");
                if new_role == Role::None {
                    for (nick, occupant) in self.room.occupants.iter() {
                        let is_self = nick == &target_nick;
                        let presence = build_kick_presence(
                            &from_room_jid,
                            &occupant.real_jid,
                            target_occupant.affiliation,
                            is_self,
                            item.reason.as_deref(),
                            Some(&msg.sender_jid.to_bare()),
                            Some(&target_occupant.real_jid),
                        );
                        presence_updates.push((occupant.real_jid.clone(), presence));
                    }
                    occupants_to_kick.push(target_nick);
                } else {
                    if let Some(occ) = self.room.occupants.get_mut(&target_nick) {
                        occ.role = new_role;
                    }
                    for (nick, occupant) in self.room.occupants.iter() {
                        let is_self = nick == &target_nick;
                        let presence = build_role_change_presence(
                            &from_room_jid,
                            &occupant.real_jid,
                            target_occupant.affiliation,
                            new_role,
                            is_self,
                            Some(&target_occupant.real_jid),
                        );
                        presence_updates.push((occupant.real_jid.clone(), presence));
                    }
                }
            }
        } else {
            for item in &msg.items {
                let Some(target_jid) = item.jid.clone() else {
                    continue;
                };
                let Some(new_affiliation) = item.affiliation else {
                    continue;
                };
                let can_modify = match new_affiliation {
                    Affiliation::Owner => msg.sender_affiliation == Affiliation::Owner,
                    Affiliation::Admin
                    | Affiliation::Member
                    | Affiliation::None
                    | Affiliation::Outcast => {
                        matches!(
                            msg.sender_affiliation,
                            Affiliation::Owner | Affiliation::Admin
                        )
                    }
                };
                if !can_modify {
                    return Err(AdminApplyError::PermissionDenied(format!(
                        "You don't have permission to set {} affiliation",
                        crate::muc::admin::affiliation_to_str(new_affiliation)
                    )));
                }
                if new_affiliation != Affiliation::Owner {
                    let target_current_affiliation = self.room.get_affiliation(&target_jid);
                    if target_current_affiliation == Affiliation::Owner {
                        let owners = self.room.get_jids_by_affiliation(Affiliation::Owner);
                        if owners.len() == 1 && owners.contains(&target_jid) {
                            return Err(AdminApplyError::CannotRemoveLastOwner);
                        }
                    }
                }
                let change = self
                    .room
                    .set_affiliation(target_jid.clone(), new_affiliation);
                if change.is_none() {
                    continue;
                }
                let affected_occupant = self
                    .room
                    .occupants
                    .values()
                    .find(|o| o.real_jid.to_bare() == target_jid)
                    .cloned();
                if let Some(occupant) = affected_occupant {
                    let from_room_jid = self
                        .room
                        .room_jid
                        .with_resource_str(&occupant.nick)
                        .expect("nick was previously accepted as resource");
                    if new_affiliation == Affiliation::Outcast {
                        for (nick, occ) in self.room.occupants.iter() {
                            let is_self = nick == &occupant.nick;
                            let presence = build_ban_presence(
                                &from_room_jid,
                                &occ.real_jid,
                                is_self,
                                item.reason.as_deref(),
                                Some(&msg.sender_jid.to_bare()),
                                Some(&occupant.real_jid),
                            );
                            presence_updates.push((occ.real_jid.clone(), presence));
                        }
                        occupants_to_kick.push(occupant.nick.clone());
                    } else {
                        for (nick, occ) in self.room.occupants.iter() {
                            let is_self = nick == &occupant.nick;
                            let presence = build_affiliation_change_presence(
                                &from_room_jid,
                                &occ.real_jid,
                                new_affiliation,
                                occupant.role,
                                is_self,
                                Some(&occupant.real_jid),
                            );
                            presence_updates.push((occ.real_jid.clone(), presence));
                        }
                    }
                }
            }
        }
        for nick in occupants_to_kick {
            self.room.remove_occupant(&nick);
        }
        Ok(presence_updates)
    }
}

pub struct IsOwner {
    pub jid: BareJid,
}

impl kameo::message::Message<IsOwner> for RoomActor {
    type Reply = bool;

    async fn handle(&mut self, msg: IsOwner, _ctx: &mut Context<Self, Self::Reply>) -> Self::Reply {
        self.room.get_affiliation(&msg.jid) == Affiliation::Owner
    }
}

pub struct ApplyOwnerConfig {
    pub form_data: ConfigFormData,
}

impl kameo::message::Message<ApplyOwnerConfig> for RoomActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: ApplyOwnerConfig,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        apply_config_form(&mut self.room.config, &msg.form_data);
    }
}

pub struct DestroyWithNotifications {
    pub sender_jid: FullJid,
    pub request: DestroyRequest,
}

impl kameo::message::Message<DestroyWithNotifications> for RoomActor {
    type Reply = Result<Vec<(jid::FullJid, Presence)>, Infallible>;

    async fn handle(
        &mut self,
        msg: DestroyWithNotifications,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let mut updates = Vec::new();
        for (nick, occupant) in self.room.occupants.iter() {
            let is_self = occupant.real_jid == msg.sender_jid;
            let presence = build_destroy_notification(
                &self.room.room_jid,
                nick,
                &occupant.real_jid,
                &msg.request,
                is_self,
            );
            updates.push((occupant.real_jid.clone(), presence));
        }
        self.room.occupants.clear();
        Ok(updates)
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
    async fn test_apply_admin_items_rejects_moderator_role_change_on_admin() {
        let actor = spawn_room_actor().await;

        actor
            .ask(Join {
                nick: "alice".to_string(),
                real_jid: test_full_jid("alice"),
                role: Role::Moderator,
                affiliation: Affiliation::Admin,
            })
            .await
            .expect("join");

        let sender_jid = test_full_jid("mod");
        let result = actor
            .ask(ApplyAdminItems {
                sender_jid,
                sender_affiliation: Affiliation::None,
                sender_role: Role::Moderator,
                items: vec![AdminItem {
                    jid: None,
                    nick: Some("alice".to_string()),
                    affiliation: None,
                    role: Some(Role::Visitor),
                    reason: None,
                }],
            })
            .await;

        assert!(matches!(
            result,
            Err(SendError::HandlerError(AdminApplyError::PermissionDenied(
                _
            )))
        ));

        let occupant = actor
            .ask(GetOccupantByNick {
                nick: "alice".to_string(),
            })
            .await
            .expect("occupant")
            .expect("occupant exists");
        assert_eq!(occupant.role, Role::Moderator);

        let count = actor.ask(OccupantCount).await.expect("count");
        assert_eq!(
            count, 1,
            "actor should stay healthy after permission denial"
        );
    }

    #[tokio::test]
    async fn test_apply_admin_items_cannot_remove_last_owner() {
        let actor = spawn_room_actor().await;
        let owner_jid: BareJid = "owner@example.com".parse().expect("valid bare jid");

        actor
            .ask(ChangeAffiliation {
                jid: owner_jid.clone(),
                affiliation: Affiliation::Owner,
            })
            .await
            .expect("set owner");

        let result = actor
            .ask(ApplyAdminItems {
                sender_jid: test_full_jid("owner"),
                sender_affiliation: Affiliation::Owner,
                sender_role: Role::Moderator,
                items: vec![AdminItem {
                    jid: Some(owner_jid.clone()),
                    nick: None,
                    affiliation: Some(Affiliation::Member),
                    role: None,
                    reason: None,
                }],
            })
            .await;

        assert!(matches!(
            result,
            Err(SendError::HandlerError(
                AdminApplyError::CannotRemoveLastOwner
            ))
        ));

        let still_owner = actor
            .ask(IsOwner { jid: owner_jid })
            .await
            .expect("owner check");
        assert!(still_owner, "last owner must be preserved");
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
