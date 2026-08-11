//! XEP-0198 Stream Management Implementation
//!
//! This module implements Stream Management as defined in XEP-0198,
//! providing reliability features for XMPP streams including:
//!
//! - Stanza acknowledgments (tracking which stanzas have been received)
//! - Stream resumption (reconnecting without losing messages)
//! - Unacknowledged stanza queuing (for resending after resume)
//!
//! ## Protocol Overview
//!
//! Stream Management adds the following elements in the `urn:xmpp:sm:3` namespace:
//! - `<enable/>` - Client request to enable stream management
//! - `<enabled/>` - Server confirmation that SM is enabled
//! - `<r/>` - Request acknowledgment of received stanzas
//! - `<a h='N'/>` - Acknowledge receipt of N stanzas
//! - `<resume/>` - Request to resume a previous stream
//! - `<resumed/>` - Confirmation that stream was resumed
//! - `<failed/>` - Stream management operation failed
//!
//! ## Architecture
//!
//! - `StreamManagementState` - Per-connection SM state (counters, queue)
//! - `SmSessionRegistry` - Server-wide registry for detached resumable sessions
//! - `UnackedQueue` - Queue of unacknowledged outbound stanzas

pub mod persistence;
mod replay;
pub(crate) mod sequence;
mod session_registry;
mod stanzas;
#[cfg(test)]
mod stanzas_tests;
mod state;
mod unacked_queue;

pub use replay::{stamp_replay_delay, ReplayStanza};
pub use session_registry::{
    CrossNodeResumeOutcome, CrossNodeResumeStage, DetachedPresenceState, DetachedSession,
    DetachedUnackedStanza, InMemorySmSessionRegistry, PendingPromotionRetryRetention,
    RecentTombstoneRecord, ReclaimedClaimReservation, ReclaimedHydrationOutcome,
    RemoteResumeAskOutcome, RemoteResumeAsker, ResumableSessionProbe, SmClaimCompletion,
    SmRegistryError, SmSessionRegistry, StealTicket, DEFAULT_MAX_SESSIONS,
    TOMBSTONE_CLOCK_SKEW_SLACK,
};
pub use stanzas::{SmAck, SmEnable, SmEnabled, SmFailed, SmRequest, SmResume, SmResumed, SmStanza};
pub use state::{DetachedSessionSnapshot, StreamManagementState};
pub use unacked_queue::{UnackedPushResult, UnackedQueue, UnackedStanza};

/// XEP-0198 Stream Management namespace (version 3)
pub const SM_NS: &str = "urn:xmpp:sm:3";

/// Default maximum unacked queue size (stanzas)
pub const DEFAULT_MAX_UNACKED_QUEUE_SIZE: usize = 1000;

/// Default ack request threshold (request ack after this many unacked stanzas)
pub const DEFAULT_ACK_REQUEST_THRESHOLD: u32 = 5;
