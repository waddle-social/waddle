//! Database-backed blocking list storage for XEP-0191 compliance.
//!
//! This module implements blocking list storage using the internal SQLx-backed database adapter for
//! persistent storage.

use crate::db::IntoParams;
use jid::{BareJid, Jid};
use tracing::{debug, instrument};

use super::Database;

/// Database-backed blocking list storage implementation.
///
/// Stores blocked JIDs in the `blocking_list` table.
#[derive(Clone)]
pub struct DatabaseBlockingStorage {
    db: Database,
}

impl DatabaseBlockingStorage {
    /// Create a new database blocking storage.
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Get all blocked JIDs for a user as typed XEP-0191 entries.
    #[instrument(skip(self, user_jid), fields(user = %user_jid))]
    pub async fn get_blocklist(
        &self,
        user_jid: &BareJid,
    ) -> Result<Vec<Jid>, BlockingStorageError> {
        self.load_blocklist_rows(user_jid)
            .await
            .map(|rows| parse_blocklist_entries(user_jid, rows))
    }

    async fn load_blocklist_rows(
        &self,
        user_jid: &BareJid,
    ) -> Result<Vec<String>, BlockingStorageError> {
        let mut rows = self
            .query_with_persistent(
                "SELECT blocked_jid FROM blocking_list WHERE user_jid = ? ORDER BY created_at",
                crate::db_params![user_jid.to_string()],
            )
            .await?;

        let mut blocked_jids = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| BlockingStorageError::QueryFailed(format!("Failed to read row: {}", e)))?
        {
            let blocked_jid: String = row.get(0).map_err(|e| {
                BlockingStorageError::QueryFailed(format!("Failed to get blocked_jid: {}", e))
            })?;
            blocked_jids.push(blocked_jid);
        }

        debug!(count = blocked_jids.len(), "Retrieved blocklist");
        Ok(blocked_jids)
    }

    /// Get all blocked JIDs for a user as typed [`BareJid`]s.
    ///
    /// Same query path as [`Self::get_blocklist`] but narrows each row's
    /// `blocked_jid` text column into a typed [`BareJid`] before
    /// returning. Rows that fail to parse are skipped and logged at
    /// WARN — the legacy `is_blocked` per-message check tolerates the
    /// same shape, so the SM-snapshot loader (#229 PR13) does too
    /// rather than fail closed and silently disable the entire
    /// blocklist if a single row is malformed.
    ///
    /// Used at bind time by
    /// [`crate::server::routes::websocket::WsConnState::ensure_state_machine`]
    /// to seed the per-connection [`waddle_xmpp::protocol::Blocklist`]
    /// snapshot consumed by the sans-I/O message pipeline. The
    /// snapshot is frozen for the duration of the session per #229 Q5
    /// — XEP-0191 IQ-set mutations during the session are reflected
    /// in this method's results on the *next* bind.
    #[instrument(skip(self, user_jid), fields(user = %user_jid))]
    pub async fn list_blocked_jids(
        &self,
        user_jid: &BareJid,
    ) -> Result<Vec<BareJid>, BlockingStorageError> {
        let raw = self.get_blocklist(user_jid).await?;
        let mut entries = Vec::with_capacity(raw.len());
        for blocked in raw {
            match blocked.clone().try_into() {
                Ok(jid) => entries.push(jid),
                Err(_) => {
                    tracing::warn!(
                        user = %user_jid,
                        blocked = %blocked,
                        "Skipping non-bare blocklist row for bare-JID snapshot"
                    );
                }
            }
        }
        debug!(count = entries.len(), "Loaded typed blocklist");
        Ok(entries)
    }

    /// Get all blocked JIDs for a user as their stored XEP-0191 JID form.
    ///
    /// This preserves full-JID and domain-JID entries for policy checks that
    /// must honor the complete XEP-0191 matching surface. Malformed legacy rows
    /// are skipped with the same warning policy as [`Self::list_blocked_jids`].
    #[instrument(skip(self, user_jid), fields(user = %user_jid))]
    pub async fn list_blocked_jid_entries(
        &self,
        user_jid: &BareJid,
    ) -> Result<Vec<Jid>, BlockingStorageError> {
        self.get_blocklist(user_jid).await
    }

    /// Check if a JID is blocked by a user.
    #[instrument(skip(self, user_jid, blocked_jid), fields(user = %user_jid, blocked = %blocked_jid))]
    pub async fn is_blocked(
        &self,
        user_jid: &BareJid,
        blocked_jid: &BareJid,
    ) -> Result<bool, BlockingStorageError> {
        self.is_blocked_jid(user_jid, &Jid::from(blocked_jid.clone()))
            .await
    }

    /// Check if a typed JID is blocked by a user using XEP-0191 matching.
    #[instrument(skip(self, user_jid, blocked_jid), fields(user = %user_jid, blocked = %blocked_jid))]
    pub async fn is_blocked_jid(
        &self,
        user_jid: &BareJid,
        blocked_jid: &Jid,
    ) -> Result<bool, BlockingStorageError> {
        let blocklist =
            waddle_xmpp::protocol::Blocklist::new(self.list_blocked_jid_entries(user_jid).await?);
        let is_blocked = blocklist.contains_jid(blocked_jid);
        debug!(is_blocked, "Checked if JID is blocked");
        Ok(is_blocked)
    }

    /// Add JIDs to the blocklist.
    ///
    /// Returns the number of JIDs that were newly blocked (ignores duplicates).
    #[instrument(skip(self, blocked_jids, user_jid), fields(user = %user_jid, count = blocked_jids.len()))]
    pub async fn add_blocks(
        &self,
        user_jid: &BareJid,
        blocked_jids: &[Jid],
    ) -> Result<usize, BlockingStorageError> {
        let mut added = 0;
        for blocked_jid in blocked_jids {
            // Use INSERT OR IGNORE to handle duplicates gracefully
            let result = self
                .execute_with_persistent(
                    "INSERT OR IGNORE INTO blocking_list (user_jid, blocked_jid) VALUES (?, ?)",
                    crate::db_params![user_jid.to_string(), blocked_jid.to_string()],
                )
                .await?;

            if result > 0 {
                added += 1;
            }
        }

        debug!(added, "Added JIDs to blocklist");
        Ok(added)
    }

    /// Remove JIDs from the blocklist.
    ///
    /// Returns the number of JIDs that were removed.
    #[instrument(skip(self, blocked_jids, user_jid), fields(user = %user_jid, count = blocked_jids.len()))]
    pub async fn remove_blocks(
        &self,
        user_jid: &BareJid,
        blocked_jids: &[Jid],
    ) -> Result<usize, BlockingStorageError> {
        let mut removed = 0;
        for blocked_jid in blocked_jids {
            let result = self
                .execute_with_persistent(
                    "DELETE FROM blocking_list WHERE user_jid = ? AND blocked_jid = ?",
                    crate::db_params![user_jid.to_string(), blocked_jid.to_string()],
                )
                .await?;

            if result > 0 {
                removed += 1;
            }
        }

        debug!(removed, "Removed JIDs from blocklist");
        Ok(removed)
    }

    /// Remove all JIDs from the blocklist.
    ///
    /// Returns the number of JIDs that were removed.
    #[instrument(skip(self, user_jid), fields(user = %user_jid))]
    pub async fn remove_all_blocks(
        &self,
        user_jid: &BareJid,
    ) -> Result<usize, BlockingStorageError> {
        let result = self
            .execute_with_persistent(
                "DELETE FROM blocking_list WHERE user_jid = ?",
                crate::db_params![user_jid.to_string()],
            )
            .await?;

        debug!(removed = result, "Removed all JIDs from blocklist");
        Ok(result as usize)
    }

    /// Execute a query using a connection guard.
    async fn query_with_persistent(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<crate::db::Rows, BlockingStorageError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|e| BlockingStorageError::ConnectionFailed(e.to_string()))?;
        conn.query(sql, params)
            .await
            .map_err(|e| BlockingStorageError::QueryFailed(e.to_string()))
    }

    /// Execute a statement using a connection guard.
    async fn execute_with_persistent(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<u64, BlockingStorageError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|e| BlockingStorageError::ConnectionFailed(e.to_string()))?;
        conn.execute(sql, params)
            .await
            .map_err(|e| BlockingStorageError::QueryFailed(e.to_string()))
    }
}

fn parse_blocklist_entries(user_jid: &BareJid, rows: Vec<String>) -> Vec<Jid> {
    let mut entries = Vec::with_capacity(rows.len());
    for blocked in rows {
        match blocked.parse::<Jid>() {
            Ok(jid) => entries.push(jid),
            Err(error) => {
                tracing::warn!(
                    user = %user_jid,
                    blocked = %blocked,
                    %error,
                    "Skipping malformed blocklist row"
                );
            }
        }
    }
    debug!(count = entries.len(), "Loaded typed blocklist entries");
    entries
}

/// Errors that can occur during blocking storage operations.
#[derive(Debug, thiserror::Error)]
pub enum BlockingStorageError {
    #[error("Failed to connect to database: {0}")]
    ConnectionFailed(String),

    #[error("Query failed: {0}")]
    QueryFailed(String),
}

/// Implements the protocol-side [`waddle_xmpp::xep::xep0191::BlockingStorage`]
/// trait so the sans-I/O message pipeline interpreter can load an
/// offline recipient's blocklist for the headless recipient pass
/// (#229 PR15) without taking a hard dependency on this concrete type.
#[async_trait::async_trait]
impl waddle_xmpp::xep::xep0191::BlockingStorage for DatabaseBlockingStorage {
    async fn list_blocked_jids(
        &self,
        user: &BareJid,
    ) -> Result<Vec<BareJid>, waddle_xmpp::xep::xep0191::BlockingStorageError> {
        Self::list_blocked_jids(self, user)
            .await
            .map_err(waddle_xmpp::xep::xep0191::BlockingStorageError::new)
    }

    async fn list_blocked_jid_entries(
        &self,
        user: &BareJid,
    ) -> Result<Vec<Jid>, waddle_xmpp::xep::xep0191::BlockingStorageError> {
        Self::list_blocked_jid_entries(self, user)
            .await
            .map_err(waddle_xmpp::xep::xep0191::BlockingStorageError::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup_test_db() -> Database {
        let db = Database::in_memory("test-blocking").await.unwrap();
        // Run migrations
        let runner = crate::db::MigrationRunner::global();
        runner.run(&db).await.unwrap();
        db
    }

    #[tokio::test]
    async fn test_blocklist_crud() {
        let db = setup_test_db().await;
        let storage = DatabaseBlockingStorage::new(db);

        let user_jid: BareJid = "alice@example.com".parse().unwrap();
        let blocked_jid1: Jid = "bob@example.com".parse().unwrap();
        let blocked_jid2: Jid = "carol@example.com".parse().unwrap();

        // Initially empty
        let blocklist = storage.get_blocklist(&user_jid).await.unwrap();
        assert!(blocklist.is_empty());

        // Add blocks
        let added = storage
            .add_blocks(&user_jid, &[blocked_jid1.clone(), blocked_jid2.clone()])
            .await
            .unwrap();
        assert_eq!(added, 2);

        // Check blocklist
        let blocklist = storage.get_blocklist(&user_jid).await.unwrap();
        assert_eq!(blocklist.len(), 2);
        assert!(blocklist.contains(&blocked_jid1));
        assert!(blocklist.contains(&blocked_jid2));

        // Check is_blocked
        let bob_jid: BareJid = "bob@example.com".parse().unwrap();
        assert!(storage.is_blocked(&user_jid, &bob_jid).await.unwrap());

        let dan_jid: BareJid = "dan@example.com".parse().unwrap();
        assert!(!storage.is_blocked(&user_jid, &dan_jid).await.unwrap());

        // Remove one block
        let removed = storage
            .remove_blocks(&user_jid, std::slice::from_ref(&blocked_jid1))
            .await
            .unwrap();
        assert_eq!(removed, 1);

        // Verify removal
        let blocklist = storage.get_blocklist(&user_jid).await.unwrap();
        assert_eq!(blocklist.len(), 1);
        assert!(!blocklist.contains(&blocked_jid1));
        assert!(blocklist.contains(&blocked_jid2));

        // Remove all
        let removed = storage.remove_all_blocks(&user_jid).await.unwrap();
        assert_eq!(removed, 1);

        // Verify all removed
        let blocklist = storage.get_blocklist(&user_jid).await.unwrap();
        assert!(blocklist.is_empty());
    }

    #[tokio::test]
    async fn test_add_duplicate_block() {
        let db = setup_test_db().await;
        let storage = DatabaseBlockingStorage::new(db);

        let user_jid: BareJid = "alice@example.com".parse().unwrap();
        let blocked_jid: Jid = "bob@example.com".parse().unwrap();

        // Add block
        let added = storage
            .add_blocks(&user_jid, std::slice::from_ref(&blocked_jid))
            .await
            .unwrap();
        assert_eq!(added, 1);

        // Add same block again - should be ignored
        let added = storage
            .add_blocks(&user_jid, std::slice::from_ref(&blocked_jid))
            .await
            .unwrap();
        assert_eq!(added, 0);

        // Should still only have one entry
        let blocklist = storage.get_blocklist(&user_jid).await.unwrap();
        assert_eq!(blocklist.len(), 1);
    }

    #[tokio::test]
    async fn test_remove_nonexistent_block() {
        let db = setup_test_db().await;
        let storage = DatabaseBlockingStorage::new(db);

        let user_jid: BareJid = "alice@example.com".parse().unwrap();
        let blocked_jid: Jid = "bob@example.com".parse().unwrap();

        // Remove nonexistent block - should succeed with 0 removed
        let removed = storage
            .remove_blocks(&user_jid, &[blocked_jid])
            .await
            .unwrap();
        assert_eq!(removed, 0);
    }

    #[tokio::test]
    async fn list_blocked_jids_returns_typed_bare_jids() {
        let db = setup_test_db().await;
        let storage = DatabaseBlockingStorage::new(db);

        let user_jid: BareJid = "alice@example.com".parse().unwrap();
        let blocked: Jid = "bob@example.com".parse().unwrap();
        storage
            .add_blocks(&user_jid, std::slice::from_ref(&blocked))
            .await
            .unwrap();

        let entries = storage.list_blocked_jids(&user_jid).await.unwrap();
        assert_eq!(entries.len(), 1);
        let bob: BareJid = "bob@example.com".parse().unwrap();
        assert_eq!(entries[0], bob);
    }

    #[tokio::test]
    async fn list_blocked_jid_entries_preserves_full_and_domain_jids() {
        let db = setup_test_db().await;
        let storage = DatabaseBlockingStorage::new(db);

        let user_jid: BareJid = "alice@example.com".parse().unwrap();
        let blocked = vec![
            "bob@example.com/phone".parse().unwrap(),
            "blocked.example.com".parse().unwrap(),
        ];
        storage.add_blocks(&user_jid, &blocked).await.unwrap();

        let entries = storage.list_blocked_jid_entries(&user_jid).await.unwrap();
        assert_eq!(entries, blocked);
    }

    #[tokio::test]
    async fn is_blocked_jid_matches_full_bare_and_domain_entries() {
        let db = setup_test_db().await;
        let storage = DatabaseBlockingStorage::new(db);

        let user_jid: BareJid = "alice@example.com".parse().unwrap();
        let blocked = vec![
            "bob@example.com/phone".parse().unwrap(),
            "carol@example.com".parse().unwrap(),
            "blocked.example.com".parse().unwrap(),
        ];
        storage.add_blocks(&user_jid, &blocked).await.unwrap();

        assert!(storage
            .is_blocked_jid(&user_jid, &"bob@example.com/phone".parse().unwrap())
            .await
            .unwrap());
        assert!(!storage
            .is_blocked_jid(&user_jid, &"bob@example.com/laptop".parse().unwrap())
            .await
            .unwrap());
        assert!(storage
            .is_blocked_jid(&user_jid, &"carol@example.com/tablet".parse().unwrap())
            .await
            .unwrap());
        assert!(storage
            .is_blocked_jid(&user_jid, &"dave@blocked.example.com".parse().unwrap())
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_blocklist_per_user_isolation() {
        let db = setup_test_db().await;
        let storage = DatabaseBlockingStorage::new(db);

        let alice_jid: BareJid = "alice@example.com".parse().unwrap();
        let bob_jid: BareJid = "bob@example.com".parse().unwrap();
        let blocked_jid: Jid = "eve@example.com".parse().unwrap();

        // Alice blocks Eve
        storage
            .add_blocks(&alice_jid, std::slice::from_ref(&blocked_jid))
            .await
            .unwrap();

        // Alice's blocklist should have Eve
        let alice_blocklist = storage.get_blocklist(&alice_jid).await.unwrap();
        assert_eq!(alice_blocklist.len(), 1);

        // Bob's blocklist should be empty
        let bob_blocklist = storage.get_blocklist(&bob_jid).await.unwrap();
        assert!(bob_blocklist.is_empty());

        // Eve should be blocked for Alice but not Bob
        let eve_jid: BareJid = "eve@example.com".parse().unwrap();
        assert!(storage.is_blocked(&alice_jid, &eve_jid).await.unwrap());
        assert!(!storage.is_blocked(&bob_jid, &eve_jid).await.unwrap());
    }
}
