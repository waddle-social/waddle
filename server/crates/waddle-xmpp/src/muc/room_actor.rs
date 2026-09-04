//! Kameo actor wrapping a single MUC room.
//!
//! Each `RoomActor` owns a [`MucRoom`] and processes all operations
//! sequentially, removing the need for external `RwLock` synchronisation.
//! This is part of the Phase 3 actor-model migration.

use chrono::{DateTime, Utc};
use jid::{BareJid, FullJid};
use kameo::message::Context;
use kameo::Actor;
use std::{collections::HashMap, convert::Infallible};
use thiserror::Error;

use super::affiliation::AffiliationEntry;
use super::durable::{
    authorize_ephemeral_projection, mint_room_mutation_commit, ChannelId,
    EphemeralProjectionAuthorization, RoomCommitError, RoomDurableMutation, RoomMutationCommit,
    RoomProjection, RoomProjectionKind, WaddleId,
};
use super::pin::{PinStateChange, PinnedEntry};
use super::{MucRoom, OccupantVoiceChange, RoomConfig, RoomSubjectTexts, SubjectState};
use crate::types::{Affiliation, Role, Voice};

/// A join-path ownership proof is fail-closed, but it must not monopolize the
/// room actor forever when the durable backend stops responding.
pub(crate) const JOIN_OWNERSHIP_CHECK_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(1);

/// Maximum exact per-JID freshness watermarks retained by one room actor.
/// Overflow compacts safely into the room-wide watermark, trading only
/// cross-JID liveness for bounded memory while never accepting stale work.
const MAX_MEMBER_ADMISSION_REVISIONS: usize = 128;

mod admin_handlers;
mod mediated_invites;
mod occupancy_handlers;
mod snapshot_handlers;
#[cfg(test)]
mod tests;

pub use admin_handlers::{
    enforce_members_only_from_room, AdminItemsApplied, ApplyAdminItems, ApplyAffiliationChange,
    EnforceMembersOnly, EnforceMembersOnlyAffiliations, GetAdminContext, IsOwner,
};
use mediated_invites::MediatedInviteOperationRecord;
pub use mediated_invites::{
    AbortMediatedInviteGrantRollback, AcknowledgeMediatedInviteOperation, AuthorizeMediatedInvite,
    CommitMediatedInviteGrantRollback, FinalizeMediatedInviteGrant, InviteMembershipGrant,
    MediatedInviteAuthorized, MediatedInviteGrantError, MediatedInviteGrantFinalization,
    MediatedInviteOperationAcknowledgement, MediatedInviteOperationId, MediatedInviteRollbackAbort,
    MediatedInviteRollbackCommit, MediatedInviteRollbackError, MediatedInviteRollbackPreparation,
    PrepareMediatedInviteGrantRollback,
};
pub use occupancy_handlers::{
    next_occupancy_order, ClearMujiPresence, ClearMujiPresenceOutcome, GetActiveMujiSessions,
    InCallPresenceUpdateOutcome, JoinAffiliationGrant, JoinWithAffiliation, LeaveAttemptId,
    LeaveByRealJid, LeaveDisposition, LeaveOrigin, LeaveSessionSelector, MujiPresenceUpdateOutcome,
    OccupancyOrder, PingSelfCheck, PresenceUpdateData, ReconcileChannelBackedRoom,
    ResolverAffiliationSyncOutcome, SyncResolverAffiliation, UpsertInCallState, UpsertMujiPresence,
};
pub use waddle_xmpp_core::OccupancySessionGeneration;

/// A local occupancy generation used to avoid removing a replacement session
/// while retrying a previously deferred departure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct OccupancyWatermark(pub(crate) u64);

impl OccupancyWatermark {
    pub const fn initial() -> Self {
        Self(0)
    }

    pub const fn from_revision(revision: u64) -> Self {
        Self(revision)
    }
}

/// A completed departure retained for idempotent attempt replay. Retained
/// until the caller acknowledges it ([`AckDepartureReceipt`]) or, for a
/// caller whose reply was lost, until the retry replays it; transferred with
/// the live roster so a successor actor can still replay it.
#[derive(Debug, Clone)]
pub struct DepartureReceipt {
    pub attempt: occupancy_handlers::LeaveAttemptId,
    pub jid: FullJid,
    /// The departure's cause: an unknown-attempt fallback only replays a
    /// receipt of the same cause, so the caller runs the right effect policy.
    pub cause: super::durable::OccupancyLeaveCause,
    /// The departed session's position in the occupancy order: the receipt is
    /// stale (never replayed) once a newer generation of this full JID has
    /// existed, even if that generation has since left or was removed.
    pub generation: occupancy_handlers::OccupancyOrder,
    /// The nick's per-nickname occupancy generation when the departure
    /// completed: a non-final departure (siblings kept the nick) is only
    /// replayable while that same generation still holds the nick.
    pub nick_generation: Option<u64>,
    pub outcome: DepartureReceiptOutcome,
}

impl DepartureReceipt {
    /// The nickname the departure vacated (typed at the source).
    pub fn nick(&self) -> &crate::muc::MucOccupantNick {
        match &self.outcome {
            DepartureReceiptOutcome::Left(outcome) => &outcome.nick,
            DepartureReceiptOutcome::Suppressed { nick, .. } => nick,
        }
    }
}

/// A consumed departure receipt: replayable, or superseded by a newer
/// generation of the same full JID (joined since, whether it left, was
/// removed, or is still present).
#[derive(Debug)]
pub(super) enum RetainedDeparture {
    Current(DepartureReceipt),
    Stale,
}

/// Everything a successor needs to keep replaying lost-reply departures
/// correctly after a live-roster transfer: the receipts, the latest known
/// generation per full JID (advanced by every join and departure), and the
/// attempts that were superseded by a newer departure.
#[derive(Debug, Clone, Default)]
pub struct DepartureLedger {
    pub receipts: Vec<DepartureReceipt>,
    pub latest_generations: Vec<(FullJid, occupancy_handlers::OccupancyOrder)>,
    pub superseded_attempts: Vec<(FullJid, Vec<occupancy_handlers::LeaveAttemptId>)>,
}

/// What a retried attempt replays.
#[derive(Debug, Clone)]
pub enum DepartureReceiptOutcome {
    Left(Box<LeaveOutcome>),
    /// Store-less room with a destroy/dormancy in flight: only the leaver's
    /// XEP-0045 §7.14 self-presence is owed.
    Suppressed {
        nick: crate::muc::MucOccupantNick,
        affiliation: crate::types::Affiliation,
    },
}

/// Acknowledgement that a `LeaveByRealJid` reply was received and its effects
/// ran; the actor drops the receipt so only lost-reply departures stay
/// retained. Answered (not fire-and-forget) so the caller knows the receipt
/// is gone — mailbox admission alone would let a handoff snapshot queued
/// ahead of the acknowledgement carry the receipt to a successor.
pub struct AckDepartureReceipt {
    pub attempt: occupancy_handlers::LeaveAttemptId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub enum AckDepartureOutcome {
    /// The receipt (if any) is dropped on the authoritative actor.
    Acknowledged,
    /// This actor lost ownership (handoff in progress or done): its ledger
    /// may already have been copied to a successor, so the acknowledgement
    /// must be retained and delivered to whoever the registry names next.
    NotAuthoritative,
}

impl kameo::message::Message<AckDepartureReceipt> for RoomActor {
    type Reply = AckDepartureOutcome;

    async fn handle(
        &mut self,
        msg: AckDepartureReceipt,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.seal_state == RoomSealState::OwnershipLost {
            return AckDepartureOutcome::NotAuthoritative;
        }
        let _ = self.take_departure_receipt(msg.attempt);
        AckDepartureOutcome::Acknowledged
    }
}

/// A durable commit capability for exactly one in-memory projection.
pub(super) enum ProjectionGate {
    Unfenced,
    Authorized(EphemeralProjectionAuthorization),
}

/// Bounded outcome label for a failed projection commit.
const fn projection_commit_failure_label(error: &DurablePersistError) -> &'static str {
    match error {
        DurablePersistError::NotOwner => "not_owner",
        DurablePersistError::OwnershipUnavailable => "ownership_unavailable",
        DurablePersistError::PersistFailed => "persist_failed",
        DurablePersistError::CommitOutcomeUnknown => "commit_outcome_unknown",
    }
}

/// Test-only observation of projection state at a seam: `pre_commit` fires
/// inside `commit_projection` BEFORE the durable commit is awaited, `apply`
/// fires inside `project` immediately before the authorized closure runs.
/// Tests assert the state carried at `pre_commit` is untouched so a mutation
/// hoisted above the commit cannot pass.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectionProbe {
    pub phase: &'static str,
    pub occupants: usize,
    pub sessions: usize,
    pub pins: usize,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionRefusal {
    #[error("projection capability was minted under a different fence or lifecycle coordinates")]
    ForeignCapability,
    #[error("projection capability revision was already projected")]
    RevisionAlreadyProjected,
    #[error("projection capability was minted for a different projection kind")]
    WrongProjectionKind,
}
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
    /// Exact durable-ownership proof retained by this actor incarnation.
    /// Callers that make room-authorized decisions from this snapshot must
    /// retain this immutable context instead of consulting mutable registry
    /// state after the actor read completes.
    pub claim_fence: Option<super::durable::RoomClaimFenceContext>,
    pub durable_coordinates: Option<super::durable::RoomCommittedCoordinates>,
    /// Exact durable coordinates for the revision that most recently changed
    /// this actor's room config. Kept distinct from `durable_coordinates`
    /// because later affiliation/admin commits advance the room lifecycle
    /// without owning the config-change outbox row.
    pub config_durable_coordinates: Option<super::durable::RoomCommittedCoordinates>,
    pub config_revision: u64,
    pub admission_revision: u64,
    pub occupancy_revision: u64,
    /// Lost-reply departure state, transferred on live-roster recovery.
    pub departures: DepartureLedger,
}

/// Transfer the live, non-durable room state into an authoritative successor.
/// Durable configuration, subject, and affiliations already installed on the
/// successor remain authoritative; only the roster and its ephemeral state
/// are restored.
pub struct RestoreLiveRoster {
    pub room: MucRoom,
    pub occupancy_revision: u64,
    /// The predecessor's lost-reply departure state, so a retry that lands on
    /// the successor replays (or refuses) exactly as the predecessor would.
    pub departures: DepartureLedger,
}

impl kameo::message::Message<RestoreLiveRoster> for RoomActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: RestoreLiveRoster,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let occupant_count_before = self.room.occupant_count();
        let config = self.room.config.clone();
        let subject = self.room.subject.clone();
        let affiliations = self.room.affiliation_list.clone();
        let mut restored = msg.room;
        // The transplant restores THIS room's live roster; its identity is
        // never the restored value's (durable commits key their claim fence
        // by the actor's room JID).
        restored.room_jid = self.room.room_jid.clone();
        restored.config = config;
        restored.subject = subject;
        restored.affiliation_list = affiliations;
        let restored_occupants = restored
            .occupants
            .iter()
            .map(|(nick, occupant)| (nick.clone(), occupant.real_jid.to_bare()))
            .collect::<Vec<_>>();
        for (nick, jid) in restored_occupants {
            let affiliation = restored.get_affiliation(&jid);
            let role = restored.derive_role_from_affiliation(affiliation);
            if let Some(occupant) = restored.occupants.get_mut(&nick) {
                occupant.affiliation = affiliation;
                occupant.role = role;
            }
        }
        self.room = restored;
        // The predecessor's `Drop` releases its occupants from the pod-wide
        // gauge; the transplanted roster is this actor's to account for now.
        crate::metrics::adjust_muc_occupant_total(
            self.room.occupant_count() as i64 - occupant_count_before as i64,
        );
        self.occupancy_revision = self.occupancy_revision.max(msg.occupancy_revision);
        self.absorb_departure_ledger(msg.departures);
    }
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
    /// The attempt whose receipt this outcome answers: the caller's own
    /// attempt, or — when a retained retry replayed a receipt minted under a
    /// different (coalesced-away) attempt — that receipt's attempt. Callers
    /// acknowledge THIS id once the effects ran; replay never consumes the
    /// receipt by itself.
    pub acknowledge: occupancy_handlers::LeaveAttemptId,
    pub nick: crate::muc::MucOccupantNick,
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
    #[error("invitee affiliation is fenced pending invite rollback acknowledgement")]
    InviteRollbackPending,
    #[error("{0}")]
    PermissionDenied(String),
    /// ADR-0017 Phase 3 Slice 7 FIX 2 (council-adjudicated): the
    /// pre-mutation fencing gate ([`RoomActor::gate_mutation`]) observed
    /// that this node no longer holds the room's ownership claim. The
    /// mutation this ask requested was NEVER APPLIED. The caller MUST
    /// trigger `RoomLocalClaims::demote` for this room so the non-serving
    /// actor stops serving, and surface a conformant, recoverable error
    /// to the requester (mirroring `dispatch_to_room`'s
    /// `<resource-constraint/>` bounce).
    #[error("this room is no longer serviceable by this actor")]
    NotOwner,
    /// The exact ownership check could not establish either ownership or
    /// loss. The requested mutation was never applied and may be retried.
    #[error("this room's ownership is temporarily unavailable")]
    OwnershipUnavailable,
    #[error("durable room mutation commit failed before the in-memory mutation")]
    PersistFailed,
    #[error("durable room mutation commit outcome could not be reconciled")]
    CommitOutcomeUnknown,
}

impl From<RoomMutationError> for AdminApplyError {
    fn from(error: RoomMutationError) -> Self {
        match error {
            RoomMutationError::NotOwner => AdminApplyError::NotOwner,
            RoomMutationError::OwnershipUnavailable => AdminApplyError::OwnershipUnavailable,
            RoomMutationError::PersistFailed => AdminApplyError::PersistFailed,
            RoomMutationError::CommitOutcomeUnknown => AdminApplyError::CommitOutcomeUnknown,
        }
    }
}

impl From<DurablePersistError> for AdminApplyError {
    fn from(error: DurablePersistError) -> Self {
        match error {
            DurablePersistError::NotOwner => AdminApplyError::NotOwner,
            DurablePersistError::OwnershipUnavailable => AdminApplyError::OwnershipUnavailable,
            DurablePersistError::PersistFailed => AdminApplyError::PersistFailed,
            DurablePersistError::CommitOutcomeUnknown => AdminApplyError::CommitOutcomeUnknown,
        }
    }
}

/// Typed failures from a durable-first room mutation. Every variant leaves
/// the actor's in-memory state and emitted effects unchanged.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RoomMutationError {
    #[error("this room is no longer serviceable by this actor")]
    NotOwner,
    #[error("this room's ownership is temporarily unavailable")]
    OwnershipUnavailable,
    #[error("durable room mutation commit failed before the in-memory mutation")]
    PersistFailed,
    #[error("durable room mutation commit outcome could not be reconciled")]
    CommitOutcomeUnknown,
}

/// The ownership result of the pre-mutation gate. This phase cannot report
/// persistence or post-apply ownership outcomes because no mutation has run.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
enum PreMutationOwnershipError {
    #[error("this room is no longer serviceable by this actor")]
    NotOwner,
    #[error("this room's ownership is temporarily unavailable")]
    OwnershipUnavailable,
}

impl From<PreMutationOwnershipError> for RoomMutationError {
    fn from(error: PreMutationOwnershipError) -> Self {
        match error {
            PreMutationOwnershipError::NotOwner => Self::NotOwner,
            PreMutationOwnershipError::OwnershipUnavailable => Self::OwnershipUnavailable,
        }
    }
}

/// Failure from an affiliation mutation that can collide with an
/// in-progress mediated-invite compensation.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum AffiliationMutationError {
    #[error("this room is no longer serviceable by this actor")]
    NotOwner,
    #[error("this room's ownership is temporarily unavailable")]
    OwnershipUnavailable,
    #[error("durable room mutation commit failed before the in-memory mutation")]
    PersistFailed,
    #[error("durable room mutation commit outcome could not be reconciled")]
    CommitOutcomeUnknown,
    #[error("invitee affiliation is fenced pending invite rollback acknowledgement")]
    InviteRollbackPending,
}

impl From<RoomMutationError> for AffiliationMutationError {
    fn from(error: RoomMutationError) -> Self {
        match error {
            RoomMutationError::NotOwner => Self::NotOwner,
            RoomMutationError::OwnershipUnavailable => Self::OwnershipUnavailable,
            RoomMutationError::PersistFailed => Self::PersistFailed,
            RoomMutationError::CommitOutcomeUnknown => Self::CommitOutcomeUnknown,
        }
    }
}

impl From<DurablePersistError> for AffiliationMutationError {
    fn from(error: DurablePersistError) -> Self {
        match error {
            DurablePersistError::NotOwner => Self::NotOwner,
            DurablePersistError::OwnershipUnavailable => Self::OwnershipUnavailable,
            DurablePersistError::PersistFailed => Self::PersistFailed,
            DurablePersistError::CommitOutcomeUnknown => Self::CommitOutcomeUnknown,
        }
    }
}

/// Typed outcome of a durable room-mutation commit before its corresponding
/// in-memory mutation.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum DurablePersistError {
    #[error("this actor became non-serving before the in-memory mutation was applied")]
    NotOwner,
    #[error("this room's exact ownership fence is unavailable")]
    OwnershipUnavailable,
    #[error("durable persist failed")]
    PersistFailed,
    #[error("durable room mutation commit outcome could not be reconciled")]
    CommitOutcomeUnknown,
}

impl From<DurablePersistError> for RoomMutationError {
    fn from(error: DurablePersistError) -> Self {
        match error {
            DurablePersistError::NotOwner => RoomMutationError::NotOwner,
            DurablePersistError::OwnershipUnavailable => RoomMutationError::OwnershipUnavailable,
            DurablePersistError::PersistFailed => RoomMutationError::PersistFailed,
            DurablePersistError::CommitOutcomeUnknown => RoomMutationError::CommitOutcomeUnknown,
        }
    }
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
    durable_coordinates: Option<super::durable::RoomCommittedCoordinates>,
    config_durable_coordinates: Option<super::durable::RoomCommittedCoordinates>,
    config_revision: u64,
    /// Monotonic sequence used to timestamp admission-relevant snapshots.
    /// Scope-specific watermarks below decide whether a snapshot is stale;
    /// an unrelated member mutation may advance this sequence without
    /// invalidating another member's pending resolver repair.
    admission_revision: u64,
    /// Latest admission revision that changed room-wide admission policy.
    room_admission_revision: u64,
    /// Latest admission revision that changed each bare JID's admission or
    /// affiliation state.
    member_admission_revisions: HashMap<BareJid, u64>,
    invite_operations: HashMap<MediatedInviteOperationId, MediatedInviteOperationRecord>,
    invite_operation_by_invitee: HashMap<BareJid, MediatedInviteOperationId>,
    /// Monotonically increasing counter bumped on every successful
    /// admission (#1108). The dormancy probe ([`IsDormant`]) returns
    /// it and the registry's guarded destroy
    /// ([`super::room_registry_actor::DestroyRoomIfInactive`]) refuses
    /// when it moved — a join that landed after the janitor's probe
    /// makes the probe's revision stale, closing the probe→destroy
    /// TOCTOU that orphaned freshly-admitted occupants.
    occupancy_revision: u64,
    /// The newest durable revision consumed by an ephemeral projection.
    projected_revision: Option<super::durable::RoomRevision>,
    /// Completed departures retained for attempt replay (see
    /// [`occupancy_handlers::LeaveAttemptId`]).
    departure_receipts: std::collections::VecDeque<DepartureReceipt>,
    /// Latest known generation per full JID (advanced by every join and every
    /// departure) while a receipt of that JID is retained; pruned with the
    /// last receipt.
    latest_generations: std::collections::HashMap<FullJid, occupancy_handlers::OccupancyOrder>,
    /// Attempts displaced by a newer retained departure of the same full
    /// JID. While that newer receipt exists, a late retry of one of these
    /// attempts must never consume the newer receipt through JID fallback.
    superseded_departure_attempts: std::collections::HashMap<
        FullJid,
        std::collections::HashSet<occupancy_handlers::LeaveAttemptId>,
    >,
    /// Why this actor refuses further admissions. Keeping the reason typed
    /// lets the registry distinguish an ordinary inactivity seal, whose
    /// removal must retain exact-release backlog fencing, from a definitive
    /// ownership-loss seal, whose non-serving local actor must be evicted even
    /// when that backlog is full.
    seal_state: RoomSealState,
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
    /// Durable ownership state store (ADR-0017 Phase 3 Slice 7), received
    /// in [`RestoreDurableRoomState`] — the first message the room
    /// registry enqueues after spawning/re-claiming this actor when a
    /// store is configured. `None` in single-node/non-clustering
    /// deployments, matching today's purely in-memory behavior exactly.
    /// Every config/subject/affiliation-mutating handler awaits persistence
    /// through this handle when it is `Some` and classifies a failure by
    /// whether its in-memory mutation has already applied.
    durable_store: Option<std::sync::Arc<dyn super::durable::MucDurableStore>>,
    /// Exact ownership tuple retained by this actor incarnation. Durable
    /// operations must never borrow a replacement actor's cached claim.
    durable_claim_fence: Option<super::durable::RoomClaimFenceContext>,
    /// ADR-0017 Phase 3 Slice 7 FIX 4 (council-adjudicated): whether this
    /// actor incarnation's durable restore genuinely completed. See
    /// [`DurableRestoreState`]'s own doc comment.
    restore_state: DurableRestoreState,
    #[cfg(test)]
    test_projection_apply_hook: Option<std::sync::Arc<dyn Fn(ProjectionProbe) + Send + Sync>>,
}

#[derive(PartialEq)]
struct AdmissionPolicySnapshot {
    waddle_id: String,
    channel_id: String,
    requires_membership: bool,
    max_occupants: u32,
    affiliations: HashMap<BareJid, Affiliation>,
}

/// ADR-0017 Phase 3 Slice 7 FIX 4 (council-adjudicated): fail-closed
/// tracking for [`RestoreDurableRoomState`]. `Ready(origin)` means either no
/// durable store is configured (single-node/non-clustering, therefore a new
/// in-memory room) or the restore genuinely completed. The typed origin
/// preserves whether the fenced load returned an existing snapshot or proved
/// that no durable room exists; the registry uses it to keep XEP-0045 creator
/// ownership and status 201 exclusive to actual room creation. `Pending`
/// means the initial load hit a
/// genuine backend error: joins are refused (a typed, recoverable bounce)
/// until [`RoomActor::ensure_restored_before_join`]'s bounded inline retry
/// succeeds — never silently served with the caller-supplied defaults,
/// which would look exactly like a legitimate empty new room while
/// actually discarding every ban/member/owner grant on record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurableRestoreState {
    Ready(DurableRoomOrigin),
    Pending,
    /// A fenced durable load definitively proved this actor incarnation was
    /// non-serving. This terminal state must never retry with the stale fence.
    OwnershipLost,
}

impl Default for DurableRestoreState {
    fn default() -> Self {
        Self::Ready(DurableRoomOrigin::New)
    }
}

/// Why a room actor has stopped accepting admissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, kameo::Reply)]
pub enum RoomSealState {
    /// The actor is live and may accept admissions.
    #[default]
    Open,
    /// The registry sealed an inactive actor before a terminal local removal.
    Inactive,
    /// The registry is committing a terminal destroy under this typed
    /// attempt. New mutations must retry through the registry.
    Destroying {
        attempt: crate::muc::durable::DestroyAttemptId,
    },
    /// The durable ownership gate proved this incarnation is non-serving.
    OwnershipLost,
}

impl RoomSealState {
    fn is_sealed(self) -> bool {
        self != Self::Open
    }
}

/// Why a join was refused, so the presence-error mapping can pick the
/// XEP-0045 §7.2 condition: §7.2.8 bans (outcast) are `<forbidden/>`
/// even in members-only rooms, while a plain non-member hitting a
/// members-only room is `<registration-required/>` (#1265 item 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinDenialReason {
    /// The user is banned (affiliation = outcast).
    Banned,
    /// The room is members-only and the user is not a member.
    MembersOnly,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RoomActorError {
    #[error("room is full")]
    RoomFull,
    #[error("nick '{0}' already in use")]
    NickAlreadyInUse(String),
    #[error("no occupant with nick '{0}'")]
    OccupantNotFound(String),
    #[error("join is forbidden ({reason:?})")]
    JoinForbidden { reason: JoinDenialReason },
    #[error("join admission snapshot is stale")]
    StaleAdmissionRevision,
    #[error("join is blocked pending invite membership rollback acknowledgement")]
    InviteRollbackPending,
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
    /// ADR-0017 Phase 3 Slice 7 FIX 4 (council-adjudicated): the initial
    /// [`RestoreDurableRoomState`] load failed (a genuine backend error,
    /// NOT the `Ok(None)` "brand new room" case) and a retry — run inline
    /// on this very join attempt — failed again. Joins are refused until a
    /// later attempt's retry succeeds, rather than risk serving with
    /// defaulted config/affiliations/subject and silently losing every
    /// ban/member/owner grant actually on record. Maps to a conformant,
    /// recoverable `<error type='wait'><resource-constraint/></error>`
    /// (the caller's join path), mirroring `dispatch_to_room`'s identical
    /// ownership-gap bounce.
    #[error("durable room-state restore has not yet completed; retry")]
    RestorePending,
    /// An admission could not prove that this actor still owns the room.
    /// The caller should return a wait-class error and retry via the registry
    /// rather than admitting into an uncertain incarnation.
    #[error("room ownership could not be confirmed; retry")]
    OwnershipUnavailable,
}

impl Drop for RoomActor {
    fn drop(&mut self) {
        // Compensate the pod-wide `xmpp.muc.occupants` gauge for any
        // occupants still present when this actor tears down — owner
        // destroy, dormancy/seal eviction, and supervision replacement
        // all bypass `LeaveByRealJid`, so join/leave deltas alone would
        // inflate the process-lifetime total forever. `Drop` is the one
        // seam every teardown path shares.
        let remaining = self.room.occupants.len();
        if remaining > 0 {
            crate::metrics::adjust_muc_occupant_total(-(remaining as i64));
        }
    }
}

impl RoomActor {
    /// Create a new `RoomActor` wrapping the given room.
    pub fn new(
        mut room: MucRoom,
        occupant_id_secret: crate::xep::xep0421::OccupantIdSecret,
    ) -> Self {
        room.config = room.config.normalized();
        Self {
            room,
            durable_coordinates: None,
            config_durable_coordinates: None,
            config_revision: 0,
            admission_revision: 0,
            room_admission_revision: 0,
            member_admission_revisions: HashMap::new(),
            invite_operations: HashMap::new(),
            invite_operation_by_invitee: HashMap::new(),
            occupancy_revision: 0,
            projected_revision: None,
            departure_receipts: std::collections::VecDeque::new(),
            latest_generations: std::collections::HashMap::new(),
            superseded_departure_attempts: std::collections::HashMap::new(),
            seal_state: RoomSealState::Open,
            occupant_id_secret,
            durable_member_recipients: Vec::new(),
            membership_source: None,
            durable_store: None,
            durable_claim_fence: None,
            restore_state: DurableRestoreState::Ready(DurableRoomOrigin::New),
            #[cfg(test)]
            test_projection_apply_hook: None,
        }
    }

    /// Install one authoritative durable snapshot and advance the room-wide
    /// admission watermark when the restored policy differs from the
    /// constructor/default state. Subject changes are deliberately excluded:
    /// they do not affect whether or how an occupant may enter.
    fn install_durable_room_state(&mut self, state: super::durable::DurableRoomState) {
        let previous_admission_state = self.admission_policy_snapshot();
        self.durable_coordinates = state.coordinates;
        self.config_durable_coordinates = state.config_coordinates;
        self.projected_revision = state.coordinates.map(|coordinates| coordinates.revision);
        self.room.waddle_id = state.waddle_id;
        self.room.channel_id = state.channel_id;
        self.replace_config(state.config);
        self.room.subject = state.subject;
        self.room.restore_affiliations(state.affiliations);
        let installed_admission_state = self.admission_policy_snapshot();
        if installed_admission_state != previous_admission_state {
            self.advance_room_admission_revision();
        }
    }

    fn admission_policy_snapshot(&self) -> AdmissionPolicySnapshot {
        AdmissionPolicySnapshot {
            waddle_id: self.room.waddle_id.clone(),
            channel_id: self.room.channel_id.clone(),
            requires_membership: self.room.config.requires_membership(),
            max_occupants: self.room.config.max_occupants,
            affiliations: self
                .room
                .get_all_affiliations()
                .into_iter()
                .map(|entry| (entry.jid, entry.affiliation))
                .collect(),
        }
    }

    fn advance_room_admission_revision(&mut self) {
        self.admission_revision = self.admission_revision.saturating_add(1);
        self.room_admission_revision = self.admission_revision;
        self.member_admission_revisions.clear();
    }

    fn advance_member_admission_revision(&mut self, jid: &BareJid) {
        self.admission_revision = self.admission_revision.saturating_add(1);
        if !self.member_admission_revisions.contains_key(jid)
            && self.member_admission_revisions.len() >= MAX_MEMBER_ADMISSION_REVISIONS
        {
            // Promote the new sequence value to a conservative room-wide
            // fence before discarding exact member watermarks. Every older
            // snapshot remains stale; only unrelated pending work may retry.
            self.room_admission_revision = self.admission_revision;
            self.member_admission_revisions.clear();
        }
        self.member_admission_revisions
            .insert(jid.clone(), self.admission_revision);
    }

    fn admission_revision_is_current(&self, jid: &BareJid, revision: u64) -> bool {
        let member_revision = self
            .member_admission_revisions
            .get(jid)
            .copied()
            .unwrap_or(0);
        revision <= self.admission_revision
            && revision >= self.room_admission_revision
            && revision >= member_revision
    }

    /// ADR-0017 Phase 3 Slice 7 FIX 2 (council-adjudicated): the
    /// pre-mutation ownership gate every durable-relevant mutation handler
    /// runs BEFORE touching in-memory state. `Ok(())` when unfenced (no
    /// durable store configured — single-node/non-clustering, byte
    /// -identical to today) or when the fenced check confirms this node
    /// still holds the claim. `Err(RoomMutationError::NotOwner)` when the
    /// fenced check observes 0 rows — the caller MUST NOT mutate.
    ///
    /// Backend failures fail closed without marking the actor non-serving: the
    /// exact fence could not be proven, so applying an in-memory mutation
    /// would let state diverge from its durable authority.
    async fn gate_pre_mutation_ownership(&mut self) -> Result<(), PreMutationOwnershipError> {
        // A definitive ownership-loss observation is monotonic for this
        // actor incarnation. Do not let a later transient store failure
        // override that proof through a later uncertain probe while the
        // registry is still converging the non-serving actor's removal.
        match self.seal_state {
            RoomSealState::OwnershipLost => {
                return Err(PreMutationOwnershipError::NotOwner);
            }
            // A sealed actor must emit no new effects: `Destroying` means
            // the registry is committing a terminal destroy (callers retry
            // through the registry) and `Inactive` means it is converging a
            // dormancy eviction. Zero-durable-delta mutations reach only
            // this gate, so refusing here is what makes the pre-seal
            // actually prevent kicks/presence/SFU effects mid-destroy.
            RoomSealState::Destroying { .. } | RoomSealState::Inactive => {
                return Err(PreMutationOwnershipError::OwnershipUnavailable);
            }
            RoomSealState::Open => {}
        }
        let Some(store) = self.durable_store.clone() else {
            return Ok(());
        };
        let Some(fence) = self.durable_claim_fence.as_ref() else {
            return Err(PreMutationOwnershipError::OwnershipUnavailable);
        };
        match store
            .check_exact_claim_fence(&self.room.room_jid, fence)
            .await
        {
            Ok(true) => Ok(()),
            Ok(false) => {
                self.seal_state = RoomSealState::OwnershipLost;
                tracing::warn!(
                    room = %self.room.room_jid,
                    "mutation gate observed ownership loss; refusing to mutate"
                );
                Err(PreMutationOwnershipError::NotOwner)
            }
            Err(error) => {
                tracing::warn!(
                    room = %self.room.room_jid,
                    %error,
                    "mutation gate could not prove the actor's exact ownership; refusing mutation"
                );
                Err(PreMutationOwnershipError::OwnershipUnavailable)
            }
        }
    }

    async fn gate_mutation(&mut self) -> Result<(), RoomMutationError> {
        self.gate_pre_mutation_ownership().await.map_err(Into::into)
    }

    /// ADR-0017 Phase 3 Slice 7 FIX 4 (council-adjudicated): the
    /// fail-closed join gate. `Ok(())` when this incarnation's durable
    /// restore is already known-good (`DurableRestoreState::Ready(_)` — the
    /// overwhelmingly common case). When `Pending` (the initial
    /// [`RestoreDurableRoomState`] load failed), runs ONE bounded inline
    /// retry against the same store before deciding: success applies the
    /// restored state (if any), flips back to `Ready`, and advances the
    /// room-wide admission fence so this join re-snapshots the recovered
    /// policy; a repeated failure returns
    /// `Err(RoomActorError::RestorePending)` and leaves the state
    /// `Pending` for the next join attempt to retry again. Never serves a
    /// join against defaulted config/affiliations/subject while a
    /// genuine restore failure is unresolved.
    async fn ensure_restored_before_join(&mut self) -> Result<(), RoomActorError> {
        match self.restore_state {
            DurableRestoreState::Ready(_) => return Ok(()),
            DurableRestoreState::OwnershipLost => return Err(RoomActorError::RoomSealed),
            DurableRestoreState::Pending => {}
        }
        let Some(store) = self.durable_store.clone() else {
            // Defensive: `Pending` is only ever set from inside the
            // `RestoreDurableRoomState` handler, which always also sets
            // `durable_store = Some(..)` in the same message. Treat a
            // missing store as nothing-to-restore rather than wedging
            // every future join on an invariant violation.
            self.restore_state = DurableRestoreState::Ready(DurableRoomOrigin::New);
            return Ok(());
        };
        let Some(fence) = self.durable_claim_fence.as_ref() else {
            return Err(RoomActorError::RestorePending);
        };
        match store
            .load_room_state_fenced(&self.room.room_jid, fence)
            .await
        {
            Ok(Some(state)) => {
                self.install_durable_room_state(state);
                self.restore_state = DurableRestoreState::Ready(DurableRoomOrigin::Restored);
                Ok(())
            }
            Ok(None) => {
                self.restore_state = DurableRestoreState::Ready(DurableRoomOrigin::New);
                Ok(())
            }
            Err(crate::XmppError::OwnershipLost { entity }) => {
                self.restore_state = DurableRestoreState::OwnershipLost;
                self.seal_state = RoomSealState::OwnershipLost;
                tracing::warn!(
                    room = %self.room.room_jid,
                    %entity,
                    "durable room-state restore retry lost this actor's exact ownership"
                );
                Err(RoomActorError::RoomSealed)
            }
            Err(error) => {
                tracing::warn!(
                    room = %self.room.room_jid,
                    %error,
                    "durable room-state restore retry failed again; refusing this \
                     join attempt (fail-closed, FIX 4)"
                );
                Err(RoomActorError::RestorePending)
            }
        }
    }

    /// A join does not naturally pass through the mutation fence before it
    /// admits an occupant. Fail closed here to keep a non-serving but
    /// still-referenced actor from admitting either a returning occupant or
    /// the creator whose registry acquisition raced an ownership change.
    async fn gate_join_ownership(&mut self) -> Result<(), RoomActorError> {
        let Some(store) = self.durable_store.clone() else {
            return Ok(());
        };
        let Some(fence) = self.durable_claim_fence.as_ref() else {
            return Err(RoomActorError::OwnershipUnavailable);
        };
        match tokio::time::timeout(
            JOIN_OWNERSHIP_CHECK_TIMEOUT,
            store.check_exact_claim_fence(&self.room.room_jid, fence),
        )
        .await
        {
            Ok(Ok(true)) => Ok(()),
            Ok(Ok(false)) => {
                self.seal_state = RoomSealState::OwnershipLost;
                Err(RoomActorError::RoomSealed)
            }
            Ok(Err(error)) => {
                tracing::warn!(
                    room = %self.room.room_jid,
                    %error,
                    "join ownership gate failed; refusing admission"
                );
                Err(RoomActorError::OwnershipUnavailable)
            }
            Err(_) => {
                tracing::warn!(
                    room = %self.room.room_jid,
                    timeout_ms = JOIN_OWNERSHIP_CHECK_TIMEOUT.as_millis(),
                    "join ownership gate timed out; refusing admission"
                );
                Err(RoomActorError::OwnershipUnavailable)
            }
        }
    }

    /// Refuse effectful external work through an already sealed actor while
    /// allowing an inactivity seal to strengthen into a definitive
    /// ownership-loss seal.
    ///
    /// The ownership probe's result never makes an inactive actor usable
    /// again: it only preserves the stronger cause for the registry's
    /// reaper. A transient probe failure therefore still leaves the original
    /// inactivity seal intact.
    async fn effectful_work_is_permitted(&mut self) -> bool {
        match self.seal_state {
            RoomSealState::Open => true,
            RoomSealState::OwnershipLost => false,
            RoomSealState::Inactive => {
                let _ = self.gate_join_ownership().await;
                false
            }
            RoomSealState::Destroying { .. } => false,
        }
    }

    async fn reject_sealed_effects(&mut self) -> Result<(), RoomActorError> {
        if self.effectful_work_is_permitted().await {
            return Ok(());
        }
        match self.seal_state {
            RoomSealState::Destroying { .. } => Err(RoomActorError::OwnershipUnavailable),
            RoomSealState::Open => Ok(()),
            RoomSealState::OwnershipLost | RoomSealState::Inactive => {
                Err(RoomActorError::RoomSealed)
            }
        }
    }

    async fn reject_sealed_join(&mut self) -> Result<(), RoomActorError> {
        self.reject_sealed_effects().await
    }

    fn classify_durable_persist_error(&mut self, error: RoomCommitError) -> DurablePersistError {
        match error {
            // A lost COMMIT acknowledgement is not a rollback proof: the
            // durable mutation may have advanced while this actor still has
            // its pre-commit memory.  Reuse the terminal, registry-retire
            // compatible NotOwner contract rather than allowing this stale
            // incarnation to answer another request.  The caller demotes
            // this exact actor and a fresh incarnation restores durable
            // state before serving again.
            RoomCommitError::CommitOutcomeUnknown => {
                self.seal_state = RoomSealState::OwnershipLost;
                tracing::warn!(
                    room = %self.room.room_jid,
                    "durable mutation commit outcome is unknown; sealing stale actor for retirement"
                );
                DurablePersistError::CommitOutcomeUnknown
            }
            RoomCommitError::NotOwner => {
                self.seal_state = RoomSealState::OwnershipLost;
                tracing::warn!(
                    room = %self.room.room_jid,
                    "durable mutation commit lost exact ownership"
                );
                DurablePersistError::NotOwner
            }
            RoomCommitError::OwnershipUnavailable | RoomCommitError::RetryExhausted => {
                tracing::warn!(
                    room = %self.room.room_jid,
                    "durable mutation commit could not prove ownership"
                );
                DurablePersistError::OwnershipUnavailable
            }
            error => {
                tracing::warn!(
                    room = %self.room.room_jid,
                    %error,
                    "durable mutation commit failed before applying memory"
                );
                DurablePersistError::PersistFailed
            }
        }
    }

    /// Commits authoritative room state before the caller projects it into
    /// actor memory. Store-less deployments retain their in-memory behavior.
    async fn commit_durable(
        &mut self,
        intent: RoomDurableMutation,
        effects: super::RoomMutationEffects,
    ) -> Result<
        (
            Option<RoomMutationCommit>,
            Option<super::RoomEffectReservation>,
        ),
        DurablePersistError,
    > {
        match self.seal_state {
            RoomSealState::Open => {}
            RoomSealState::OwnershipLost => return Err(DurablePersistError::NotOwner),
            RoomSealState::Inactive => return Err(DurablePersistError::OwnershipUnavailable),
            RoomSealState::Destroying { .. } => {
                return Err(DurablePersistError::OwnershipUnavailable);
            }
        }
        let Some(store) = self.durable_store.clone() else {
            return Ok((None, None));
        };
        let fence = self
            .durable_claim_fence
            .clone()
            .ok_or(DurablePersistError::OwnershipUnavailable)?;
        let kind = super::durable::RoomCommitKind::of(&intent);
        let outcome = store
            .commit_room_mutation(&self.room.room_jid, &fence, intent, effects)
            .await
            .map_err(|error| self.classify_durable_persist_error(error))?;
        let commit = mint_room_mutation_commit(fence, outcome.coordinates, kind);
        self.durable_coordinates = Some(outcome.coordinates);
        Ok((Some(commit), outcome.reservation))
    }

    #[cfg(test)]
    fn projection_probe(&self, phase: &'static str) -> ProjectionProbe {
        ProjectionProbe {
            phase,
            occupants: self.room.occupant_count(),
            sessions: self
                .room
                .occupants
                .values()
                .map(|occupant| self.room.get_occupant_sessions(&occupant.nick).len())
                .sum(),
            pins: self.room.pinned_entries().len(),
        }
    }

    pub(super) async fn commit_projection(
        &mut self,
        projection: RoomProjection,
    ) -> Result<ProjectionGate, DurablePersistError> {
        let kind = projection.kind();
        #[cfg(test)]
        if let Some(hook) = self.test_projection_apply_hook.clone() {
            hook(self.projection_probe("pre_commit"));
        }
        let started = std::time::Instant::now();
        let committed = self
            .commit_durable(
                RoomDurableMutation::Projection(projection),
                super::RoomMutationEffects::none(),
            )
            .await;
        crate::metrics::record_muc_projection_commit_duration(
            kind.as_str(),
            started.elapsed().as_secs_f64(),
        );
        let (commit, _) = match committed {
            Ok(committed) => committed,
            Err(error) => {
                crate::metrics::record_muc_projection_commit(
                    kind.as_str(),
                    projection_commit_failure_label(&error),
                );
                return Err(error);
            }
        };
        match commit {
            None => {
                crate::metrics::record_muc_projection_commit(kind.as_str(), "unfenced");
                Ok(ProjectionGate::Unfenced)
            }
            Some(commit) => match authorize_ephemeral_projection(commit) {
                Ok(authorization) => {
                    crate::metrics::record_muc_projection_commit(kind.as_str(), "ok");
                    Ok(ProjectionGate::Authorized(authorization))
                }
                Err(_) => {
                    crate::metrics::record_muc_projection_commit(kind.as_str(), "not_projection");
                    Err(DurablePersistError::PersistFailed)
                }
            },
        }
    }

    /// Record that a session of `jid` joined at `generation`: every retained
    /// departure receipt of that JID is now stale, whatever happens to the
    /// new session later (leave, kick, ban, transfer).
    pub(super) fn note_session_joined(&mut self, jid: &FullJid) {
        let Some(generation) = self.room.session_order(jid) else {
            return;
        };
        if self
            .departure_receipts
            .iter()
            .any(|receipt| &receipt.jid == jid)
        {
            let latest = self
                .latest_generations
                .entry(jid.clone())
                .or_insert(generation);
            *latest = (*latest).max(generation);
        }
    }

    /// Retain a completed departure so a retry of the same attempt (after a
    /// lost reply) can replay its outcome exactly once. Acknowledged receipts
    /// are dropped at once and a newer departure of the same full JID AND
    /// nick supersedes every older receipt of that (JID, nick), so the
    /// retained set is bounded by (full JID, nick) pairs with a lost reply.
    /// A different-nick departure keeps the older receipt: each nickname's
    /// unavailable fan-out is independently owed to the remaining occupants
    /// (#1647, codex round 26).
    pub(super) fn retain_departure_receipt(&mut self, receipt: DepartureReceipt) {
        // The same nick-aware rule as `replay_departure_receipt_at`: a newer
        // per-JID generation alone does not supersede — a transferred (or
        // late-minted) receipt whose NICK generation has not moved still owes
        // its unavailable fan-out (#1647, codex round 29). An existing
        // same-nick receipt with a NEWER generation always wins, though: an
        // older transferred copy must never displace it.
        let displaced_by_newer_same_nick = self.departure_receipts.iter().any(|retained| {
            retained.jid == receipt.jid
                && retained.nick() == receipt.nick()
                && retained.generation > receipt.generation
        });
        let superseded = displaced_by_newer_same_nick
            || (self
                .latest_generations
                .get(&receipt.jid)
                .is_some_and(|latest| *latest > receipt.generation)
                && self
                    .room
                    .current_nickname_generation(receipt.nick().as_str())
                    != receipt.nick_generation);
        if superseded {
            self.superseded_departure_attempts
                .entry(receipt.jid)
                .or_default()
                .insert(receipt.attempt);
            return;
        }
        let displaced_attempts: Vec<_> = self
            .departure_receipts
            .iter()
            .filter(|retained| retained.jid == receipt.jid && retained.nick() == receipt.nick())
            .map(|retained| retained.attempt)
            .collect();
        if !displaced_attempts.is_empty() {
            self.superseded_departure_attempts
                .entry(receipt.jid.clone())
                .or_default()
                .extend(displaced_attempts);
        }
        self.departure_receipts
            .retain(|retained| retained.jid != receipt.jid || retained.nick() != receipt.nick());
        self.latest_generations
            .insert(receipt.jid.clone(), receipt.generation);
        self.departure_receipts.push_back(receipt);
    }

    /// Replay the receipt of an already-completed attempt WITHOUT consuming
    /// it: only an answered acknowledgement removes a current receipt (the
    /// replay's own reply may be lost too). A stale receipt is dropped here.
    pub(super) fn replay_departure_receipt(
        &mut self,
        attempt: occupancy_handlers::LeaveAttemptId,
    ) -> Option<RetainedDeparture> {
        let index = self
            .departure_receipts
            .iter()
            .position(|receipt| receipt.attempt == attempt)?;
        self.replay_departure_receipt_at(index)
    }

    /// JID-fallback variant of [`Self::replay_departure_receipt`].
    pub(super) fn replay_departure_receipt_for_jid(
        &mut self,
        jid: &FullJid,
        cause: super::durable::OccupancyLeaveCause,
    ) -> Option<RetainedDeparture> {
        let index = self
            .departure_receipts
            .iter()
            .position(|receipt| &receipt.jid == jid && receipt.cause == cause)?;
        self.replay_departure_receipt_at(index)
    }

    fn replay_departure_receipt_at(&mut self, index: usize) -> Option<RetainedDeparture> {
        let receipt = self.departure_receipts.get(index)?;
        // A newer generation of this full JID alone does not stale the
        // receipt: a rejoin under a DIFFERENT nick never announced the old
        // nick's departure, so remaining occupants still owe its unavailable
        // fan-out (#1647, codex round 25). The receipt only becomes
        // unreplayable when its NICK's state moved on too — a same-nick
        // rejoin (kicked since or not) bumped the nickname generation, and
        // whatever the occupants last saw of that nick already supersedes
        // the retained announcement.
        let newer_generation = self
            .latest_generations
            .get(&receipt.jid)
            .is_some_and(|latest| *latest > receipt.generation);
        let nick_state_moved = self
            .room
            .current_nickname_generation(receipt.nick().as_str())
            != receipt.nick_generation;
        let stale = newer_generation && nick_state_moved;
        if stale {
            let receipt = self.departure_receipts.remove(index)?;
            self.prune_departure_state(&receipt.jid);
            return Some(RetainedDeparture::Stale);
        }
        Some(RetainedDeparture::Current(receipt.clone()))
    }

    /// Remove a receipt that can never replay again (its nick was re-taken,
    /// or its JID holds a live session): without consumption it would veto
    /// dormancy and empty-room sealing (`EffectsOwed`) forever, leaking the
    /// actor and its eviction retry (#1647).
    pub(super) fn discard_departure_receipt(
        &mut self,
        attempt: occupancy_handlers::LeaveAttemptId,
    ) {
        if let Some(index) = self
            .departure_receipts
            .iter()
            .position(|receipt| receipt.attempt == attempt)
        {
            if let Some(receipt) = self.departure_receipts.remove(index) {
                self.prune_departure_state(&receipt.jid);
            }
        }
    }

    /// Consume the receipt of an already-completed attempt, if any.
    pub(super) fn take_departure_receipt(
        &mut self,
        attempt: occupancy_handlers::LeaveAttemptId,
    ) -> Option<RetainedDeparture> {
        let index = self
            .departure_receipts
            .iter()
            .position(|receipt| receipt.attempt == attempt)?;
        self.take_departure_receipt_at(index)
    }

    /// Staleness is decided BEFORE the JID's tombstones are pruned with its
    /// last receipt, or a stale receipt would look current.
    fn take_departure_receipt_at(&mut self, index: usize) -> Option<RetainedDeparture> {
        let receipt = self.departure_receipts.remove(index)?;
        let stale = self
            .latest_generations
            .get(&receipt.jid)
            .is_some_and(|latest| *latest > receipt.generation);
        self.prune_departure_state(&receipt.jid);
        Some(if stale {
            RetainedDeparture::Stale
        } else {
            RetainedDeparture::Current(receipt)
        })
    }

    fn prune_departure_state(&mut self, jid: &FullJid) {
        if !self
            .departure_receipts
            .iter()
            .any(|receipt| &receipt.jid == jid)
        {
            self.latest_generations.remove(jid);
            self.superseded_departure_attempts.remove(jid);
        }
    }

    pub(super) fn departure_ledger(&self) -> DepartureLedger {
        DepartureLedger {
            receipts: self.departure_receipts.iter().cloned().collect(),
            latest_generations: self
                .latest_generations
                .iter()
                .map(|(jid, generation)| (jid.clone(), *generation))
                .collect(),
            superseded_attempts: self
                .superseded_departure_attempts
                .iter()
                .map(|(jid, attempts)| (jid.clone(), attempts.iter().copied().collect()))
                .collect(),
        }
    }

    /// Merge a predecessor's ledger: generations take the max, superseded
    /// attempts union, receipts go through the same supersession gate.
    pub(super) fn absorb_departure_ledger(&mut self, ledger: DepartureLedger) {
        for (jid, generation) in ledger.latest_generations {
            let latest = self.latest_generations.entry(jid).or_insert(generation);
            *latest = (*latest).max(generation);
        }
        for (jid, attempts) in ledger.superseded_attempts {
            self.superseded_departure_attempts
                .entry(jid)
                .or_default()
                .extend(attempts);
        }
        let transferred_jids: Vec<FullJid> = ledger
            .receipts
            .iter()
            .map(|receipt| receipt.jid.clone())
            .collect();
        for receipt in ledger.receipts {
            self.retain_departure_receipt(receipt);
        }
        // A transferred receipt refused as superseded leaves no receipt to
        // prune the JID's tombstones with later; drop them now.
        for jid in transferred_jids {
            self.prune_departure_state(&jid);
        }
        let orphaned: Vec<FullJid> = self
            .latest_generations
            .keys()
            .chain(self.superseded_departure_attempts.keys())
            .filter(|jid| {
                !self
                    .departure_receipts
                    .iter()
                    .any(|receipt| &receipt.jid == *jid)
            })
            .cloned()
            .collect();
        for jid in orphaned {
            self.prune_departure_state(&jid);
        }
    }

    pub(super) fn departure_attempt_is_superseded(
        &self,
        jid: &FullJid,
        attempt: occupancy_handlers::LeaveAttemptId,
    ) -> bool {
        self.superseded_departure_attempts
            .get(jid)
            .is_some_and(|attempts| attempts.contains(&attempt))
    }

    pub(super) fn project<T>(
        &mut self,
        gate: ProjectionGate,
        expected: RoomProjectionKind,
        apply: impl FnOnce(&mut Self) -> T,
    ) -> Result<T, ProjectionRefusal> {
        let ProjectionGate::Authorized(authorization) = gate else {
            #[cfg(test)]
            if let Some(hook) = self.test_projection_apply_hook.clone() {
                hook(self.projection_probe("apply"));
            }
            return Ok(apply(self));
        };
        let (commit, kind) = authorization.consume();
        if kind != expected {
            crate::metrics::record_muc_projection_refused("wrong_kind");
            tracing::warn!(room = %self.room.room_jid, "refusing projection with wrong kind");
            return Err(ProjectionRefusal::WrongProjectionKind);
        }
        if self.durable_claim_fence.as_ref() != Some(commit.fence())
            || self.durable_coordinates != Some(commit.coordinates())
        {
            self.seal_state = RoomSealState::OwnershipLost;
            crate::metrics::record_muc_projection_refused("foreign_capability");
            return Err(ProjectionRefusal::ForeignCapability);
        }
        if self
            .projected_revision
            .is_some_and(|revision| revision >= commit.revision())
        {
            crate::metrics::record_muc_projection_refused("revision_already_projected");
            return Err(ProjectionRefusal::RevisionAlreadyProjected);
        }
        self.projected_revision = Some(commit.revision());
        #[cfg(test)]
        if let Some(hook) = self.test_projection_apply_hook.clone() {
            hook(self.projection_probe("apply"));
        }
        Ok(apply(self))
    }

    pub(super) fn map_projection_commit_error(error: DurablePersistError) -> RoomActorError {
        match error {
            DurablePersistError::NotOwner | DurablePersistError::CommitOutcomeUnknown => {
                RoomActorError::RoomSealed
            }
            DurablePersistError::OwnershipUnavailable | DurablePersistError::PersistFailed => {
                RoomActorError::OwnershipUnavailable
            }
        }
    }

    pub(super) fn map_projection_refusal(refusal: ProjectionRefusal) -> RoomActorError {
        match refusal {
            ProjectionRefusal::ForeignCapability => RoomActorError::RoomSealed,
            ProjectionRefusal::RevisionAlreadyProjected
            | ProjectionRefusal::WrongProjectionKind => RoomActorError::OwnershipUnavailable,
        }
    }

    /// Replace config only after restoring cross-field privacy invariants.
    fn replace_config(&mut self, config: RoomConfig) {
        self.room.config = config.normalized();
    }

    fn config_effect_recipients(&self, next: &RoomConfig, plan: ConfigEffectPlan) -> Vec<FullJid> {
        let use_post_enforcement_audience =
            matches!(plan, ConfigEffectPlan::UnmanagedMembersOnlyPostEnforcement)
                && !self.room.config.requires_membership()
                && next.requires_membership();
        self.room
            .occupants
            .values()
            .filter(|occupant| {
                !use_post_enforcement_audience || occupant.affiliation >= Affiliation::Member
            })
            .flat_map(|occupant| self.room.get_occupant_sessions(&occupant.nick))
            .collect()
    }

    fn config_notification_for_update(
        &self,
        next: &RoomConfig,
        plan: ConfigEffectPlan,
    ) -> Option<ConfigChangeNotification> {
        let status_codes = super::config_change_status_codes(&self.room.config, next);
        if status_codes.is_empty() {
            return None;
        }
        Some(ConfigChangeNotification {
            status_codes,
            recipients: self.config_effect_recipients(next, plan),
        })
    }

    fn config_voice_changes_for_update(&self, next: &RoomConfig) -> Vec<OccupantVoiceChange> {
        if self.room.config.moderated == next.moderated {
            return Vec::new();
        }

        let current_moderation = self.room.moderation();
        let next_moderation = crate::types::Moderation::from_moderated_flag(next.moderated);
        self.room
            .occupants
            .values()
            .flat_map(|occupant| {
                let current_voice = occupant.role.voice(current_moderation);
                let next_voice = occupant.role.voice(next_moderation);
                (current_voice != next_voice)
                    .then(|| {
                        self.room
                            .get_occupant_sessions(&occupant.nick)
                            .into_iter()
                            .map(move |session| OccupantVoiceChange {
                                session,
                                voice: next_voice,
                            })
                    })
                    .into_iter()
                    .flatten()
            })
            .collect()
    }

    fn config_effects_for_update(
        &self,
        next: &RoomConfig,
        plan: ConfigEffectPlan,
    ) -> (super::RoomMutationEffects, Option<ConfigChangeNotification>) {
        let notification = self.config_notification_for_update(next, plan);
        let voice_changes = self.config_voice_changes_for_update(next);
        let effects = notification
            .as_ref()
            .map(|notification| {
                super::RoomMutationEffects::config_with_voice_changes(
                    self.room.room_jid.clone(),
                    notification.status_codes.clone(),
                    notification.recipients.clone(),
                    voice_changes,
                )
            })
            .unwrap_or_else(super::RoomMutationEffects::none);
        (effects, notification)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigChangeNotification {
    pub status_codes: Vec<super::MucConfigStatusCode>,
    pub recipients: Vec<FullJid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigMutationApplied {
    pub revision: u64,
    pub notification: Option<ConfigChangeNotification>,
    pub reservation: Option<super::RoomEffectReservation>,
}

impl PartialEq<u64> for ConfigMutationApplied {
    fn eq(&self, other: &u64) -> bool {
        self.revision == *other
    }
}

impl PartialEq<ConfigMutationApplied> for u64 {
    fn eq(&self, other: &ConfigMutationApplied) -> bool {
        *self == other.revision
    }
}

#[derive(Debug, Clone)]
pub struct GroupDmConfigMutationApplied {
    pub snapshot: RoomSnapshot,
    pub notification: Option<ConfigChangeNotification>,
    pub reservation: Option<super::RoomEffectReservation>,
}

impl std::ops::Deref for GroupDmConfigMutationApplied {
    type Target = RoomSnapshot;

    fn deref(&self) -> &Self::Target {
        &self.snapshot
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigEffectPlan {
    DirectAudience,
    ManagedMembersOnlyFallback,
    UnmanagedMembersOnlyPostEnforcement,
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

/// Restore this room actor incarnation's durable configuration,
/// affiliation list, and subject from Postgres before accepting any join
/// (ADR-0017 Phase 3 Slice 7, element 7's locked text: the new owner
/// "restores configuration, affiliations, and subject from Postgres
/// before accepting any join").
///
/// The room registry enqueues this as the first message after
/// spawning/re-claiming a `RoomActor` whenever a [`super::durable::
/// MucDurableStore`] is configured — before [`HydrateDurableRecipients`]
/// and before the actor ref is handed to any caller. Because the mailbox
/// is FIFO and the actor processes messages sequentially, no
/// [`occupancy_handlers::JoinWithAffiliation`] for this incarnation can
/// ever observe a not-yet-restored room — mirroring exactly the ordering
/// guarantee [`HydrateDurableRecipients`]'s own doc comment already
/// relies on for durable membership.
///
/// Load errors fail closed. A successful `None` still means a brand-new room
/// and keeps the caller-supplied initial config. The ownership proof and
/// repeatable-read snapshot are one storage transaction so a claim steal
/// cannot interleave between them.
pub struct RestoreDurableRoomState {
    pub store: std::sync::Arc<dyn super::durable::MucDurableStore>,
    pub claim_fence: super::durable::RoomClaimFenceContext,
}

/// Whether this actor's initial durable restore completed successfully.
///
/// The room registry sends [`GetDurableRestoreReadiness`] behind
/// [`RestoreDurableRoomState`] in the same mailbox. A `Ready` reply is
/// therefore a publication barrier: every caller that can discover the actor
/// observes the restored config, affiliations, and subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableRoomOrigin {
    /// The exact fenced load proved no durable row exists for this room.
    New,
    /// An existing durable room snapshot was installed into this actor.
    Restored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub enum DurableRestoreReadiness {
    Ready(DurableRoomOrigin),
    Pending,
    OwnershipLost,
}

/// FIFO publication barrier for a freshly prepared room actor.
pub struct GetDurableRestoreReadiness;

impl kameo::message::Message<GetDurableRestoreReadiness> for RoomActor {
    type Reply = DurableRestoreReadiness;

    async fn handle(
        &mut self,
        _msg: GetDurableRestoreReadiness,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        match self.restore_state {
            DurableRestoreState::Ready(origin) => DurableRestoreReadiness::Ready(origin),
            DurableRestoreState::Pending => DurableRestoreReadiness::Pending,
            DurableRestoreState::OwnershipLost => DurableRestoreReadiness::OwnershipLost,
        }
    }
}

impl kameo::message::Message<RestoreDurableRoomState> for RoomActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: RestoreDurableRoomState,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.restore_state == DurableRestoreState::OwnershipLost {
            return;
        }
        if let Some(retained) = self.durable_claim_fence.as_ref() {
            if retained != &msg.claim_fence {
                tracing::warn!(
                    room = %self.room.room_jid,
                    retained_entity = %retained.entity,
                    incoming_entity = %msg.claim_fence.entity,
                    "refusing to transplant a room actor onto a different durable claim"
                );
                // The first exact fence permanently identifies this actor
                // incarnation. A delayed restore carrying a successor's
                // tuple must not read, install, or retain successor state.
                self.restore_state = DurableRestoreState::OwnershipLost;
                self.seal_state = RoomSealState::OwnershipLost;
                return;
            }
        }
        // Retain this incarnation's exact authority before awaiting the
        // fenced load. A terminal result must leave a coherent sealed actor,
        // rather than one whose non-serving state lacks its identifying fence.
        self.durable_store = Some(std::sync::Arc::clone(&msg.store));
        self.durable_claim_fence = Some(msg.claim_fence.clone());
        match msg
            .store
            .load_room_state_fenced(&self.room.room_jid, &msg.claim_fence)
            .await
        {
            Ok(Some(state)) => {
                self.install_durable_room_state(state);
                self.restore_state = DurableRestoreState::Ready(DurableRoomOrigin::Restored);
            }
            Ok(None) => {
                // No durable row yet — brand new room, nothing to restore.
                self.restore_state = DurableRestoreState::Ready(DurableRoomOrigin::New);
            }
            Err(crate::XmppError::OwnershipLost { entity }) => {
                self.restore_state = DurableRestoreState::OwnershipLost;
                self.seal_state = RoomSealState::OwnershipLost;
                tracing::warn!(
                    room = %self.room.room_jid,
                    %entity,
                    "durable room-state restore lost this actor's exact ownership"
                );
            }
            Err(error) => {
                // ADR-0017 Phase 3 Slice 7 FIX 4 (council-adjudicated):
                // fail CLOSED, not open. Serving with the caller-supplied
                // defaults here would look exactly like a legitimate,
                // brand-new empty room to every occupant while actually
                // discarding whatever ban/member/owner grants are on
                // record in Postgres. Joins are refused (see
                // `Self::ensure_restored_before_join`) until a later
                // retry succeeds.
                tracing::warn!(
                    room = %self.room.room_jid,
                    %error,
                    "durable room-state restore failed; refusing joins until a retry \
                     succeeds (fail-closed, FIX 4 — previously fail-open)"
                );
                self.restore_state = DurableRestoreState::Pending;
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

fn affiliation_overflows_full_room(affiliation: Affiliation) -> bool {
    matches!(affiliation, Affiliation::Owner | Affiliation::Admin)
}

impl kameo::message::Message<Join> for RoomActor {
    type Reply = Result<(), RoomActorError>;

    async fn handle(&mut self, msg: Join, _ctx: &mut Context<Self, Self::Reply>) -> Self::Reply {
        self.reject_sealed_join().await?;
        // `Join` predates the resolver-aware admission message, but it is
        // still reachable by internal callers holding an ActorRef. Apply the
        // same fail-closed restore and ownership gates as
        // `JoinWithAffiliation`; otherwise a stale reference could admit an
        // occupant after this actor's durable fence became non-serving.
        self.ensure_restored_before_join().await?;
        self.gate_join_ownership().await?;
        if self.invite_rollback_pending(&msg.real_jid.to_bare()) {
            return Err(RoomActorError::InviteRollbackPending);
        }
        if self.room.is_full() && !affiliation_overflows_full_room(msg.affiliation) {
            return Err(RoomActorError::RoomFull);
        }
        if self.room.get_occupant(&msg.nick).is_some() {
            return Err(RoomActorError::NickAlreadyInUse(msg.nick));
        }
        let Some(durable_nick) = super::durable::MucOccupantNick::new(msg.nick.clone()) else {
            return Err(RoomActorError::NickAlreadyInUse(msg.nick));
        };
        let gate = self
            .commit_projection(RoomProjection::OccupancyJoin {
                occupant: msg.real_jid.clone(),
                nick: durable_nick,
            })
            .await
            .map_err(Self::map_projection_commit_error)?;
        self.project(gate, RoomProjectionKind::OccupancyJoin, |actor| {
            actor.room.add_occupant(super::Occupant {
                real_jid: msg.real_jid.clone(),
                nick: msg.nick.clone(),
                role: msg.role,
                affiliation: msg.affiliation,
                is_remote: false,
                home_server: None,
            });
            let joined_at =
                OccupancyWatermark::from_revision(actor.occupancy_revision.saturating_add(1));
            actor
                .room
                .set_session_watermark(msg.real_jid.clone(), joined_at);
            actor
                .room
                .set_session_generation(&msg.real_jid, OccupancySessionGeneration::mint());
            actor.note_session_joined(&msg.real_jid);
            actor.occupancy_revision = actor.occupancy_revision.saturating_add(1);
        })
        .map_err(Self::map_projection_refusal)
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

/// Resolve the SFU media-grant inputs for `jid` in one round-trip:
/// whether they are a current occupant, and if so their XEP-0045
/// voice (which needs both the role and the room's moderation).
///
/// The Muji gate uses this instead of [`GetOccupantByJid`] +
/// [`GetConfig`] so authorization reads a single consistent snapshot
/// of the room — two asks could straddle a role change or a config
/// change and mint grants matching neither.
pub struct GetOccupantVoice {
    pub jid: FullJid,
}

impl kameo::message::Message<GetOccupantVoice> for RoomActor {
    type Reply = Option<Voice>;

    async fn handle(
        &mut self,
        msg: GetOccupantVoice,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let moderation = self.room.moderation();
        self.room
            .find_occupant_by_real_jid(&msg.jid)
            .map(|occupant| occupant.role.voice(moderation))
    }
}

/// Current XEP-0045 voice of every active occupant session.
///
/// Used after a room-configuration change that alters `moderated`:
/// flipping moderation silently re-decides voice for every seated
/// visitor without touching any occupant's role, so callers owning an
/// SFU handle must converge live media grants or a visitor who just
/// lost text voice keeps publishing.
pub struct OccupantVoices;

impl kameo::message::Message<OccupantVoices> for RoomActor {
    type Reply = Vec<(FullJid, Voice)>;

    async fn handle(
        &mut self,
        _msg: OccupantVoices,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let moderation = self.room.moderation();
        self.room
            .occupants
            .values()
            .flat_map(|occupant| {
                let voice = occupant.role.voice(moderation);
                self.room
                    .get_occupant_sessions(&occupant.nick)
                    .into_iter()
                    .map(move |session| (session, voice))
            })
            .collect()
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

/// Basic room information — the [`GetInfo`] reply.
#[derive(Debug, Clone)]
pub struct RoomInfo {
    /// Room JID
    pub room_jid: BareJid,
    /// Number of occupants
    pub occupant_count: usize,
    /// Room name
    pub name: String,
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
    pub effect_plan: ConfigEffectPlan,
}

impl kameo::message::Message<UpdateConfig> for RoomActor {
    type Reply = Result<ConfigMutationApplied, RoomMutationError>;

    async fn handle(
        &mut self,
        msg: UpdateConfig,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let config = msg.config.normalized();
        let (effects, notification) = self.config_effects_for_update(&config, msg.effect_plan);
        let (_, reservation) = self
            .commit_durable(
                RoomDurableMutation::Config {
                    config: config.clone(),
                    waddle_id: WaddleId::new(self.room.waddle_id.clone()),
                    channel_id: ChannelId::new(self.room.channel_id.clone()),
                },
                effects,
            )
            .await?;
        self.config_durable_coordinates = self.durable_coordinates;
        self.replace_config(config);
        self.config_revision = self.config_revision.saturating_add(1);
        self.advance_room_admission_revision();
        Ok(ConfigMutationApplied {
            revision: self.config_revision,
            notification,
            reservation,
        })
    }
}

/// Replace the room config only if the config revision still matches the
/// caller's attempted update. Used for best-effort rollback without clobbering
/// a later successful rename, including identical-name updates.
pub struct RollbackConfigIfRevision {
    pub expected_revision: u64,
    pub config: RoomConfig,
    pub reservation: Option<super::RoomEffectReservation>,
}

impl kameo::message::Message<RollbackConfigIfRevision> for RoomActor {
    type Reply = Result<bool, RoomMutationError>;

    async fn handle(
        &mut self,
        msg: RollbackConfigIfRevision,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.config_revision != msg.expected_revision {
            return Ok(false);
        }
        let config = msg.config.normalized();
        self.commit_durable(
            RoomDurableMutation::Config {
                config: config.clone(),
                waddle_id: WaddleId::new(self.room.waddle_id.clone()),
                channel_id: ChannelId::new(self.room.channel_id.clone()),
            },
            msg.reservation
                .map(super::RoomMutationEffects::none_superseding)
                .unwrap_or_else(super::RoomMutationEffects::none),
        )
        .await?;
        self.config_durable_coordinates = self.durable_coordinates;
        self.replace_config(config);
        self.config_revision = self.config_revision.saturating_add(1);
        self.advance_room_admission_revision();
        Ok(true)
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
    /// ADR-0017 Phase 3 Slice 7 FIX 2: see [`AdminApplyError::NotOwner`]'s
    /// doc comment — identical contract, one message type over.
    #[error("this room is no longer serviceable by this actor")]
    NotOwner,
    #[error("this room's ownership is temporarily unavailable")]
    OwnershipUnavailable,
    #[error("durable room mutation commit failed before the in-memory mutation")]
    PersistFailed,
    #[error("durable room mutation commit outcome could not be reconciled")]
    CommitOutcomeUnknown,
}

impl From<RoomMutationError> for UpdateGroupDmConfigByMemberError {
    fn from(error: RoomMutationError) -> Self {
        match error {
            RoomMutationError::NotOwner => UpdateGroupDmConfigByMemberError::NotOwner,
            RoomMutationError::OwnershipUnavailable => {
                UpdateGroupDmConfigByMemberError::OwnershipUnavailable
            }
            RoomMutationError::PersistFailed => UpdateGroupDmConfigByMemberError::PersistFailed,
            RoomMutationError::CommitOutcomeUnknown => {
                UpdateGroupDmConfigByMemberError::CommitOutcomeUnknown
            }
        }
    }
}

impl From<DurablePersistError> for UpdateGroupDmConfigByMemberError {
    fn from(error: DurablePersistError) -> Self {
        match error {
            DurablePersistError::NotOwner => UpdateGroupDmConfigByMemberError::NotOwner,
            DurablePersistError::OwnershipUnavailable => {
                UpdateGroupDmConfigByMemberError::OwnershipUnavailable
            }
            DurablePersistError::PersistFailed => UpdateGroupDmConfigByMemberError::PersistFailed,
            DurablePersistError::CommitOutcomeUnknown => {
                UpdateGroupDmConfigByMemberError::CommitOutcomeUnknown
            }
        }
    }
}

impl kameo::message::Message<UpdateGroupDmConfigByMember> for RoomActor {
    type Reply = Result<GroupDmConfigMutationApplied, UpdateGroupDmConfigByMemberError>;

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
        let mut config = msg.config;
        config.group_dm = true;
        let config = config.normalized();
        let (effects, notification) =
            self.config_effects_for_update(&config, ConfigEffectPlan::DirectAudience);
        let (_, reservation) = self
            .commit_durable(
                RoomDurableMutation::Config {
                    config: config.clone(),
                    waddle_id: WaddleId::new(self.room.waddle_id.clone()),
                    channel_id: ChannelId::new(self.room.channel_id.clone()),
                },
                effects,
            )
            .await?;
        self.config_durable_coordinates = self.durable_coordinates;
        self.replace_config(config);
        self.config_revision = self.config_revision.saturating_add(1);
        self.advance_room_admission_revision();
        Ok(GroupDmConfigMutationApplied {
            snapshot: RoomSnapshot {
                room: self.room.clone(),
                claim_fence: self.durable_claim_fence.clone(),
                durable_coordinates: self.durable_coordinates,
                config_durable_coordinates: self.config_durable_coordinates,
                config_revision: self.config_revision,
                admission_revision: self.admission_revision,
                occupancy_revision: self.occupancy_revision,
                departures: self.departure_ledger(),
            },
            notification,
            reservation,
        })
    }
}

/// Apply a XEP-0045 §8.1 subject change to the room. The interpreter
/// emits this in response to an `OutboundEvent::PersistRoomSubject`
/// produced by the room handler chain's subject handler after
/// authorization passes. The actor constructs a `SubjectState`, persists it
/// before applying, then installs it in `MucRoom.subject` for replay on the
/// next join (XEP-0045 §7.2.15).
pub struct SetSubject {
    pub texts: RoomSubjectTexts,
    pub setter: BareJid,
    pub setter_nick: String,
    pub set_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum SetSubjectError {
    #[error("this room is no longer serviceable by this actor")]
    NotOwner,
    #[error("this room's ownership is temporarily unavailable")]
    OwnershipUnavailable,
    #[error("durable subject persist failed before the in-memory mutation")]
    PersistFailedBeforeApply,
    #[error("durable subject commit outcome could not be reconciled")]
    CommitOutcomeUnknown,
}

impl From<PreMutationOwnershipError> for SetSubjectError {
    fn from(error: PreMutationOwnershipError) -> Self {
        match error {
            PreMutationOwnershipError::NotOwner => Self::NotOwner,
            PreMutationOwnershipError::OwnershipUnavailable => Self::OwnershipUnavailable,
        }
    }
}

impl From<DurablePersistError> for SetSubjectError {
    fn from(error: DurablePersistError) -> Self {
        match error {
            DurablePersistError::NotOwner => Self::NotOwner,
            DurablePersistError::OwnershipUnavailable => Self::OwnershipUnavailable,
            DurablePersistError::PersistFailed => Self::PersistFailedBeforeApply,
            DurablePersistError::CommitOutcomeUnknown => Self::CommitOutcomeUnknown,
        }
    }
}

impl kameo::message::Message<SetSubject> for RoomActor {
    type Reply = Result<(), SetSubjectError>;

    async fn handle(
        &mut self,
        msg: SetSubject,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.gate_pre_mutation_ownership().await?;
        let subject = SubjectState {
            texts: msg.texts,
            setter: msg.setter,
            setter_nick: msg.setter_nick,
            set_at: msg.set_at,
        };
        self.commit_durable(
            RoomDurableMutation::Subject(Some(subject.clone())),
            super::RoomMutationEffects::none(),
        )
        .await?;
        self.room.subject = Some(subject);
        Ok(())
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
    type Reply = Result<(), RoomActorError>;

    async fn handle(
        &mut self,
        msg: ApplyPin,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        // Pins are in-memory-only state, but their system-message broadcast
        // is an externally visible effect. Refuse both while sealed so the
        // interpreter cannot announce a pin the actor did not retain.
        self.reject_sealed_effects().await?;
        let projection = match &msg.change {
            PinStateChange::Pin(entry) => {
                RoomProjection::Pin(super::durable::RoomPinProjection::Pin {
                    target: entry.target_stanza_id.clone(),
                })
            }
            PinStateChange::Unpin { target_stanza_id } => {
                RoomProjection::Pin(super::durable::RoomPinProjection::Unpin {
                    target: target_stanza_id.clone(),
                })
            }
        };
        let expected = projection.kind();
        let gate = self
            .commit_projection(projection)
            .await
            .map_err(Self::map_projection_commit_error)?;
        self.project(gate, expected, |actor| match msg.change {
            PinStateChange::Pin(entry) => {
                actor.room.upsert_pin(entry);
            }
            PinStateChange::Unpin { target_stanza_id } => {
                actor.room.remove_pin_by_target(&target_stanza_id);
            }
        })
        .map_err(Self::map_projection_refusal)
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
    type Reply = Result<(), AffiliationMutationError>;

    async fn handle(
        &mut self,
        msg: ChangeAffiliation,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.invite_rollback_pending(&msg.jid) {
            return Err(AffiliationMutationError::InviteRollbackPending);
        }
        let changed = self.room.get_affiliation(&msg.jid) != msg.affiliation;
        if changed {
            self.commit_durable(
                RoomDurableMutation::Affiliation(super::durable::AffiliationEntry::new(
                    msg.jid.clone(),
                    (msg.affiliation != Affiliation::None).then_some(msg.affiliation),
                )),
                super::RoomMutationEffects::none(),
            )
            .await?;
        } else {
            self.gate_pre_mutation_ownership()
                .await
                .map_err(RoomMutationError::from)?;
        }
        self.invalidate_invite_grant(&msg.jid);
        let needs_rehydration = self.prune_durable_recipient_if_removed(&msg.jid, msg.affiliation);
        if self
            .room
            .set_affiliation(msg.jid.clone(), msg.affiliation)
            .is_some()
        {
            self.advance_member_admission_revision(&msg.jid);
        }
        if needs_rehydration {
            self.refresh_durable_recipients_from_source().await;
        }
        Ok(())
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
            // Completed and no-grant operation records are replay metadata,
            // not live room state. An unresolved temporary grant still owns
            // compensation responsibility and must survive both lifecycle
            // guards (the Dormant guard is also protected by the explicit
            // Member affiliation itself).
            // Unconsumed departure receipts veto dormancy: they are owed
            // effects, and reaping the actor would strand their replays.
            // A sealed actor stays dormant unconditionally: sealing now
            // requires an empty ledger, and a sealed actor refuses the
            // admissions that could mint new receipts.
            dormant: self.seal_state.is_sealed()
                || (self.room.is_dormant()
                    && !self.has_lifecycle_fenced_invite_operation()
                    && self.departure_receipts.is_empty()),
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
        self.seal_state.is_sealed()
    }
}

/// Return the typed admission seal state used by the registry's reaper.
///
/// Unlike [`IsSealed`], this preserves whether the actor was sealed by an
/// inactivity transition or by a definitive ownership-loss fence.
pub struct GetRoomSealState;

impl kameo::message::Message<GetRoomSealState> for RoomActor {
    type Reply = RoomSealState;

    async fn handle(
        &mut self,
        _msg: GetRoomSealState,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.seal_state
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

/// Pre-seal an actor before the registry commits a terminal `Destroy`.
/// Repeating the same attempt is deliberately idempotent so the registry can
/// reconcile a lost ask reply without reopening the actor.
pub struct SealForDestroy {
    pub attempt: crate::muc::durable::DestroyAttemptId,
}

impl kameo::message::Message<SealForDestroy> for RoomActor {
    type Reply = RoomSealState;

    async fn handle(
        &mut self,
        msg: SealForDestroy,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        match self.seal_state {
            RoomSealState::Open => {
                self.seal_state = RoomSealState::Destroying {
                    attempt: msg.attempt,
                };
                self.seal_state
            }
            // An inactivity seal already excludes every admission. Upgrade it
            // in the same mailbox turn so an explicit terminal destroy can
            // reconcile the exact attempt instead of retaining it forever.
            RoomSealState::Inactive => {
                self.seal_state = RoomSealState::Destroying {
                    attempt: msg.attempt,
                };
                self.seal_state
            }
            RoomSealState::Destroying { attempt } if attempt == msg.attempt => self.seal_state,
            state => state,
        }
    }
}

/// Undo a failed destroy only when this is still the matching attempt. A
/// delayed recovery must never reopen a later destroy attempt.
pub struct UnsealDestroy {
    pub attempt: crate::muc::durable::DestroyAttemptId,
}

/// Reopen an inactivity seal when the registry could not durably record the
/// dormancy transition. This is intentionally narrower than destroy unseal.
pub struct UnsealInactive;

impl kameo::message::Message<UnsealInactive> for RoomActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        _msg: UnsealInactive,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.seal_state == RoomSealState::Inactive {
            self.seal_state = RoomSealState::Open;
            true
        } else {
            false
        }
    }
}

impl kameo::message::Message<UnsealDestroy> for RoomActor {
    type Reply = bool;

    async fn handle(
        &mut self,
        msg: UnsealDestroy,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.seal_state
            == (RoomSealState::Destroying {
                attempt: msg.attempt,
            })
        {
            self.seal_state = RoomSealState::Open;
            true
        } else {
            false
        }
    }
}

/// Typed result of an inactivity-seal attempt.
///
/// The registry must retain the distinction between ordinary inactivity and
/// a prior ownership-loss seal: both refuse admission, but only the latter
/// requires immediate non-serving actor teardown. Registry cleanup then uses
/// the retained fence to distinguish an already-missing tuple from a locally
/// superseded tuple that still needs one conditional exact release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, kameo::Reply)]
pub enum SealIfInactiveOutcome {
    Refused,
    Inactive,
    OwnershipLost,
    /// The room is inactive but still owes departure effects: unconsumed
    /// departure receipts are waiting to be acknowledged. Destroying now
    /// would strand the owed replays (the retained departure would find the
    /// room absent and drop its 110 self-echo). Not definitive - the caller
    /// retains its eviction responsibility and retries after the acks drain.
    EffectsOwed,
}

impl kameo::message::Message<SealIfInactive> for RoomActor {
    type Reply = SealIfInactiveOutcome;

    async fn handle(
        &mut self,
        msg: SealIfInactive,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        match self.seal_state {
            RoomSealState::OwnershipLost => return SealIfInactiveOutcome::OwnershipLost,
            RoomSealState::Inactive => return SealIfInactiveOutcome::Inactive,
            RoomSealState::Destroying { .. } => return SealIfInactiveOutcome::Refused,
            RoomSealState::Open => {}
        }
        if self.occupancy_revision != msg.expected_occupancy_revision {
            return SealIfInactiveOutcome::Refused;
        }
        let inactive = !self.has_lifecycle_fenced_invite_operation()
            && match msg.guard {
                SealGuard::Dormant => self.room.is_dormant(),
                SealGuard::EmptyNonPersistent => {
                    self.room.occupants.is_empty() && !self.room.config.persistent
                }
            };
        if inactive && !self.departure_receipts.is_empty() {
            return SealIfInactiveOutcome::EffectsOwed;
        }
        if inactive {
            self.seal_state = RoomSealState::Inactive;
            SealIfInactiveOutcome::Inactive
        } else {
            SealIfInactiveOutcome::Refused
        }
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
            claim_fence: self.durable_claim_fence.clone(),
            durable_coordinates: self.durable_coordinates,
            config_durable_coordinates: self.config_durable_coordinates,
            config_revision: self.config_revision,
            admission_revision: self.admission_revision,
            occupancy_revision: self.occupancy_revision,
            departures: self.departure_ledger(),
        })
    }
}

/// Internal liveness probe for the owner-veto path (ADR-0017 Phase 3 Slice
/// 3, element 4's "Unwedge" text) — the `RoomActor` counterpart of
/// `registry::user_actor::HealthCheck`: a successful reply proves this
/// actor's mailbox loop is live and responsive right now, since kameo
/// processes a mailbox strictly in order. Production callers:
/// `RoomLocalClaims::health_check` (the Slice 7 owner-veto/reconciliation
/// path) and `RoomLocalClaims::seal_before_release` (the Slice 10 drain
/// barrier — a reply after the drain's `owned()` snapshot proves every
/// mutation queued ahead of it already committed its fenced durable write).
pub struct HealthCheck;

impl kameo::message::Message<HealthCheck> for RoomActor {
    type Reply = ();

    async fn handle(
        &mut self,
        _msg: HealthCheck,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
    }
}
