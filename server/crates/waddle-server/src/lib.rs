#![recursion_limit = "256"]

pub mod admin;
pub mod auth;
pub mod channel_space_links;
pub mod clustering;
pub mod config;
pub mod db;
pub mod dnd_projection;
pub mod dnd_reader;
pub mod inbox;
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
