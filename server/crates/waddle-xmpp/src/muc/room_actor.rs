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
use super::pin::{PinStateChange, PinnedEntry};
use super::{MucRoom, RoomConfig, RoomSubjectTexts, SubjectState};
use crate::types::{Affiliation, Role};

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
    AdminItemsApplied, ApplyAdminItems, ApplyAffiliationChange, EnforceMembersOnly,
    EnforceMembersOnlyAffiliations, GetAdminContext, IsOwner,
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
    ClearMujiPresence, InCallPresenceUpdateOutcome, JoinAffiliationGrant, JoinWithAffiliation,
    LeaveByRealJid, MujiPresenceUpdateOutcome, PingSelfCheck, PresenceUpdateData,
    ReconcileChannelBackedRoom, ResolverAffiliationSyncOutcome, SyncResolverAffiliation,
    UpsertInCallState, UpsertMujiPresence,
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
    /// The pre-mutation gate passed, but this actor became non-serving before the
    /// following durable write committed. The mutation exists only in this
    /// now-stale actor incarnation; callers must demote it and retry against
    /// the current owner.
    #[error("this actor became non-serving after the in-memory mutation was applied")]
    OwnershipLostAfterApply,
    /// The exact ownership check could not establish either ownership or
    /// loss. The requested mutation was never applied and may be retried.
    #[error("this room's ownership is temporarily unavailable")]
    OwnershipUnavailable,
    /// FIX 2: the fenced gate passed (or was skipped, single-node), the
    /// in-memory mutation was applied, but the awaited durable persist
    /// afterwards failed for a reason OTHER than ownership loss
    /// (a transient backend outage). Previously silently logged and
    /// swallowed; FIX 2 revises that contract for affiliation-mutating
    /// operations — the caller is told the durable side did not
    /// converge rather than reporting bare success.
    #[error("durable persist failed after the in-memory mutation committed")]
    PersistFailed,
}

impl From<RoomMutationError> for AdminApplyError {
    fn from(error: RoomMutationError) -> Self {
        match error {
            RoomMutationError::NotOwner => AdminApplyError::NotOwner,
            RoomMutationError::OwnershipLostAfterApply => AdminApplyError::OwnershipLostAfterApply,
            RoomMutationError::OwnershipUnavailable => AdminApplyError::OwnershipUnavailable,
            RoomMutationError::PersistFailed => AdminApplyError::PersistFailed,
        }
    }
}

impl From<DurablePersistError> for AdminApplyError {
    fn from(error: DurablePersistError) -> Self {
        match error {
            DurablePersistError::NotOwner => AdminApplyError::NotOwner,
            DurablePersistError::OwnershipLostAfterApply => {
                AdminApplyError::OwnershipLostAfterApply
            }
            DurablePersistError::OwnershipUnavailable => AdminApplyError::OwnershipUnavailable,
            DurablePersistError::PersistFailed => AdminApplyError::PersistFailed,
        }
    }
}

/// ADR-0017 Phase 3 Slice 7 FIX 2 (council-adjudicated): typed outcome of
/// the two-stage durability gate every durable-relevant `RoomActor`
/// mutation handler (`UpdateConfig`, `RollbackConfigIfRevision`,
/// `UpdateGroupDmConfigByMember`, `SetSubject`, `ChangeAffiliation`,
/// `ApplyAdminItems`, `ApplyAffiliationChange`,
/// `EnforceMembersOnlyAffiliations`, `ReconcileChannelBackedRoom`) now
/// runs:
///
/// 1. **Before mutating**: [`RoomActor::gate_mutation`] runs the SAME
///    fenced `check_exact_claim_fence` pre-check using this actor
///    incarnation's retained fence.
///    `NotOwner` here means the in-memory mutation NEVER RAN — the
///    caller must not report success, must trigger
///    `RoomLocalClaims::demote`, and must surface a conformant,
///    recoverable error to whatever requested the mutation.
/// 2. **After mutating**: an awaited `persist_*` write can still
///    fail for a reason OTHER than ownership loss (a transient DB
///    outage). This was previously silently logged and swallowed
///    (`muc/durable.rs`'s old fail-open doc contract, corrected by this
///    fix). `PersistFailed` surfaces that failure typed instead — the
///    in-memory mutation has already committed by this point (undoing
///    it risks a worse inconsistency than leaving it applied-but-
///    not-yet-durable), but the caller now knows the durable side did
///    not converge and can retry or surface a typed error rather than
///    reporting bare success.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RoomMutationError {
    #[error("this room is no longer serviceable by this actor")]
    NotOwner,
    #[error("this actor became non-serving after the in-memory mutation was applied")]
    OwnershipLostAfterApply,
    #[error("this room's ownership is temporarily unavailable")]
    OwnershipUnavailable,
    #[error("durable persist failed after the in-memory mutation committed")]
    PersistFailed,
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
    #[error("this actor became non-serving after the in-memory mutation was applied")]
    OwnershipLostAfterApply,
    #[error("this room's ownership is temporarily unavailable")]
    OwnershipUnavailable,
    #[error("durable persist failed after the in-memory mutation committed")]
    PersistFailed,
    #[error("invitee affiliation is fenced pending invite rollback acknowledgement")]
    InviteRollbackPending,
}

impl From<RoomMutationError> for AffiliationMutationError {
    fn from(error: RoomMutationError) -> Self {
        match error {
            RoomMutationError::NotOwner => Self::NotOwner,
            RoomMutationError::OwnershipLostAfterApply => Self::OwnershipLostAfterApply,
            RoomMutationError::OwnershipUnavailable => Self::OwnershipUnavailable,
            RoomMutationError::PersistFailed => Self::PersistFailed,
        }
    }
}

impl From<DurablePersistError> for AffiliationMutationError {
    fn from(error: DurablePersistError) -> Self {
        match error {
            DurablePersistError::NotOwner => Self::NotOwner,
            DurablePersistError::OwnershipLostAfterApply => Self::OwnershipLostAfterApply,
            DurablePersistError::OwnershipUnavailable => Self::OwnershipUnavailable,
            DurablePersistError::PersistFailed => Self::PersistFailed,
        }
    }
}

/// FIX 2: typed outcome of an awaited durable `save_*` call inside
/// [`RoomActor::persist_config`] and [`RoomActor::persist_affiliation`]. The
/// ownership variants distinguish a failure before an in-memory mutation from
/// ownership loss after one. Subject persistence has its own pre-apply-only
/// [`SetSubjectError`] contract. Definitive ownership variants preserve
/// whether the mutation was applied; `PersistFailed` is phase-neutral so
/// each operation's public error can state its own apply boundary. Backend
/// diagnostics stay in structured logs rather than becoming a stringly-typed
/// protocol payload.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum DurablePersistError {
    #[error("this actor became non-serving before the in-memory mutation was applied")]
    NotOwner,
    #[error("this actor became non-serving after the in-memory mutation was applied")]
    OwnershipLostAfterApply,
    #[error("this room's exact ownership fence is unavailable")]
    OwnershipUnavailable,
    #[error("durable persist failed")]
    PersistFailed,
}

impl From<DurablePersistError> for RoomMutationError {
    fn from(error: DurablePersistError) -> Self {
        match error {
            DurablePersistError::NotOwner => RoomMutationError::NotOwner,
            DurablePersistError::OwnershipLostAfterApply => {
                RoomMutationError::OwnershipLostAfterApply
            }
            DurablePersistError::OwnershipUnavailable => RoomMutationError::OwnershipUnavailable,
            DurablePersistError::PersistFailed => RoomMutationError::PersistFailed,
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
    /// Why this actor refuses further admissions. Keeping the reason typed
    /// lets the registry distinguish an ordinary inactivity seal, whose
    /// removal must retain exact-release backlog fencing, from a definitive
    /// ownership-loss seal, whose non-serving local actor must be evicted even
    /// when that backlog is full.
    seal_state: RoomSealState,
    /// Whether the graceful-shutdown mailbox barrier has run for this actor.
    ///
    /// Kept separately from `seal_state` so an actor can retain definitive
    /// `OwnershipLost` provenance while still rejecting the cache-only
    /// recovery replays that ownership loss ordinarily permits.
    shutdown_seal_installed: bool,
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
    /// Graceful shutdown placed a terminal mailbox seal after every message
    /// already queued ahead of it. No later serving action may start while the
    /// actor remains alive awaiting retirement and claim release: admissions,
    /// occupancy/presence/call updates, room/admin/invite mutations,
    /// restore/hydration work, destruction, and traffic-producing dispatch
    /// snapshots are all refused.
    Shutdown,
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
    /// This room actor was sealed by guarded destruction, ownership loss, or
    /// graceful shutdown and is no longer allowed to perform serving work.
    /// Retryable during ordinary inactivity eviction; during shutdown the
    /// caller must stop dispatching new room work.
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
            config_revision: 0,
            admission_revision: 0,
            room_admission_revision: 0,
            member_admission_revisions: HashMap::new(),
            invite_operations: HashMap::new(),
            invite_operation_by_invitee: HashMap::new(),
            occupancy_revision: 0,
            seal_state: RoomSealState::Open,
            shutdown_seal_installed: false,
            occupant_id_secret,
            durable_member_recipients: Vec::new(),
            membership_source: None,
            durable_store: None,
            durable_claim_fence: None,
            restore_state: DurableRestoreState::Ready(DurableRoomOrigin::New),
        }
    }

    /// Install one authoritative durable snapshot and advance the room-wide
    /// admission watermark when the restored policy differs from the
    /// constructor/default state. Subject changes are deliberately excluded:
    /// they do not affect whether or how an occupant may enter.
    fn install_durable_room_state(&mut self, state: super::durable::DurableRoomState) {
        let previous_admission_state = self.admission_policy_snapshot();
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

    /// Reject any serving operation once an actor seal is visible.
    ///
    /// Read-only diagnostics intentionally do not call this helper, so the
    /// drain can still inspect the actor until retirement. Every operation
    /// that mutates room state or prepares externally emitted room traffic
    /// must call it before reading data that could drive that work.
    fn gate_serving_activity(&self) -> Result<(), RoomActorError> {
        if self.seal_state.is_sealed() {
            Err(RoomActorError::RoomSealed)
        } else {
            Ok(())
        }
    }

    /// Whether even exact, read-only recovery replays are forbidden.
    ///
    /// Bare ownership loss permits callers to recover already-recorded
    /// idempotency outcomes, while every state transition remains fenced by
    /// the mutation gate. Inactivity and shutdown are actor-lifecycle
    /// terminals. The separate shutdown bit also covers an OwnershipLost
    /// actor after the drain's ordered mailbox barrier without erasing that
    /// stronger ownership provenance.
    fn blocks_recovery_replay(&self) -> bool {
        self.shutdown_seal_installed
            || matches!(
                self.seal_state,
                RoomSealState::Inactive | RoomSealState::Shutdown
            )
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
        if self.seal_state.is_sealed() {
            return Err(PreMutationOwnershipError::NotOwner);
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

    /// Refuse admission through an already sealed actor while allowing an
    /// inactivity seal to strengthen into a definitive ownership-loss seal.
    ///
    /// The ownership probe's result never makes an inactive actor joinable:
    /// it only preserves the stronger cause for the registry's reaper. A
    /// transient probe failure therefore still returns `RoomSealed` and
    /// leaves the original inactivity seal intact.
    async fn reject_sealed_join(&mut self) -> Result<(), RoomActorError> {
        match self.seal_state {
            RoomSealState::Open => Ok(()),
            RoomSealState::Shutdown | RoomSealState::OwnershipLost => {
                Err(RoomActorError::RoomSealed)
            }
            RoomSealState::Inactive => {
                let _ = self.gate_join_ownership().await;
                Err(RoomActorError::RoomSealed)
            }
        }
    }

    /// Await the durable persist of the current config (ADR-0017 Phase 3
    /// Slice 7, FIX 2 revision): a persistence error is logged AND
    /// returned typed — the in-memory mutation the caller already applied
    /// remains authoritative for this actor incarnation regardless, but
    /// the caller now learns the durable side did not converge (see
    /// [`DurablePersistError`]'s doc comment for why this is no longer
    /// silently swallowed).
    fn classify_durable_persist_error(
        &mut self,
        write_error: crate::XmppError,
        operation: &'static str,
        mutation_applied: bool,
    ) -> DurablePersistError {
        match write_error {
            crate::XmppError::OwnershipLost { entity } => {
                self.seal_state = RoomSealState::OwnershipLost;
                tracing::warn!(
                    room = %self.room.room_jid,
                    %operation,
                    %entity,
                    "durable write failed because this actor lost exact ownership"
                );
                if mutation_applied {
                    DurablePersistError::OwnershipLostAfterApply
                } else {
                    DurablePersistError::NotOwner
                }
            }
            crate::XmppError::OwnershipUnavailable { entity } => {
                tracing::warn!(
                    room = %self.room.room_jid,
                    %operation,
                    %entity,
                    "durable write could not prove exact ownership"
                );
                if mutation_applied {
                    DurablePersistError::PersistFailed
                } else {
                    DurablePersistError::OwnershipUnavailable
                }
            }
            error => {
                tracing::warn!(
                    room = %self.room.room_jid,
                    %operation,
                    %error,
                    "durable write failed for a non-ownership reason"
                );
                DurablePersistError::PersistFailed
            }
        }
    }

    async fn persist_config(&mut self) -> Result<(), DurablePersistError> {
        let Some(store) = self.durable_store.clone() else {
            return Ok(());
        };
        let fence = self
            .durable_claim_fence
            .clone()
            .ok_or(DurablePersistError::OwnershipUnavailable)?;
        let result = store
            .save_config_fenced(
                &self.room.room_jid,
                &self.room.waddle_id,
                &self.room.channel_id,
                &self.room.config,
                &fence,
            )
            .await;
        match result {
            Ok(()) => Ok(()),
            Err(error) => Err(self.classify_durable_persist_error(error, "config", true)),
        }
    }

    /// Replace config only after restoring cross-field privacy invariants.
    fn replace_config(&mut self, config: RoomConfig) {
        self.room.config = config.normalized();
    }

    /// Persist a constructed subject before installing it in memory. See
    /// [`Self::persist_config`]'s FIX 2 rationale.
    async fn persist_subject_before_apply(
        &mut self,
        subject: Option<&SubjectState>,
    ) -> Result<(), SetSubjectError> {
        let Some(store) = self.durable_store.clone() else {
            return Ok(());
        };
        let fence = self
            .durable_claim_fence
            .clone()
            .ok_or(SetSubjectError::OwnershipUnavailable)?;
        let result = store
            .save_subject_fenced(&self.room.room_jid, subject, &fence)
            .await;
        match result {
            Ok(()) => Ok(()),
            Err(crate::XmppError::OwnershipLost { entity }) => {
                self.seal_state = RoomSealState::OwnershipLost;
                tracing::warn!(
                    room = %self.room.room_jid,
                    %entity,
                    "durable subject write failed because this actor lost exact ownership"
                );
                Err(SetSubjectError::NotOwner)
            }
            Err(crate::XmppError::OwnershipUnavailable { entity }) => {
                tracing::warn!(
                    room = %self.room.room_jid,
                    %entity,
                    "durable subject write could not prove exact ownership"
                );
                Err(SetSubjectError::OwnershipUnavailable)
            }
            Err(error) => {
                tracing::warn!(
                    room = %self.room.room_jid,
                    %error,
                    "durable subject write failed before applying the in-memory mutation"
                );
                Err(SetSubjectError::PersistFailedBeforeApply)
            }
        }
    }

    /// Await the durable persist of one affiliation-list entry. See
    /// [`Self::persist_config`]'s FIX 2 rationale.
    async fn persist_affiliation(
        &mut self,
        jid: &BareJid,
        affiliation: Affiliation,
    ) -> Result<(), DurablePersistError> {
        self.persist_affiliation_with_phase(jid, affiliation, true)
            .await
    }

    async fn persist_affiliation_before_apply(
        &mut self,
        jid: &BareJid,
        affiliation: Affiliation,
    ) -> Result<(), DurablePersistError> {
        self.persist_affiliation_with_phase(jid, affiliation, false)
            .await
    }

    async fn persist_affiliation_with_phase(
        &mut self,
        jid: &BareJid,
        affiliation: Affiliation,
        mutation_applied: bool,
    ) -> Result<(), DurablePersistError> {
        let Some(store) = self.durable_store.clone() else {
            return Ok(());
        };
        let fence = self
            .durable_claim_fence
            .clone()
            .ok_or(DurablePersistError::OwnershipUnavailable)?;
        let entry = super::affiliation::AffiliationEntry::new(jid.clone(), affiliation);
        let result = store
            .save_affiliation_fenced(&self.room.room_jid, &entry, &fence)
            .await;
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                Err(self.classify_durable_persist_error(error, "affiliation", mutation_applied))
            }
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
    type Reply = Result<(), RoomActorError>;

    async fn handle(
        &mut self,
        msg: HydrateDurableRecipients,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.gate_serving_activity()?;
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
        Ok(())
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
    type Reply = Result<(), RoomActorError>;

    async fn handle(
        &mut self,
        msg: RestoreDurableRoomState,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.blocks_recovery_replay() {
            return Err(RoomActorError::RoomSealed);
        }
        if self.seal_state == RoomSealState::OwnershipLost {
            // Ownership loss is terminal for this incarnation. A duplicate
            // restore is an idempotent convergence message: do not retry the
            // stale fence or replace the retained store/fence identity.
            return Ok(());
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
                return Ok(());
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
        Ok(())
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
        self.gate_serving_activity()?;
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
    type Reply = Result<Option<OccupantInfo>, RoomActorError>;

    async fn handle(
        &mut self,
        msg: GetOccupantByJid,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.gate_serving_activity()?;
        Ok(self
            .room
            .find_occupant_by_real_jid(&msg.jid)
            .map(OccupantInfo::from_occupant))
    }
}

/// Look up an occupant by their nickname.
pub struct GetOccupantByNick {
    pub nick: String,
}

impl kameo::message::Message<GetOccupantByNick> for RoomActor {
    type Reply = Result<Option<OccupantInfo>, RoomActorError>;

    async fn handle(
        &mut self,
        msg: GetOccupantByNick,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.gate_serving_activity()?;
        Ok(self
            .room
            .get_occupant(&msg.nick)
            .map(OccupantInfo::from_occupant))
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
    type Reply = Result<RoomInfo, RoomActorError>;

    async fn handle(
        &mut self,
        _msg: GetInfo,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.gate_serving_activity()?;
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
    type Reply = Result<RoomConfig, RoomActorError>;

    async fn handle(
        &mut self,
        _msg: GetConfig,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.gate_serving_activity()?;
        Ok(self.room.config.clone())
    }
}

/// Replace the room configuration.
pub struct UpdateConfig {
    pub config: RoomConfig,
}

impl kameo::message::Message<UpdateConfig> for RoomActor {
    type Reply = Result<u64, RoomMutationError>;

    async fn handle(
        &mut self,
        msg: UpdateConfig,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.gate_mutation().await?;
        self.replace_config(msg.config);
        self.config_revision = self.config_revision.saturating_add(1);
        self.advance_room_admission_revision();
        self.persist_config().await?;
        Ok(self.config_revision)
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
    type Reply = Result<bool, RoomMutationError>;

    async fn handle(
        &mut self,
        msg: RollbackConfigIfRevision,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.config_revision != msg.expected_revision {
            return Ok(false);
        }
        self.gate_mutation().await?;
        self.replace_config(msg.config);
        self.config_revision = self.config_revision.saturating_add(1);
        self.advance_room_admission_revision();
        self.persist_config().await?;
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
    #[error("this actor became non-serving after the in-memory mutation was applied")]
    OwnershipLostAfterApply,
    #[error("this room's ownership is temporarily unavailable")]
    OwnershipUnavailable,
    /// FIX 2: see [`AdminApplyError::PersistFailed`]'s doc comment.
    #[error("durable persist failed after the in-memory mutation committed")]
    PersistFailed,
}

impl From<RoomMutationError> for UpdateGroupDmConfigByMemberError {
    fn from(error: RoomMutationError) -> Self {
        match error {
            RoomMutationError::NotOwner => UpdateGroupDmConfigByMemberError::NotOwner,
            RoomMutationError::OwnershipLostAfterApply => {
                UpdateGroupDmConfigByMemberError::OwnershipLostAfterApply
            }
            RoomMutationError::OwnershipUnavailable => {
                UpdateGroupDmConfigByMemberError::OwnershipUnavailable
            }
            RoomMutationError::PersistFailed => UpdateGroupDmConfigByMemberError::PersistFailed,
        }
    }
}

impl From<DurablePersistError> for UpdateGroupDmConfigByMemberError {
    fn from(error: DurablePersistError) -> Self {
        match error {
            DurablePersistError::NotOwner => UpdateGroupDmConfigByMemberError::NotOwner,
            DurablePersistError::OwnershipLostAfterApply => {
                UpdateGroupDmConfigByMemberError::OwnershipLostAfterApply
            }
            DurablePersistError::OwnershipUnavailable => {
                UpdateGroupDmConfigByMemberError::OwnershipUnavailable
            }
            DurablePersistError::PersistFailed => UpdateGroupDmConfigByMemberError::PersistFailed,
        }
    }
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
        self.gate_mutation().await?;
        let mut config = msg.config;
        config.group_dm = true;
        self.replace_config(config);
        self.config_revision = self.config_revision.saturating_add(1);
        self.advance_room_admission_revision();
        self.persist_config().await?;
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
}

impl From<PreMutationOwnershipError> for SetSubjectError {
    fn from(error: PreMutationOwnershipError) -> Self {
        match error {
            PreMutationOwnershipError::NotOwner => Self::NotOwner,
            PreMutationOwnershipError::OwnershipUnavailable => Self::OwnershipUnavailable,
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
        self.persist_subject_before_apply(Some(&subject)).await?;
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
        self.gate_serving_activity()?;
        match msg.change {
            PinStateChange::Pin(entry) => {
                self.room.upsert_pin(entry);
            }
            PinStateChange::Unpin { target_stanza_id } => {
                self.room.remove_pin_by_target(&target_stanza_id);
            }
        }
        Ok(())
    }
}

/// Read the room's pin list (#414). Returns the current pinned entries
/// in pin-time-desc order. Used by the IQ query handler for
/// `<query xmlns='urn:waddle:pin:0'/>` and by the chat-side hydration
/// path on room entry.
pub struct GetPinList;

impl kameo::message::Message<GetPinList> for RoomActor {
    type Reply = Result<Vec<PinnedEntry>, RoomActorError>;

    async fn handle(
        &mut self,
        _msg: GetPinList,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.gate_serving_activity()?;
        Ok(self.room.pinned_entries().to_vec())
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
        self.gate_mutation().await?;
        if self.invite_rollback_pending(&msg.jid) {
            return Err(AffiliationMutationError::InviteRollbackPending);
        }
        self.invalidate_invite_grant(&msg.jid);
        let needs_rehydration = self.prune_durable_recipient_if_removed(&msg.jid, msg.affiliation);
        if self
            .room
            .set_affiliation(msg.jid.clone(), msg.affiliation)
            .is_some()
        {
            self.advance_member_admission_revision(&msg.jid);
            self.persist_affiliation(&msg.jid, msg.affiliation).await?;
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
    type Reply = Result<Affiliation, RoomActorError>;

    async fn handle(
        &mut self,
        msg: GetAffiliation,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.gate_serving_activity()?;
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
    type Reply = Result<Vec<AffiliationEntry>, RoomActorError>;

    async fn handle(
        &mut self,
        msg: ListAffiliations,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.gate_serving_activity()?;
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
        Ok(entries)
    }
}

/// List all current occupants.
pub struct ListOccupants;

impl kameo::message::Message<ListOccupants> for RoomActor {
    type Reply = Result<Vec<OccupantInfo>, RoomActorError>;

    async fn handle(
        &mut self,
        _msg: ListOccupants,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.gate_serving_activity()?;
        Ok(self
            .room
            .occupants
            .values()
            .map(OccupantInfo::from_occupant)
            .collect())
    }
}

/// Get the number of occupants currently in the room.
pub struct OccupantCount;

impl kameo::message::Message<OccupantCount> for RoomActor {
    type Reply = Result<usize, RoomActorError>;

    async fn handle(
        &mut self,
        _msg: OccupantCount,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.gate_serving_activity()?;
        Ok(self.room.occupant_count())
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
            dormant: self.seal_state.is_sealed()
                || (self.room.is_dormant() && !self.has_lifecycle_fenced_invite_operation()),
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

/// Read the exact durable claim retained by this actor incarnation.
///
/// This is a typed, read-only diagnostic for ownership convergence. It must
/// never be used as dispatch authorization: [`GetRoomSnapshot`] remains the
/// serving boundary and rejects every sealed actor.
pub struct GetRetainedRoomClaimFence;

impl kameo::message::Message<GetRetainedRoomClaimFence> for RoomActor {
    type Reply = Option<super::durable::RoomClaimFenceContext>;

    async fn handle(
        &mut self,
        _msg: GetRetainedRoomClaimFence,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.durable_claim_fence.clone()
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
            // Graceful shutdown owns this actor's terminal retirement and
            // exact claim release. The inactivity reaper must leave it in
            // place rather than racing that ordered sequence.
            RoomSealState::Shutdown => return SealIfInactiveOutcome::Refused,
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
    type Reply = Result<(), RoomActorError>;

    async fn handle(
        &mut self,
        _msg: Destroy,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.gate_serving_activity()?;
        self.room.occupants.clear();
        Ok(())
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
    type Reply = Result<RoomSnapshot, RoomActorError>;

    async fn handle(
        &mut self,
        _msg: GetSnapshot,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        // The full snapshot drives roster/config responses, external archive
        // authorization, and fan-out. Keep it behind the serving seal; narrow
        // typed diagnostics have dedicated messages.
        self.gate_serving_activity()?;
        Ok(RoomSnapshot {
            room: self.room.clone(),
            config_revision: self.config_revision,
            admission_revision: self.admission_revision,
        })
    }
}

/// Install the terminal graceful-shutdown seal at this exact mailbox
/// position.
///
/// A successful reply proves every message queued before this one finished,
/// including any awaited fenced durable write. Messages queued later still
/// reach the live actor until the drain retires it, but the central serving
/// gate and typed handler outcomes refuse admissions, occupancy/presence/call
/// updates, self-ping and occupant authorization reads, pin/config/admin/
/// affiliation/invite mutations, restore/hydration, destruction, and
/// broadcast/dispatch preparation. Harmless diagnostic reads remain available
/// to the drain. The transition is idempotent. A separate shutdown overlay
/// blocks ownership-loss recovery replays without erasing that definitive
/// provenance from [`GetRoomSealState`].
pub struct SealForShutdown;

impl kameo::message::Message<SealForShutdown> for RoomActor {
    type Reply = RoomSealState;

    async fn handle(
        &mut self,
        _msg: SealForShutdown,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.shutdown_seal_installed = true;
        if self.seal_state != RoomSealState::OwnershipLost {
            self.seal_state = RoomSealState::Shutdown;
        }
        self.seal_state
    }
}

/// Internal liveness probe for the owner-veto path (ADR-0017 Phase 3 Slice
/// 3, element 4's "Unwedge" text) — the `RoomActor` counterpart of
/// `registry::user_actor::HealthCheck`: a successful reply proves this
/// actor's mailbox loop is live and responsive right now, since kameo
/// processes a mailbox strictly in order. Production callers:
/// `RoomLocalClaims::health_check` (the Slice 7 owner-veto/reconciliation
/// path). Graceful drain uses [`SealForShutdown`] instead so the same mailbox
/// barrier also prevents any later mutation from starting before retirement
/// and claim release.
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
