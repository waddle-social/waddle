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
/// Transactional ingress authority and bounded post-commit execution.
pub mod ingress;
/// Durable ingress authority for message identity, effects, and SM frontiers.
/// Dark Postgres-only ingress identity substrate (#1653), consumed by tests
/// now and by #1654 repositories later.
pub mod ingress_substrate;
/// Atomic PostgreSQL ingress transaction seam (#1654).
pub mod ingress_uow;
pub mod mam_runtime;
pub mod metrics;
pub mod muc_destroy_completion_outbox;
/// File-armed seam on the owner-IQ room destroy reply, for the cluster
/// suite. Absent from every build without the `test-support` feature.
#[cfg(feature = "test-support")]
pub(crate) mod muc_destroy_completion_seam;
/// Postgres-backed durable MUC room ownership state (ADR-0017 Phase 3
/// Slice 7). Gated behind the `clustering` Cargo feature for the same
/// reason as `sm_persistence_fenced`: it depends on `clustering::relay`/
/// `clustering::NodeId`, which only exist there.
#[cfg(feature = "clustering")]
pub mod muc_durable;
pub mod notification_activity;
pub mod notification_outbox;
pub mod notification_settings_projection;
pub mod occupancy_authority;
/// Operator subcommands of the server binary that prepare a PostgreSQL
/// database for the restricted runtime role.
pub mod owner_cli;
pub mod pending_delivery;
pub mod pep_feed_bridge;
pub mod permissions;
pub mod profile;
pub mod pubsub;
pub mod pubsub_authz;
pub mod push_registrations;
pub mod push_service;
/// Durable, FIFO room-mutation broadcast effect queue (#1646).
pub mod room_effect_outbox;
pub(crate) mod room_observation;
pub mod room_policy;
pub mod server;
/// The one enclosing graceful-shutdown deadline every stage derives from.
pub mod shutdown_deadline;
/// Unselected PostgreSQL custody candidate; no release transport calls this API.
#[cfg(all(feature = "test-support", feature = "clustering"))]
pub mod sm_custody;
/// Retained occurrence model for an unselected SM custody experiment.
#[doc(hidden)]
#[path = "sm_custody/frame_occurrence.rs"]
pub mod sm_frame_occurrence;
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
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod threads;
pub mod time;
pub mod vcard;

pub use config::{ServerConfig, ServerMode};

/// Supervised WebSocket transport primitives for the future connection owner.
#[doc(hidden)]
pub mod websocket_transport;
