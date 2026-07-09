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

pub use core::{InMemorySmSessionRegistry, ReclaimedSessionHydration};
pub use cross_node_resume::{
    CrossNodeResumeOutcome, CrossNodeResumeStage, RemoteResumeAskOutcome, RemoteResumeAsker,
    StealTicket,
};
pub use resources::DetachedPresenceState;
pub use session::{DetachedSession, DetachedUnackedStanza};
pub use tombstones::RecentTombstoneRecord;
pub use traits::{SmClaimCompletion, SmRegistryError, SmSessionRegistry};

/// Default session timeout (5 minutes)
pub const DEFAULT_SESSION_TIMEOUT_SECS: u64 = 300;

/// Maximum number of sessions to store
pub const DEFAULT_MAX_SESSIONS: usize = 10000;

#[cfg(test)]
mod tests;
