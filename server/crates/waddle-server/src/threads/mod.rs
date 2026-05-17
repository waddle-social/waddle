//! Waddle threads — per-thread cross-channel aggregation view.
//!
//! This module carries the IQ-level protocol for the global threads view:
//! a single query that returns an ordered list of threads the user has
//! participated in or has unread in, across every channel.
//!
//! Wire shape: `urn:waddle:threads:0`. See
//! `docs/superpowers/specs/2026-05-17-threads-design.md` for the contract.
//!
//! Data source: the existing `inbox_entries` table (rows with non-empty
//! `thread_id`). No new schema.

pub mod query;
pub mod storage;
pub mod wire;
