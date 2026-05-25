//! Kameo actor wrapping a single MUC room.
//!
//! Each `RoomActor` owns a [`MucRoom`] and processes all operations
//! sequentially, removing the need for external `RwLock` synchronisation.
//! This is part of the Phase 3 actor-model migration.

use chrono::{DateTime, Utc};
use jid::{BareJid, FullJid};
use kameo::message::Context;
use kameo::Actor;
use std::convert::Infallible;
use thiserror::Error;

use super::affiliation::AffiliationEntry;
use super::pin::{PinStateChange, PinnedEntry};
use super::room_registry::RoomInfo;
use super::{MucRoom, RoomConfig, RoomSubjectTexts, SubjectState};
use crate::types::{Affiliation, Role};

mod admin_handlers;
mod occupancy_handlers;
mod snapshot_handlers;
#[cfg(test)]
mod tests;

pub use admin_handlers::{ApplyAdminItems, GetAdminContext, IsOwner};
pub use occupancy_handlers::{
    ClearMujiPresence, JoinWithAffiliation, LeaveByRealJid, MujiPresenceUpdateOutcome,
    PingSelfCheck, PresenceUpdateData, ReconcileChannelBackedRoom, UpsertMujiPresence,
};
pub use snapshot_handlers::{
    BuildGroupchatBroadcast, GetNicknameGeneration, GetRoomSnapshot, GroupchatBroadcastResult,
    RoomChainOccupant, RoomChainSnapshot,
};

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
pub struct JoinExistingOccupant {
    pub jid: FullJid,
    pub nick: String,
    pub affiliation: Affiliation,
    pub role: Role,
    /// Snapshot of this exact session's currently-advertised XEP-0272
    /// `<muji xmlns='urn:xmpp:jingle:muji:0'/>` element at the moment
    /// the new joiner asked to enter. Preparing is resource-owned
    /// coordination state, so join replay must not stamp an aggregate
    /// same-nick Muji payload onto an arbitrary full JID.
    pub muji: Option<crate::xep::xep0272::Muji>,
}

#[derive(Debug, Clone)]
pub struct JoinOutcome {
    pub existing_occupants: Vec<JoinExistingOccupant>,
    pub new_occupant_affiliation: Affiliation,
    pub new_occupant_role: Role,
    pub occupant_count: usize,
    pub room_jid: BareJid,
    pub is_same_bare_multi_session_join: bool,
    /// Snapshot of `MucRoom.subject` at join time. Powers the XEP-0045
    /// §7.2.15 historical-subject emission the WebSocket join handler
    /// builds via `muc::messages::build_subject_message`. Bundled with
    /// the rest of the join outcome so the join path needs no second
    /// actor round-trip.
    pub subject_state: Option<SubjectState>,
}

#[derive(Debug, Clone)]
pub struct LeaveOutcome {
    pub nick: String,
    pub affiliation: Affiliation,
    pub role: Role,
    pub leaving_room_jid: FullJid,
    pub remaining_occupants: Vec<FullJid>,
    pub removed_last_session: bool,
    pub cleared_muji_state: bool,
    pub remaining_muji: Option<crate::xep::xep0272::Muji>,
    pub remaining_muji_sessions: Vec<(FullJid, crate::xep::xep0272::Muji)>,
    pub remaining_nick_real_jid: Option<FullJid>,
    pub occupant_count: usize,
    /// Mirrors `MucRoom.config.persistent`. Surfaced so the leave
    /// caller can decide whether to evict the room's `RoomActor` +
    /// registry entry without an extra `GetConfig` round-trip when
    /// `removed_last_session && occupant_count == 0`. Non-persistent
    /// instant rooms (XEP-0045 §10.1.3) are eligible for immediate
    /// destruction; persistent rooms must NOT be evicted from memory
    /// without a re-hydration path.
    pub is_persistent: bool,
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
pub struct RoomActor {
    room: MucRoom,
    occupant_id_secret: crate::xep::xep0421::OccupantIdSecret,
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
    pub fn new(room: MucRoom, occupant_id_secret: crate::xep::xep0421::OccupantIdSecret) -> Self {
        Self {
            room,
            occupant_id_secret,
        }
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

/// Apply a XEP-0045 §8.1 subject change to the room. The interpreter
/// emits this in response to an `OutboundEvent::PersistRoomSubject`
/// produced by the room handler chain's subject handler after
/// authorization passes. The actor delegates to
/// [`MucRoom::set_subject`], which writes a `SubjectState` onto
/// `MucRoom.subject` for replay on the next join (XEP-0045 §7.2.15).
pub struct SetSubject {
    pub texts: RoomSubjectTexts,
    pub setter: BareJid,
    pub setter_nick: String,
    pub set_at: DateTime<Utc>,
}

impl kameo::message::Message<SetSubject> for RoomActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: SetSubject,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.room
            .set_subject(msg.texts, msg.setter, msg.setter_nick, msg.set_at);
    }
}

/// Apply a pin state change to the room (#414). The interpreter emits
/// this in response to an `OutboundEvent::ApplyPinChange` produced by
/// the room handler chain's `MucPinHandler` after authorization passes.
/// The actor delegates to [`MucRoom::upsert_pin`] /
/// [`MucRoom::remove_pin_by_target`].
pub struct ApplyPin {
    pub change: PinStateChange,
}

impl kameo::message::Message<ApplyPin> for RoomActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: ApplyPin,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        match msg.change {
            PinStateChange::Pin(entry) => {
                self.room.upsert_pin(entry);
            }
            PinStateChange::Unpin { target_stanza_id } => {
                self.room.remove_pin_by_target(&target_stanza_id);
            }
        }
    }
}

/// Read the room's pin list (#414). Returns the current pinned entries
/// in pin-time-desc order. Used by the IQ query handler for
/// `<query xmlns='urn:waddle:pin:0'/>` and by the chat-side hydration
/// path on room entry.
pub struct GetPinList;

impl kameo::message::Message<GetPinList> for RoomActor {
    type Reply = Vec<PinnedEntry>;

    async fn handle(
        &mut self,
        _msg: GetPinList,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.room.pinned_entries().to_vec()
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

/// Read the full persistent affiliation roster for the room,
/// optionally filtered to a single tier.
///
/// Returns every JID currently recorded in the room's
/// [`super::affiliation::AffiliationList`], wrapped as
/// [`AffiliationEntry`] values and sorted by JID ascending so callers
/// get a stable, deterministic ordering. When `filter` is
/// `Some(tier)`, only entries whose `affiliation == tier` are
/// returned; when `None`, every entry is returned.
///
/// Plumbing for the admin V2 `channels:affiliations` command (see
/// `docs/superpowers/specs/2026-05-17-admin-v2-design.md`). The
/// per-JID [`GetAffiliation`] message remains for point lookups;
/// batched roster readers must use this message rather than folding
/// over `GetAffiliation`.
///
/// `Affiliation::None` is never stored, so passing
/// `filter = Some(Affiliation::None)` always yields an empty `Vec`.
pub struct ListAffiliations {
    pub filter: Option<Affiliation>,
}

impl kameo::message::Message<ListAffiliations> for RoomActor {
    type Reply = Vec<AffiliationEntry>;

    async fn handle(
        &mut self,
        msg: ListAffiliations,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let mut entries: Vec<AffiliationEntry> = self
            .room
            .get_all_affiliations()
            .into_iter()
            .filter(|entry| match msg.filter {
                Some(tier) => entry.affiliation == tier,
                None => true,
            })
            .collect();
        entries.sort_by(|a, b| a.jid.cmp(&b.jid));
        entries
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

/// Ask whether this room is fully dormant — no occupants, no
/// subject, no pins, and no in-memory affiliations. The dormancy
/// janitor uses this to decide whether the persistent-room
/// `RoomActor` is safe to reap. See [`super::MucRoom::is_dormant`]
/// for the exact predicate.
pub struct IsDormant;

impl kameo::message::Message<IsDormant> for RoomActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        _msg: IsDormant,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.room.is_dormant()
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
