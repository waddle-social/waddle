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

pub use admin_handlers::{
    ApplyAdminItems, ApplyAffiliationChange, EnforceMembersOnly, EnforceMembersOnlyAffiliations,
    GetAdminContext, IsOwner,
};
pub use occupancy_handlers::{
    ClearMujiPresence, InCallPresenceUpdateOutcome, JoinAffiliationGrant, JoinWithAffiliation,
    LeaveByRealJid, MujiPresenceUpdateOutcome, PingSelfCheck, PresenceUpdateData,
    ReconcileChannelBackedRoom, UpsertInCallState, UpsertMujiPresence,
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
    pub config_revision: u64,
    pub admission_revision: u64,
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
    /// This exact session's in-call presence state
    /// (`urn:waddle:in-call:0`, #1029 raised hand / #1030 mute) at join
    /// time. The join handler appends an `<in-call>` payload carrying
    /// these sub-states to the replayed presence so a late joiner sees
    /// who already has a hand up or is muted.
    pub in_call: crate::xep::InCallPresenceState,
}

#[derive(Debug, Clone)]
pub struct JoinOutcome {
    pub existing_occupants: Vec<JoinExistingOccupant>,
    pub new_occupant_affiliation: Affiliation,
    pub new_occupant_role: Role,
    pub occupant_count: usize,
    pub room_jid: BareJid,
    pub is_same_bare_multi_session_join: bool,
    pub is_existing_session_rejoin: bool,
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
    /// The room's admission counter at leave time (#1108). The
    /// empty-room eviction path passes it to the registry's guarded
    /// destroy; a join admitted after this leave bumps the counter and
    /// makes the destroy refuse instead of orphaning the new occupant.
    pub occupancy_revision: u64,
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
    #[error("admin cannot change an owner's affiliation")]
    CannotAdminModifyOwner,
    #[error("admins and moderators cannot change an owner or admin role")]
    CannotModifyPrivilegedRole,
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
    config_revision: u64,
    admission_revision: u64,
    /// Monotonically increasing counter bumped on every successful
    /// admission (#1108). The dormancy probe ([`IsDormant`]) returns
    /// it and the registry's guarded destroy
    /// ([`super::room_registry_actor::DestroyRoomIfInactive`]) refuses
    /// when it moved — a join that landed after the janitor's probe
    /// makes the probe's revision stale, closing the probe→destroy
    /// TOCTOU that orphaned freshly-admitted occupants.
    occupancy_revision: u64,
    /// Set by [`SealIfInactive`] immediately before the registry
    /// removes this actor from its map (#1108). A sealed actor refuses
    /// further admissions with [`RoomActorError::RoomSealed`] so a
    /// caller holding a stale `ActorRef` retries through the registry
    /// (which respawns the room) instead of joining a destroyed room.
    sealed: bool,
    occupant_id_secret: crate::xep::xep0421::OccupantIdSecret,
    /// Durable membership hydrated from the deployment's membership
    /// source at spawn (#1135). Kept separate from
    /// `MucRoom.affiliation_list` on purpose: it never grants join /
    /// admin rights and never clobbers richer session-observed
    /// affiliations (owner/admin) — it only widens the durable inbox
    /// recipient set computed by [`GetRoomSnapshot`], and being a pure
    /// mirror of the durable store it does not block room dormancy
    /// ([`MucRoom::is_dormant`]) the way in-memory-only affiliations do.
    durable_member_recipients: Vec<BareJid>,
    /// The durable membership source received in
    /// [`HydrateDurableRecipients`], retained so an affiliation change
    /// to `None` can re-run hydration (round-2 review R1): the durable
    /// union covers channel AND space relations, so revoking only the
    /// explicit channel grant must not prune a user who remains
    /// space-entitled. `None` until first hydration (tests spawning a
    /// bare actor; pre-hydration mutations fall back to the plain
    /// prune, which is the fail-closed direction).
    membership_source: Option<std::sync::Arc<dyn super::affiliation::DurableMembershipSource>>,
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
    #[error("join admission snapshot is stale")]
    StaleAdmissionRevision,
    /// #1108: this room actor was sealed by the registry's guarded
    /// destroy and is about to be dropped. Retryable — the caller must
    /// re-run the registry lookup, which respawns the room.
    #[error("room actor is sealed pending destruction")]
    RoomSealed,
    /// #1107: the joining FULL JID already occupies this room under a
    /// different nick. Waddle locks nicknames to identity, so per
    /// XEP-0045 §7.6 this maps to the typed stanza error
    /// `<error type='cancel'><not-acceptable/></error>` instead of
    /// admitting a second (ghost) occupancy or performing a nick change.
    #[error(
        "occupant already joined as '{current_nick}', refusing second nick '{requested_nick}'"
    )]
    OccupantAlreadyJoinedUnderDifferentNick {
        current_nick: String,
        requested_nick: String,
    },
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
            config_revision: 0,
            admission_revision: 0,
            occupancy_revision: 0,
            sealed: false,
            occupant_id_secret,
            durable_member_recipients: Vec::new(),
            membership_source: None,
        }
    }

    /// Drop a JID from the spawn-time hydrated durable-recipient
    /// mirror (#1135) when a runtime affiliation mutation removes its
    /// membership (F1). Returns whether the caller must re-run
    /// hydration afterwards via
    /// [`Self::refresh_durable_recipients_from_source`].
    ///
    /// Every affiliation-mutating handler must call this with the JID
    /// and the *requested* affiliation — unconditionally, even when
    /// `MucRoom::set_affiliation` reports no stored change, because a
    /// hydrated-only member has no affiliation-list entry (its stored
    /// affiliation is already `None`) yet still needs pruning from the
    /// mirror.
    ///
    /// - `Outcast` (ban) is unambiguous: the direct prune is final and
    ///   needs no re-hydration — the snapshot's Outcast filter backs
    ///   it up regardless of what the durable union reports.
    /// - `None` only revokes the *requested channel affiliation*; the
    ///   durable union hydrated at spawn also covers SPACE-level
    ///   relations (round-2 review R1), so the caller must re-run
    ///   hydration to converge the mirror to the durable truth —
    ///   otherwise a space-entitled member silently loses inbox
    ///   fan-out until respawn. The prune still runs first so a
    ///   failed re-hydration fails toward NOT delivering (F1).
    ///
    /// A later re-grant to `Member`+ is covered by the
    /// affiliation-list side of the durable-recipient union in
    /// [`snapshot_handlers::GetRoomSnapshot`].
    #[must_use]
    fn prune_durable_recipient_if_removed(
        &mut self,
        jid: &BareJid,
        new_affiliation: Affiliation,
    ) -> bool {
        match new_affiliation {
            Affiliation::Outcast => {
                self.durable_member_recipients
                    .retain(|recipient| recipient != jid);
                false
            }
            Affiliation::None => {
                self.durable_member_recipients
                    .retain(|recipient| recipient != jid);
                true
            }
            Affiliation::Owner | Affiliation::Admin | Affiliation::Member => false,
        }
    }

    /// Re-run durable-membership hydration against the retained source
    /// (round-2 review R1), replacing the mirror with the query result.
    ///
    /// Called by affiliation handlers after a prune-to-`None` so the
    /// mirror converges to the durable channel∪space truth (rare
    /// event, one reply-timeout-bounded query). Because the query is
    /// awaited *inside* the mutating handler and Kameo processes
    /// messages sequentially, no [`snapshot_handlers::GetRoomSnapshot`]
    /// can observe an intermediate state — the prune and the refresh
    /// are atomic from the mailbox's perspective, so a pruned member
    /// can never transiently resurrect. On query failure the mirror is
    /// left as pruned: fail toward NOT delivering to the removed jid
    /// (privacy beats availability — F1).
    async fn refresh_durable_recipients_from_source(&mut self) {
        let Some(source) = self.membership_source.clone() else {
            return;
        };
        match source
            .list_durable_member_jids(&self.room.waddle_id, &self.room.channel_id)
            .await
        {
            Ok(mut members) => {
                members.sort();
                members.dedup();
                self.durable_member_recipients = members;
            }
            Err(error) => {
                tracing::warn!(
                    room = %self.room.room_jid,
                    %error,
                    "durable membership re-hydration after affiliation removal \
                     failed; keeping the pruned mirror (fail toward not \
                     delivering to the removed member)"
                );
            }
        }
    }
}

/// Hydrate this actor incarnation's durable-recipient set from the
/// deployment's durable membership store (#1135).
///
/// The room registry enqueues this as the *first* message after
/// spawning a `RoomActor`, before handing the actor ref to any caller.
/// Because the mailbox is FIFO and the actor processes messages
/// sequentially, every later [`GetRoomSnapshot`] observes the hydrated
/// set — there is no window in which a groupchat message fans out
/// against a not-yet-hydrated recipient list.
///
/// Fail-open on source errors: the actor keeps serving with
/// session-observed affiliations only (pre-#1135 behavior) rather than
/// wedging the room.
pub struct HydrateDurableRecipients {
    pub source: std::sync::Arc<dyn super::affiliation::DurableMembershipSource>,
}

impl kameo::message::Message<HydrateDurableRecipients> for RoomActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: HydrateDurableRecipients,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        // Retain the source so affiliation changes to `None` can
        // re-run hydration later (round-2 review R1) instead of
        // guessing whether the member is still space-entitled.
        self.membership_source = Some(std::sync::Arc::clone(&msg.source));
        match msg
            .source
            .list_durable_member_jids(&self.room.waddle_id, &self.room.channel_id)
            .await
        {
            Ok(mut members) => {
                members.sort();
                members.dedup();
                self.durable_member_recipients = members;
            }
            Err(error) => {
                tracing::warn!(
                    room = %self.room.room_jid,
                    %error,
                    "durable membership hydration failed; durable inbox \
                     recipients limited to session-observed affiliations"
                );
            }
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
        if self.sealed {
            return Err(RoomActorError::RoomSealed);
        }
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
        self.occupancy_revision = self.occupancy_revision.saturating_add(1);
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
    type Reply = u64;

    async fn handle(
        &mut self,
        msg: UpdateConfig,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.room.config = msg.config;
        self.config_revision = self.config_revision.saturating_add(1);
        self.admission_revision = self.admission_revision.saturating_add(1);
        self.config_revision
    }
}

/// Replace the room config only if the config revision still matches the
/// caller's attempted update. Used for best-effort rollback without clobbering
/// a later successful rename, including identical-name updates.
pub struct RollbackConfigIfRevision {
    pub expected_revision: u64,
    pub config: RoomConfig,
}

impl kameo::message::Message<RollbackConfigIfRevision> for RoomActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: RollbackConfigIfRevision,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.config_revision != msg.expected_revision {
            return false;
        }
        self.room.config = msg.config;
        self.config_revision = self.config_revision.saturating_add(1);
        self.admission_revision = self.admission_revision.saturating_add(1);
        true
    }
}

/// Atomically update a group-DM config after checking the exact sender is still
/// both a persistent member and a joined occupant session.
pub struct UpdateGroupDmConfigByMember {
    pub config: RoomConfig,
    pub sender_jid: FullJid,
}

#[derive(Debug, Error)]
pub enum UpdateGroupDmConfigByMemberError {
    #[error("room is not a group DM")]
    NotGroupDm,
    #[error("sender is not a group-DM member")]
    NotMember,
    #[error("sender is not a joined group-DM occupant")]
    NotOccupant,
}

impl kameo::message::Message<UpdateGroupDmConfigByMember> for RoomActor {
    type Reply = Result<RoomSnapshot, UpdateGroupDmConfigByMemberError>;

    async fn handle(
        &mut self,
        msg: UpdateGroupDmConfigByMember,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if !self.room.config.group_dm {
            return Err(UpdateGroupDmConfigByMemberError::NotGroupDm);
        }
        let sender_bare = msg.sender_jid.to_bare();
        if self.room.get_affiliation(&sender_bare) < Affiliation::Member {
            return Err(UpdateGroupDmConfigByMemberError::NotMember);
        }
        let sender_is_occupant = self.room.occupants.values().any(|occupant| {
            occupant.real_jid == msg.sender_jid
                || self
                    .room
                    .get_occupant_sessions(&occupant.nick)
                    .iter()
                    .any(|session| session == &msg.sender_jid)
        });
        if !sender_is_occupant {
            return Err(UpdateGroupDmConfigByMemberError::NotOccupant);
        }
        self.room.config = msg.config;
        self.config_revision = self.config_revision.saturating_add(1);
        self.admission_revision = self.admission_revision.saturating_add(1);
        Ok(RoomSnapshot {
            room: self.room.clone(),
            config_revision: self.config_revision,
            admission_revision: self.admission_revision,
        })
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
        let needs_rehydration = self.prune_durable_recipient_if_removed(&msg.jid, msg.affiliation);
        if self
            .room
            .set_affiliation(msg.jid, msg.affiliation)
            .is_some()
        {
            self.admission_revision = self.admission_revision.saturating_add(1);
        }
        if needs_rehydration {
            self.refresh_durable_recipients_from_source().await;
        }
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
/// subject, no pins, and no explicit in-memory affiliation grants.
/// The dormancy janitor uses this to decide whether the
/// persistent-room `RoomActor` is safe to reap. See
/// [`super::MucRoom::is_dormant`] for the exact predicate.
///
/// The reply carries the admission [`occupancy
/// revision`](DormancyStatus::occupancy_revision) so the follow-up
/// guarded destroy can detect a join that raced the probe (#1108).
pub struct IsDormant;

/// Reply to [`IsDormant`]: the dormancy verdict plus the occupancy
/// revision it was computed at.
#[derive(Debug, Clone, Copy, kameo::Reply)]
pub struct DormancyStatus {
    pub dormant: bool,
    /// The room's admission counter at probe time. Pass it to
    /// [`super::room_registry_actor::DestroyRoomIfInactive`]; the
    /// destroy refuses when any admission moved the counter since.
    pub occupancy_revision: u64,
}

impl kameo::message::Message<IsDormant> for RoomActor {
    type Reply = DormancyStatus;

    async fn handle(
        &mut self,
        _msg: IsDormant,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        DormancyStatus {
            // A sealed actor is dormant by definition: the seal already
            // certified inactivity and refuses every admission since.
            // Without this, an EmptyNonPersistent seal whose registry
            // reply timed out (explicit creator-Owner grant keeps
            // `is_dormant()` false) would never be re-confirmed by the
            // janitor — a permanently unjoinable registered room.
            dormant: self.sealed || self.room.is_dormant(),
            occupancy_revision: self.occupancy_revision,
        }
    }
}

/// Whether this actor was sealed for destruction (#1108). Used by the
/// registry's [`super::room_registry_actor::ReapSealedRoom`] to purge
/// a sealed actor left registered by a timed-out guarded destroy.
pub struct IsSealed;

impl kameo::message::Message<IsSealed> for RoomActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        _msg: IsSealed,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.sealed
    }
}

/// The inactivity predicate a guarded destroy checks before sealing
/// the room actor (#1108).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealGuard {
    /// [`super::MucRoom::is_dormant`]: nothing in memory would be lost
    /// on eviction. Used by the room dormancy janitor for persistent
    /// rooms.
    Dormant,
    /// Zero occupants and `persistent == false` — the last-leave
    /// eviction of instant rooms (XEP-0045 §10.1.3), which may still
    /// hold a subject or the creator's Owner grant.
    EmptyNonPersistent,
}

/// Seal this room actor for destruction if it is still inactive at the
/// expected occupancy revision (#1108).
///
/// Sent by the registry from inside its serialized
/// `DestroyRoomIfInactive` handler. Because the room actor's mailbox
/// serializes this against `JoinWithAffiliation`/`Join`, the check is
/// race-free: a join processed after the janitor's probe either bumped
/// the revision or is queued behind this message and will be refused
/// by the `sealed` gate. Idempotent — an already-sealed actor confirms
/// again so a previously timed-out destroy can converge.
pub struct SealIfInactive {
    pub expected_occupancy_revision: u64,
    pub guard: SealGuard,
}

impl kameo::message::Message<SealIfInactive> for RoomActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: SealIfInactive,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.sealed {
            return true;
        }
        if self.occupancy_revision != msg.expected_occupancy_revision {
            return false;
        }
        let inactive = match msg.guard {
            SealGuard::Dormant => self.room.is_dormant(),
            SealGuard::EmptyNonPersistent => {
                self.room.occupants.is_empty() && !self.room.config.persistent
            }
        };
        if inactive {
            self.sealed = true;
        }
        inactive
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
            config_revision: self.config_revision,
            admission_revision: self.admission_revision,
        })
    }
}
