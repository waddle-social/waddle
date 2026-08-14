//! Durable per-lifecycle FIFO outbox for MUC mutation effects.
mod schema;
mod store;
mod store_jobs;
mod types;
pub use store::{
    retry_delay_ms, RoomEffectEnqueue, RoomEffectOutboxStore, BASE_RETRY_DELAY_MS,
    CLAIM_TIMEOUT_MS, MAX_ATTEMPTS, MAX_RETRY_DELAY_MS,
};
pub use types::{
    ClaimedRoomEffect, PersistedRoomEffect, RoomEffectKey, RoomEffectLastError,
    RoomEffectLeaseToken, RoomEffectOriginInstanceId, RoomEffectOutboxError,
    RoomEffectProducingNode, RoomEffectReleaseOutcome, RoomEffectRow,
};
#[cfg(test)]
mod tests;
