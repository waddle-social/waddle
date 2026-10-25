//! MUC Room Registry Actor
//!
//! Kameo actor that manages all MUC room actors. Replaced the DashMap-based
//! legacy registry (deleted in #1136) with a single-writer actor that owns
//! the room map and spawns per-room `RoomActor` instances on demand.

use crate::xep::xep0317::ServerHats;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use jid::BareJid;
use kameo::actor::{ActorRef, Spawn};
use kameo::error::BoxSendError;
use kameo::message::{BoxReply, Context};
use kameo::reply::{DelegatedReply, ReplySender};
use kameo::{Actor, Reply};
use thiserror::Error;
use tracing::{debug, info, warn};

use super::affiliation::DurableMembershipSource;
use super::durable::{ChannelId, MucDurableStore, WaddleId};
use super::room_actor::{
    DurableRestoreReadiness, DurableRoomOrigin, GetDurableRestoreReadiness, GetRoomSealState,
    GetSnapshot, HydrateDurableRecipients, RestoreDurableRoomState, RestoreLiveRoster, RoomActor,
    RoomSealState, SealForDestroy, SealGuard, SealIfInactive, SealIfInactiveOutcome, UnsealDestroy,
    UnsealInactive,
};
use super::{
    DestroyPassword, DestroyReason, DestroyRecipient, MucOccupantNick, MucRoom, RoomCommitError,
    RoomConfig, RoomDurableMutation, RoomMutationEffects,
};
use crate::metrics;
use crate::ownership::{
    ClaimEpoch, ClaimError, ClaimSnapshot, ClaimStore, Entity, EntityType, ExactReleaseOutcome,
    InProcessClaimStore, NodeIdentity, RolloutBackoff, SharedNodeIdentity, StalePredicate,
};
use crate::xep::xep0421::OccupantIdSecret;

mod durable_restore;
pub use durable_restore::GetOrRestoreDurableRoom;

/// Test seam: set (or age) the handoff marker for a room as if a handoff had
/// retired its actor `age` ago without publishing a successor.
#[cfg(test)]
struct MarkHandoffPendingForTest {
    room_jid: BareJid,
    age: std::time::Duration,
    restore: Option<Arc<RoomCreationSpec>>,
}

#[cfg(test)]
impl kameo::message::Message<MarkHandoffPendingForTest> for RoomRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: MarkHandoffPendingForTest,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.handoff_pending.insert(
            msg.room_jid,
            PendingHandoff {
                since: std::time::Instant::now() - msg.age,
                stashed_spec: msg.restore,
            },
        );
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

/// Test-only mailbox marker used to prove that queued work runs between
/// serialized publication-boundary fences.
#[cfg(test)]
pub(crate) struct MarkRegistryProgress(pub std::sync::Arc<std::sync::atomic::AtomicBool>);

#[cfg(test)]
impl kameo::message::Message<MarkRegistryProgress> for RoomRegistryActor {
    type Reply = ();

    async fn handle(
        &mut self,
        msg: MarkRegistryProgress,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        msg.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;

mod implementation;
pub use implementation::AbortDestroyRoomAttempt;
pub use implementation::AckDestroyCompletion;
pub use implementation::CancelDestroyCompletion;
pub use implementation::CancelDestroyCompletionAttempt;
pub use implementation::CancelPendingReclaimedRoomReservation;
pub use implementation::CreateInstantRoom;
pub use implementation::CreateRoom;
pub use implementation::CreateRoomWithInitialAffiliations;
pub use implementation::DemoteRoomIfExactActor;
pub use implementation::DemoteRoomIfOwner;
pub use implementation::DestroyCompletion;
pub use implementation::DestroyRoom;
pub use implementation::DestroyRoomIfInactive;
pub use implementation::DestroyRoomOutcome;
pub use implementation::DestroyRoomReason;
pub use implementation::DestroyRoomWithAttempt;
pub use implementation::DrainRoomOwnershipForShutdown;
pub use implementation::GetOrCreateRoom;
pub use implementation::GetOrCreateRoomWithInitialAffiliations;
pub use implementation::GetOrCreateRoomWithLiveRoster;
pub use implementation::GetOrRestoreRoom;
pub use implementation::GetPendingReclaimedRoomBacklog;
pub use implementation::GetPendingRoomReleaseBacklog;
pub use implementation::GetRoom;
pub use implementation::GetSingleNodeRoomRetirementAuthority;
pub use implementation::GuardedDestroyOutcome;
pub use implementation::IsCurrentIdentityPendingRoomReleaseOnly;
pub use implementation::IsCurrentRoomPendingRelease;
pub use implementation::IsMucJid;
pub use implementation::IsPendingRoomReleaseOnly;
pub use implementation::ListPendingReclaimedRooms;
pub use implementation::ListPendingRoomReleaseJids;
pub use implementation::ListRooms;
pub use implementation::ListRoomsOwnedBy;
pub use implementation::LocalRoomJids;
pub use implementation::PendingReclaimedRoom;
pub use implementation::PendingReclaimedRoomBacklog;
pub use implementation::PendingRoomReleaseBacklog;
pub use implementation::ReapSealedRoom;
pub use implementation::ReclaimedRoomOutcome;
pub use implementation::ReconcileReclaimedRoom;
pub use implementation::RegisterDestroyCompletion;
pub use implementation::RememberPendingReclaimedRoom;
pub use implementation::RequeueDestroyCompletion;
pub use implementation::ReservePendingReclaimedRoom;
pub use implementation::RetireUnresponsiveRoomIfExactActor;
pub use implementation::RetryPendingRoomReleases;
pub use implementation::RoomAcquisition;
pub use implementation::RoomCount;
pub use implementation::RoomCreation;
use implementation::RoomCreationSpec;
pub use implementation::RoomExists;
pub use implementation::RoomOwnershipDrainOutcome;
use implementation::RoomPreparationOrigin;
use implementation::RoomPreparationSpec;
use implementation::RoomPreparationWaiter;
pub use implementation::RoomRegistryActor;
pub use implementation::RoomRegistryError;
pub use implementation::RunAbsentRoomGuardedAsync;
pub use implementation::SealRoomForDestroySnapshot;
pub use implementation::TakeDestroyCompletionAttempt;
pub use implementation::TakeDestroyCompletions;
pub use implementation::WireClusteringClaims;
pub use implementation::HANDOFF_PENDING_WINDOW;
pub use implementation::MAX_PENDING_RECLAIMED_ROOMS;
pub use implementation::MAX_PENDING_ROOM_ACQUISITIONS;
pub use implementation::MAX_PENDING_ROOM_OWNERSHIP_RESPONSIBILITIES;
pub use implementation::MAX_PENDING_ROOM_RELEASES;
pub use implementation::MAX_ROOM_PREPARATION_WAITERS;
use implementation::ROOM_OWNERSHIP_CALL_TIMEOUT;

#[cfg(test)]
use implementation::{
    DestroyAttemptPhase, LiveRoomRestore, PendingHandoff, PendingReclaimedState,
    PendingRoomOwnershipResponsibility, PendingRoomReleaseState, RetainedDestroyAttempt,
    RetryUnpublishedPreparationDestroy, RoomEntry, UnpublishedDestroyPhase,
    UnpublishedPreparationDestroyOutcome, PENDING_ROOM_RETRY_BATCH, PENDING_ROOM_RETRY_DELAY,
    PRE_ACQUIRE_CONVERGENCE_BUDGET, RECLAIMED_ROOM_RELEASE_TIMEOUT,
    SNAPSHOT_PRESEAL_RECONCILE_DELAY,
};
