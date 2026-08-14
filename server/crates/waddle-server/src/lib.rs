#![recursion_limit = "256"]

pub mod admin;
pub mod auth;
pub mod call_teardown_outbox;
pub mod channel_space_links;
pub mod clustering;
pub mod config;
pub mod db;
pub mod dnd_projection;
pub mod dnd_reader;
pub mod inbox;
/// Non-blocking shadow executor for durable SM ingress frontiers.
pub mod ingress_shadow;
/// Dark Postgres-only ingress identity substrate (#1653), consumed by tests
/// now and by #1654 repositories later.
pub mod ingress_substrate;
/// Atomic PostgreSQL ingress transaction seam (#1654).
pub mod ingress_uow;
pub mod muc_destroy_completion_outbox;
/// Postgres-backed durable MUC room ownership state (ADR-0017 Phase 3
/// Slice 7). Gated behind the `clustering` Cargo feature for the same
/// reason as `sm_persistence_fenced`: it depends on `clustering::relay`/
/// `clustering::NodeId`, which only exist there.
#[cfg(feature = "clustering")]
pub mod muc_durable;
pub mod notification_activity;
pub mod notification_outbox;
pub mod notification_settings_projection;
pub mod pending_delivery;
pub mod pep_feed_bridge;
pub mod permissions;
pub mod profile;
pub mod pubsub;
pub mod pubsub_authz;
pub mod push_registrations;
pub mod push_service;
pub mod room_policy;
pub mod server;
pub mod sm_persistence;
/// Postgres-fenced `SmPersistenceStorage` (ADR-0017 Phase 3 Slice 4).
/// Gated behind the `clustering` Cargo feature, matching every other
/// Postgres-cluster-only module (`clustering::claims`, `clustering::lease`,
/// …): a default build links none of this, and `sm_persistence::
/// open_for_cluster_mode` — itself unconditionally compiled — is the only
/// call site that ever names it.
#[cfg(feature = "clustering")]
pub mod sm_persistence_fenced;
pub mod sm_promotion;
pub mod space_identity;
pub mod spaces_metadata;
pub mod spaces_pubsub_seed;
pub mod storage;
pub mod telemetry;
pub mod threads;
pub mod time;
pub mod vcard;

pub use config::{ServerConfig, ServerMode};
