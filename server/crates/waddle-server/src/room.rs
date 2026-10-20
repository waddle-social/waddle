//! Room-scoped effect outbox, observation, and policy.

/// Durable, FIFO room-mutation broadcast effect queue (#1646).
pub mod effect_outbox;
pub(crate) mod observation;
pub mod policy;
