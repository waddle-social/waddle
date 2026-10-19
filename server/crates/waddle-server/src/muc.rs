//! Durable MUC room ownership and the destroy-completion outbox.
pub mod destroy_completion_outbox;

/// Postgres-backed durable MUC room ownership state (ADR-0017 Phase 3
/// Slice 7). Gated behind the `clustering` Cargo feature for the same
/// reason as `sm::persistence::fenced`: it depends on `clustering::relay`/
/// `clustering::NodeId`, which only exist there.
#[cfg(feature = "clustering")]
pub mod durable;
