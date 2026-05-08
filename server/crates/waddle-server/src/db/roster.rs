//! Database-backed roster storage for RFC 6121 compliance.
//!
//! This module implements the `RosterStorage` trait from `waddle-xmpp` using
//! the internal SQLx-backed database adapter for persistent storage.

mod mutation;
mod query;
mod retry;
#[cfg(test)]
mod tests;
mod types;

use dashmap::DashMap;
use jid::BareJid;
use std::sync::Arc;
use tokio::sync::Mutex;

use super::Database;

pub use types::{
    RosterItemRow, RosterRowChange, RosterRowMutation, RosterRowMutationKind, RosterStorageError,
    UserMutationLock,
};

/// Database-backed roster storage implementation.
///
/// Stores roster items in the `roster_items` table and manages roster
/// versioning via the `roster_versions` table.
///
/// Mutations go through [`DatabaseRosterStorage::apply_roster_change`] which
/// serialises per-user writes via an in-process mutex map and returns the new
/// `RosterVersion` from the same call. Splitting the mutation and version
/// read into separate awaits would race with concurrent callers and violate
/// XEP-0237 §2.6's "version on each push MUST be unique" / "in order of
/// modification" requirements.
#[derive(Clone)]
pub struct DatabaseRosterStorage {
    pub(super) db: Database,
    user_locks: Arc<DashMap<BareJid, Arc<Mutex<()>>>>,
}

impl DatabaseRosterStorage {
    /// Create a new database roster storage.
    pub fn new(db: Database) -> Self {
        Self {
            db,
            user_locks: Arc::new(DashMap::new()),
        }
    }

    pub(super) fn user_lock(&self, user_jid: &BareJid) -> Arc<Mutex<()>> {
        self.user_locks
            .entry(user_jid.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}
