//! Database-backed blocking list storage for XEP-0191 compliance.
//!
//! This module implements blocking list storage using libSQL/Turso for
//! persistent storage.

use jid::BareJid;
use libsql::params::IntoParams;
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

    /// Get all blocked JIDs for a user.
    #[instrument(skip(self), fields(user = %user_jid))]
    pub async fn get_blocklist(
        &self,
        user_jid: &BareJid,
    ) -> Result<Vec<String>, BlockingStorageError> {
        let mut rows = self
            .query_with_persistent(
                "SELECT blocked_jid FROM blocking_list WHERE user_jid = ? ORDER BY created_at",
                libsql::params![user_jid.to_string()],
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

    /// Check if a JID is blocked by a user.
    #[instrument(skip(self), fields(user = %user_jid, blocked = %blocked_jid))]
    pub async fn is_blocked(
        &self,
        user_jid: &BareJid,
        blocked_jid: &BareJid,
    ) -> Result<bool, BlockingStorageError> {
        let mut rows = self
            .query_with_persistent(
                "SELECT 1 FROM blocking_list WHERE user_jid = ? AND blocked_jid = ?",
                libsql::params![user_jid.to_string(), blocked_jid.to_string()],
            )
            .await?;

        let is_blocked = rows
            .next()
            .await
            .map_err(|e| BlockingStorageError::QueryFailed(format!("Failed to read row: {}", e)))?
            .is_some();

        debug!(is_blocked, "Checked if JID is blocked");
        Ok(is_blocked)
    }

    /// Add JIDs to the blocklist.
    ///
    /// Returns the number of JIDs that were newly blocked (ignores duplicates).
    #[instrument(skip(self, blocked_jids), fields(user = %user_jid, count = blocked_jids.len()))]
    pub async fn add_blocks(
        &self,
        user_jid: &BareJid,
        blocked_jids: &[String],
    ) -> Result<usize, BlockingStorageError> {
        let mut added = 0;
        for blocked_jid in blocked_jids {
            // Use INSERT OR IGNORE to handle duplicates gracefully
            let result = self
                .execute_with_persistent(
                    "INSERT OR IGNORE INTO blocking_list (user_jid, blocked_jid) VALUES (?, ?)",
                    libsql::params![user_jid.to_string(), blocked_jid.clone()],
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
    #[instrument(skip(self, blocked_jids), fields(user = %user_jid, count = blocked_jids.len()))]
    pub async fn remove_blocks(
        &self,
        user_jid: &BareJid,
        blocked_jids: &[String],
    ) -> Result<usize, BlockingStorageError> {
        let mut removed = 0;
        for blocked_jid in blocked_jids {
            let result = self
                .execute_with_persistent(
                    "DELETE FROM blocking_list WHERE user_jid = ? AND blocked_jid = ?",
                    libsql::params![user_jid.to_string(), blocked_jid.clone()],
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
    #[instrument(skip(self), fields(user = %user_jid))]
    pub async fn remove_all_blocks(
        &self,
        user_jid: &BareJid,
    ) -> Result<usize, BlockingStorageError> {
        let result = self
            .execute_with_persistent(
                "DELETE FROM blocking_list WHERE user_jid = ?",
                libsql::params![user_jid.to_string()],
            )
            .await?;

        debug!(removed = result, "Removed all JIDs from blocklist");
        Ok(result as usize)
    }

    /// Execute a query using the persistent connection for in-memory databases.
    /// This ensures data written by migrations is visible to queries.
    async fn query_with_persistent<'a>(
        &self,
        sql: &'a str,
        params: impl IntoParams,
    ) -> Result<libsql::Rows, BlockingStorageError> {
        if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            conn.query(sql, params)
                .await
                .map_err(|e| BlockingStorageError::QueryFailed(e.to_string()))
        } else {
            let conn = self
                .db
                .connect()
                .map_err(|e| BlockingStorageError::ConnectionFailed(e.to_string()))?;
            conn.query(sql, params)
                .await
                .map_err(|e| BlockingStorageError::QueryFailed(e.to_string()))
        }
    }

    /// Execute a statement using the persistent connection for in-memory databases.
    async fn execute_with_persistent(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<u64, BlockingStorageError> {
        if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            conn.execute(sql, params)
                .await
                .map_err(|e| BlockingStorageError::QueryFailed(e.to_string()))
        } else {
            let conn = self
                .db
                .connect()
                .map_err(|e| BlockingStorageError::ConnectionFailed(e.to_string()))?;
            conn.execute(sql, params)
                .await
                .map_err(|e| BlockingStorageError::QueryFailed(e.to_string()))
        }
    }
}

/// Errors that can occur during blocking storage operations.
#[derive(Debug, thiserror::Error)]
pub enum BlockingStorageError {
    #[error("Failed to connect to database: {0}")]
    ConnectionFailed(String),

    #[error("Query failed: {0}")]
    QueryFailed(String),
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
        let blocked_jid1 = "bob@example.com".to_string();
        let blocked_jid2 = "carol@example.com".to_string();

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
            .remove_blocks(&user_jid, &[blocked_jid1.clone()])
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
        let blocked_jid = "bob@example.com".to_string();

        // Add block
        let added = storage
            .add_blocks(&user_jid, &[blocked_jid.clone()])
            .await
            .unwrap();
        assert_eq!(added, 1);

        // Add same block again - should be ignored
        let added = storage
            .add_blocks(&user_jid, &[blocked_jid.clone()])
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
        let blocked_jid = "bob@example.com".to_string();

        // Remove nonexistent block - should succeed with 0 removed
        let removed = storage
            .remove_blocks(&user_jid, &[blocked_jid])
            .await
            .unwrap();
        assert_eq!(removed, 0);
    }

    #[tokio::test]
    async fn test_blocklist_per_user_isolation() {
        let db = setup_test_db().await;
        let storage = DatabaseBlockingStorage::new(db);

        let alice_jid: BareJid = "alice@example.com".parse().unwrap();
        let bob_jid: BareJid = "bob@example.com".parse().unwrap();
        let blocked_jid = "eve@example.com".to_string();

        // Alice blocks Eve
        storage
            .add_blocks(&alice_jid, &[blocked_jid.clone()])
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
