//! MAM storage trait and sqlx-backed implementations.
//!
//! Provides persistent storage for archived messages (XEP-0313).
//! The storage layer supports:
//! - Storing messages with unique archive IDs
//! - Querying with time-based and sender filters
//! - RSM (Result Set Management) pagination

mod error;
mod in_memory;
mod origin_dedup;
mod query_semantics;
mod sqlx_store;
mod tombstone;
mod traits;

#[cfg(test)]
mod tests;

pub use error::MamStorageError;
pub use in_memory::InMemoryMamStorage;
pub use sqlx_store::SqlxMamStorage;
pub use traits::{MamArchiveKind, MamStorage, StoreOutcome, TerminalTombstoneOutcome};
