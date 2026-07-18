//! Session Registry for XEP-0198 Stream Management
//!
//! This module provides server-side storage for detached stream sessions,
//! allowing streams to be resumed after disconnection.
//!
//! When a client disconnects with SM enabled and resumption requested,
//! the server stores the session state. When the client reconnects with
//! a resume request, the server can restore the session.

mod claims;
mod core;
mod cross_node_resume;
mod persistence_codec;
mod resources;
mod sequence;
mod session;
mod tombstones;
mod trait_impl;
mod traits;

pub use core::{InMemorySmSessionRegistry, ReclaimedClaimReservation, ReclaimedHydrationOutcome};
pub use cross_node_resume::{
    CrossNodeResumeOutcome, CrossNodeResumeStage, RemoteResumeAskOutcome, RemoteResumeAsker,
    StealTicket,
};
pub use resources::{DetachedPresenceState, ResumableSessionProbe};
pub use session::{
    DetachedReplaySequenceConflict, DetachedSession, DetachedUnackedStanza, SmSessionGenerationId,
};
pub use tombstones::{RecentTombstoneRecord, TOMBSTONE_CLOCK_SKEW_SLACK};
pub use traits::{SmClaimCompletion, SmRegistryError, SmSessionRegistry};

/// Exclusive authority for one fixed SM stream-lock shard.
///
/// Callers that must establish node-incarnation authority after serializing
/// stream state use this guard to preserve the global shard-before-identity
/// lock order across an external protocol transaction.
pub struct SmSessionOperationGuard {
    pub(self) stream_id: String,
    pub(self) shard: std::sync::Arc<tokio::sync::Mutex<()>>,
    pub(self) _guard: tokio::sync::OwnedMutexGuard<()>,
}

/// Exclusive authority for one current-generation persistence mutation.
///
/// The owned stream-shard guard prevents local demotion from revoking the
/// generation while a pending-delivery or durable-storage await is in flight.
/// Borrowing the exact promotion lease prevents that lease from being retired
/// while this guard exists.
#[must_use = "current-generation mutation authority must remain held through the storage await"]
pub struct SmCurrentPromotionMutationGuard<'lease> {
    pub(self) _operation: SmSessionOperationGuard,
    pub(self) lease: &'lease SmSessionPromotionLease,
}

impl SmCurrentPromotionMutationGuard<'_> {
    pub fn session_id(&self) -> &crate::pending_delivery::SmSessionId {
        self.lease.session_id()
    }

    pub fn claim_fence(&self) -> Option<&super::persistence::SmClaimFence> {
        self.lease.claim_fence()
    }
}

/// Exclusive authority for one exact terminal-generation persistence mutation.
///
/// Unlike [`SmCurrentPromotionMutationGuard`], this guard never grants access
/// to the bare current-session row or its successor-linked pending rows.  Its
/// key includes the immutable local generation so every storage mutation is
/// confined to the archived terminal carrier that the lease reserved.
#[must_use = "terminal-generation mutation authority must remain held through the storage await"]
pub struct SmTerminalPromotionMutationGuard<'lease> {
    pub(self) _operation: SmSessionOperationGuard,
    pub(self) lease: &'lease SmSessionPromotionLease,
}

impl SmTerminalPromotionMutationGuard<'_> {
    pub fn key(&self) -> super::persistence::SmTerminalGenerationKey {
        super::persistence::SmTerminalGenerationKey::new(
            self.lease.stream_id.clone(),
            self.lease.generation_id,
        )
    }

    pub fn claim_fence(&self) -> Option<&super::persistence::SmClaimFence> {
        self.lease.claim_fence()
    }
}

/// The authority held by one exact process-local promotion generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmSessionPromotionAuthority {
    /// This generation owns the durable SM row and its linked pending rows.
    CurrentDurable,
    /// This generation owns one exact non-resumable terminal snapshot.
    /// It must never inspect or mutate same-stream successor-linked state.
    TerminalDurable,
    /// A same-stream successor owns the bare durable state. This generation
    /// may only deliver its own payload and retire its own local token.
    ObsoleteGeneration,
}

/// Exact result of retiring one promotion generation under its lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmSessionDrainConfirmation {
    CurrentDurableConfirmed,
    TerminalDurableConfirmed,
    ObsoleteGenerationRetired,
    /// Q6 payload delivery and exact durable-generation deletion completed,
    /// but the shared stream claim still needs an asynchronous empty-work
    /// proof before its exact release can finish.
    PayloadRetiredClaimReconciliationPending,
    AuthorityLost,
    Unconfirmed,
}

/// Claim responsibilities still associated with graceful SM shutdown.
///
/// `exact` counts immutable `(stream id, owner, epoch)` fences and therefore
/// preserves distinct same-stream claim generations. `unknown` counts only
/// capacity reservations whose ownership mutation may have committed but has
/// not yielded an exact fence yet. Recovery-only payload carriers are absent
/// from both counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SmShutdownClaimResponsibilityCounts {
    pub exact: usize,
    pub unknown: usize,
}

/// Outcomes from one bounded pass over exact claim-release handoffs.
///
/// Hydration and acquisition reconciliation can consume the shared retry
/// budget without reaching an exact release. Those operations are absent,
/// but an exact release performed while reconciling an acquisition is
/// included. Every attempted exact fence contributes to exactly one of
/// `released`, `disproved`, or `retained`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SmClaimReleaseRetrySummary {
    pub attempted: usize,
    pub released: usize,
    pub disproved: usize,
    pub retained: usize,
}

impl SmClaimReleaseRetrySummary {
    fn record(&mut self, outcome: SmClaimReleaseRetryOutcome) {
        self.attempted += 1;
        match outcome {
            SmClaimReleaseRetryOutcome::Released => self.released += 1,
            SmClaimReleaseRetryOutcome::Disproved => self.disproved += 1,
            SmClaimReleaseRetryOutcome::Retained => self.retained += 1,
        }
    }
}

/// Exact result of one local owner+epoch release attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmClaimReleaseRetryOutcome {
    Released,
    Disproved,
    Retained,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SmPromotionLeaseNonce(uuid::Uuid);

impl SmPromotionLeaseNonce {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

/// Logical same-stream authority held from before Q6 promotion through the
/// last pending-row and durable-retirement side effect.
#[must_use = "the promotion lease must remain alive through terminal retirement"]
pub struct SmSessionPromotionLease {
    pub(self) stream_id: crate::pending_delivery::SmSessionId,
    pub(self) generation_id: SmSessionGenerationId,
    pub(self) authority: SmSessionPromotionAuthority,
    pub(self) claim_fence: Option<super::persistence::SmClaimFence>,
    pub(self) nonce: SmPromotionLeaseNonce,
    pub(self) pending_promotions: std::sync::Arc<std::sync::RwLock<core::PendingPromotions>>,
    pub(self) reservation_active: bool,
}

impl SmSessionPromotionLease {
    pub fn authority(&self) -> SmSessionPromotionAuthority {
        self.authority
    }

    pub fn claim_fence(&self) -> Option<&super::persistence::SmClaimFence> {
        self.claim_fence.as_ref()
    }

    pub fn session_id(&self) -> &crate::pending_delivery::SmSessionId {
        &self.stream_id
    }
}

impl Drop for SmSessionPromotionLease {
    fn drop(&mut self) {
        if !self.reservation_active {
            return;
        }
        if let Ok(mut promotions) = self.pending_promotions.write() {
            promotions.release_reservation(self.stream_id.as_str(), self.generation_id, self.nonce);
        }
    }
}

/// Default session timeout (5 minutes)
pub const DEFAULT_SESSION_TIMEOUT_SECS: u64 = 300;

/// Maximum number of sessions to store
pub const DEFAULT_MAX_SESSIONS: usize = 10000;

#[cfg(test)]
mod tests;
