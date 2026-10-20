//! Durable per-lifecycle FIFO outbox for MUC mutation effects.
pub mod drain;
pub mod render;
mod schema;
mod store;
mod store_jobs;
mod supervisor;
mod types;
pub use store::{
    retry_delay_ms, RoomEffectEnqueue, RoomEffectOutboxStore, BASE_RETRY_DELAY_MS,
    CLAIM_TIMEOUT_MS, MAX_ATTEMPTS, MAX_RETRY_DELAY_MS,
};
pub use supervisor::RoomEffectArmSupervisor;
pub use types::{
    ClaimedRoomEffect, PersistedRoomEffect, RoomEffectKey, RoomEffectLastError,
    RoomEffectLeaseToken, RoomEffectOriginInstanceId, RoomEffectOutboxError,
    RoomEffectProducingNode, RoomEffectReleaseOutcome, RoomEffectRow,
};

pub(crate) fn room_effect_origin_instance_id() -> RoomEffectOriginInstanceId {
    static INSTANCE_ID: std::sync::OnceLock<RoomEffectOriginInstanceId> =
        std::sync::OnceLock::new();
    INSTANCE_ID
        .get_or_init(|| {
            RoomEffectOriginInstanceId::new(uuid::Uuid::new_v4().to_string())
                .expect("UUID room-effect origin instance id is non-empty")
        })
        .clone()
}
#[cfg(test)]
mod tests;
