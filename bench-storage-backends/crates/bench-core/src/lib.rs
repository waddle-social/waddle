//! Shared types, traits, workload generator and metrics for the
//! Waddle XMPP stanza-database benchmark suite.
//!
//! The schema in [`ArchivedMessage`] is deliberately kept byte-compatible
//! with the `mam_messages` table in
//! `waddle/server/crates/waddle-xmpp/src/mam/storage.rs` so that benchmark
//! results transfer directly to the production server.

pub mod message;
pub mod metrics;
pub mod mock;
pub mod report;
pub mod store;
pub mod workload;

pub use message::{ArchivedMessage, MamQuery, MessageType};
pub use metrics::{op_stats_from_hist, LatencyRecorder, OpKind, OpStats};
pub use report::RunReport;
pub use store::{StanzaStore, StoreError};
pub use workload::{Scale, WorkloadConfig, WorkloadRunner};
