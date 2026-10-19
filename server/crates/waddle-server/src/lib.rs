#![recursion_limit = "256"]

pub mod admin;
pub mod auth;
pub mod call_teardown_outbox;
pub mod channel_space_links;
pub mod clustering;
pub mod config;
pub mod db;
pub mod dnd;
pub mod inbox;
/// Transactional ingress authority and bounded post-commit execution.
pub mod ingress;
pub mod metrics;
pub mod muc;
pub mod notification;
pub mod occupancy_authority;
pub mod pending_delivery;
pub mod pep_feed_bridge;
pub mod permissions;
pub mod profile;
pub mod pubsub;
pub mod push;
pub mod room;
pub mod server;
pub mod sm;
pub mod spaces;
pub mod storage;
pub mod telemetry;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod threads;
pub mod time;
pub mod vcard;

pub use config::{ServerConfig, ServerMode};
