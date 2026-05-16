#![recursion_limit = "256"]

pub mod auth;
pub mod config;
pub mod db;
pub mod inbox;
pub mod messages;
pub mod notification_settings_projection;
pub mod pending_delivery;
pub mod permissions;
pub mod profile;
pub mod pubsub;
pub mod pubsub_authz;
pub mod push_registrations;
pub mod server;
pub mod sm_persistence;
pub mod sm_promotion;
pub mod spaces_pubsub_seed;
pub mod storage;
pub mod telemetry;
pub mod time;
pub mod vcard;

pub use config::{ServerConfig, ServerMode};
