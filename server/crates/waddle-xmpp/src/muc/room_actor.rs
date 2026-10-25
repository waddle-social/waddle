//! Kameo actor wrapping a single MUC room.
//!
//! Each `RoomActor` owns a [`MucRoom`] and processes all operations
//! sequentially, removing the need for external `RwLock` synchronisation.
//! This is part of the Phase 3 actor-model migration.

use crate::muc::durable::DestroyAttemptId;
use crate::muc::MucOccupantNick;
use crate::types::Moderation;
use crate::xep::xep0272::Muji;
use crate::xep::xep0317::ServerHats;
use crate::xep::xep0421::OccupantIdSecret;
use crate::xep::InCallPresenceState;
use chrono::{DateTime, Utc};
use jid::{BareJid, FullJid};
use kameo::message::Context;
use kameo::Actor;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::convert::Infallible;
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

mod admin_handlers;
mod live_roster_restore;
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
pub use snapshot_handlers::{
    BuildGroupchatBroadcast, GetNicknameGeneration, GetReplayOccupantSessionGeneration,
    GetRoomSnapshot, GroupchatBroadcastResult, OccupancyReplayGuardOutcome, RoomChainOccupant,
    RoomChainSnapshot, RoomChildRetirementReason, RunOccupancyReplayGuarded,
    RunOccupancyReplayGuardedAsync, RunRoomChildRetirementGuardedAsync,
    RunSupersededOccupancyGuardedAsync,
};

mod extension_subscriptions;
pub use extension_subscriptions::{
    NotifyRoomExtensions, RoomExtensionSubscriber, SubscribeRoomExtension,
};

mod implementation;
use implementation::affiliation_overflows_full_room;
pub use implementation::AckDepartureOutcome;
pub use implementation::AckDepartureReceipt;
pub use implementation::AcknowledgeAdminProjection;
pub use implementation::AdminApplyError;
pub use implementation::AdminContext;
pub use implementation::AdminMutationResolution;
pub use implementation::AffiliationMutationError;
pub use implementation::ApplyPin;
pub use implementation::ChangeAffiliation;
pub use implementation::ConfigChangeNotification;
pub use implementation::ConfigEffectPlan;
pub use implementation::ConfigMutationApplied;
pub use implementation::DepartureLedger;
pub use implementation::DepartureProvenance;
pub use implementation::DepartureReceipt;
pub use implementation::DepartureReceiptOutcome;
pub use implementation::Destroy;
pub use implementation::DormancyStatus;
pub use implementation::DurablePersistError;
pub use implementation::DurableRestoreReadiness;
use implementation::DurableRestoreState;
pub use implementation::DurableRoomOrigin;
pub use implementation::GetAffiliation;
pub use implementation::GetConfig;
pub use implementation::GetDurableRestoreReadiness;
pub use implementation::GetInfo;
pub use implementation::GetOccupantByJid;
pub use implementation::GetOccupantByNick;
pub use implementation::GetOccupantSessionGeneration;
pub use implementation::GetOccupantVoice;
pub use implementation::GetPinList;
pub use implementation::GetRoomJid;
pub use implementation::GetRoomSealState;
pub use implementation::GetSnapshot;
pub use implementation::GroupDmConfigMutationApplied;
pub use implementation::HealthCheck;
pub use implementation::HydrateDurableRecipients;
pub use implementation::IsDormant;
pub use implementation::IsSealed;
pub use implementation::Join;
pub use implementation::JoinDenialReason;
pub use implementation::JoinExistingOccupant;
pub use implementation::JoinOutcome;
pub use implementation::LeaveOutcome;
pub use implementation::ListAffiliations;
pub use implementation::ListOccupants;
pub use implementation::LiveRosterRestoreError;
pub use implementation::OccupancyWatermark;
pub use implementation::OccupantCount;
pub use implementation::OccupantInfo;
pub use implementation::OccupantVoices;
pub use implementation::PendingAdminProjection;
pub use implementation::PresenceUpdateOutcome;
pub(super) use implementation::ProjectionGate;
pub use implementation::ProjectionRefusal;
pub use implementation::RestoreDurableRoomState;
pub use implementation::RestoreLiveRoster;
pub(super) use implementation::RetainedDeparture;
pub use implementation::RollbackConfigIfRevision;
pub use implementation::RoomActor;
pub use implementation::RoomActorError;
pub use implementation::RoomInfo;
pub use implementation::RoomMutationError;
pub use implementation::RoomSealState;
pub use implementation::RoomSnapshot;
pub use implementation::SealForDestroy;
pub use implementation::SealGuard;
pub use implementation::SealIfInactive;
pub use implementation::SealIfInactiveOutcome;
pub use implementation::SetSubject;
pub use implementation::SetSubjectError;
pub use implementation::UnsealDestroy;
pub use implementation::UnsealInactive;
pub use implementation::UpdateConfig;
pub use implementation::UpdateGroupDmConfigByMember;
pub use implementation::UpdateGroupDmConfigByMemberError;
pub(crate) use implementation::JOIN_OWNERSHIP_CHECK_TIMEOUT;

#[cfg(test)]
use implementation::MAX_MEMBER_ADMISSION_REVISIONS;
