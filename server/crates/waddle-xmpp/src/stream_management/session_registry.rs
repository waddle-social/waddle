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
mod session;
mod tombstones;
mod trait_impl;
mod traits;

pub use claims::PendingPromotionRetryRetention;
pub use core::{InMemorySmSessionRegistry, ReclaimedClaimReservation, ReclaimedHydrationOutcome};
pub use cross_node_resume::{
    CrossNodeResumeOutcome, CrossNodeResumeStage, RemoteResumeAskOutcome, RemoteResumeAsker,
    StealTicket,
};
pub use resources::{DetachedPresenceState, ResumableSessionProbe};
pub use session::{DetachedSession, DetachedUnackedStanza};
pub use tombstones::{RecentTombstoneRecord, TOMBSTONE_CLOCK_SKEW_SLACK};
pub use traits::{
    SmClaimCompletion, SmRegistryError, SmSessionRegistry, TombstoneScrubbedSmEntries,
    TombstoneScrubbedSmEntry,
};

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

/// Default session timeout (5 minutes)
pub const DEFAULT_SESSION_TIMEOUT_SECS: u64 = 300;

/// Maximum number of sessions to store
pub const DEFAULT_MAX_SESSIONS: usize = 10000;

#[cfg(test)]
mod tests;
