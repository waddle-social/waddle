//! Durable retry queue for call teardown side effects.
//!
//! The outbox stores typed teardown intent before the server-specific drain
//! decides whether this node owns the relevant clustered scope and executes
//! the effect. Raw 1:1 call IDs have no room JID and may be drained by any
//! node because their live call registries are node-local.

mod drain;
mod producer;
mod schema;
mod store;
mod types;

pub(crate) use drain::drain_due;
pub(crate) use producer::CallTeardownPersistenceSupervisor;

pub use store::{
    retry_delay_ms, CallTeardownOutboxStore, BASE_RETRY_DELAY_MS, CLAIM_TIMEOUT_MS,
    FAILED_RETENTION_MS, MAX_ATTEMPTS, MAX_RETRY_DELAY_MS,
};
pub use types::{
    CallTeardownIntent, CallTeardownIntentId, CallTeardownJob, CallTeardownLastError,
    CallTeardownOutboxError, CallTeardownQueueStats, CallTeardownRetryOutcome,
    CallTeardownRetryReason, CallTeardownStatus, ClaimToken, TeardownTarget,
};

#[cfg(test)]
mod tests;
