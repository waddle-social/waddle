#![recursion_limit = "256"]

pub mod admin;
pub mod auth;
pub mod channel_space_links;
pub mod config;
pub mod db;
pub mod inbox;
pub mod messages;
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
pub mod server;
pub mod sm_persistence;
pub mod sm_promotion;
pub mod spaces_metadata;
pub mod spaces_pubsub_seed;
pub mod storage;
pub mod telemetry;
pub mod threads;
pub mod time;
pub mod vcard;

pub use config::{ServerConfig, ServerMode};
