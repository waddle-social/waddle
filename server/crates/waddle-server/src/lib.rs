#![recursion_limit = "256"]

pub mod auth;
pub mod config;
pub mod db;
pub mod inbox;
pub mod messages;
pub mod permissions;
pub mod pubsub;
pub mod pubsub_authz;
pub mod server;
pub mod storage;
pub mod telemetry;
pub mod time;
pub mod vcard;

pub use config::{ServerConfig, ServerMode};
