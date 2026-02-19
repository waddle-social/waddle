//! vCard storage for XEP-0054 vcard-temp.
//!
//! This module provides storage and retrieval for vCards, allowing users to
//! store profile information (name, photo, etc.) per XEP-0054.
//!
//! ## Storage Model
//!
//! vCards are stored as XML blobs keyed by bare JID. This allows for full
//! preservation of all vCard fields without needing to parse/reconstruct
//! the XML on every request.

use std::sync::Arc;

use tracing::debug;

use crate::db::Database;

/// Error type for vCard operations.
#[derive(Debug, thiserror::Error)]
pub enum VCardError {
    #[error("Database error: {0}")]
    DatabaseError(String),
}

/// vCard store for XEP-0054 vcard-temp.
#[derive(Clone)]
pub struct VCardStore {
    /// Database connection
    db: Arc<Database>,
}

impl VCardStore {
    /// Create a new vCard store.
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Get a database connection.
    ///
    /// For in-memory databases, this returns a guard to the persistent connection
    /// to ensure data consistency (libSQL creates isolated databases for each `:memory:` connection).
    /// For file-based databases, we create new connections.
    async fn get_connection(&self) -> Result<ConnectionGuard<'_>, VCardError> {
        if let Some(persistent) = self.db.persistent_connection() {
            let guard = persistent.lock().await;
            Ok(ConnectionGuard::Persistent(guard))
        } else {
            let conn = self
                .db
                .connect()
                .map_err(|e| VCardError::DatabaseError(e.to_string()))?;
            Ok(ConnectionGuard::Owned(conn))
        }
    }

    /// Get the vCard for a user.
    ///
    /// Returns the vCard XML if found, None otherwise.
    pub async fn get(&self, jid: &jid::BareJid) -> Result<Option<String>, VCardError> {
        let jid_str = jid.to_string();
        debug!(jid = %jid_str, "Getting vCard from storage");

        let conn = self.get_connection().await?;

        let mut rows = conn
            .as_ref()
            .query(
                "SELECT vcard_xml FROM vcard_storage WHERE jid = ?",
                [jid_str.as_str()],
            )
            .await
            .map_err(db_err)?;

        match rows.next().await.map_err(db_err)? {
            Some(row) => {
                let vcard_xml: String = row.get(0).map_err(db_err)?;
                debug!(jid = %jid_str, "Found vCard");
                Ok(Some(vcard_xml))
            }
            None => {
                debug!(jid = %jid_str, "No vCard found");
                Ok(None)
            }
        }
    }

    /// Store or update the vCard for a user (marks source as 'manual').
    ///
    /// This uses an UPSERT to handle both new vCards and updates.
    /// Always sets source = 'manual' since this is a user-initiated write.
    pub async fn set(&self, jid: &jid::BareJid, vcard_xml: &str) -> Result<(), VCardError> {
        let jid_str = jid.to_string();
        debug!(jid = %jid_str, "Storing vCard (manual)");

        let conn = self.get_connection().await?;

        conn.as_ref()
            .execute(
                r#"
                INSERT INTO vcard_storage (jid, vcard_xml, source, created_at, updated_at)
                VALUES (?, ?, 'manual', datetime('now'), datetime('now'))
                ON CONFLICT(jid) DO UPDATE SET
                    vcard_xml = excluded.vcard_xml,
                    source = 'manual',
                    updated_at = datetime('now')
                "#,
                (jid_str.as_str(), vcard_xml),
            )
            .await
            .map_err(db_err)?;

        debug!(jid = %jid_str, "vCard stored successfully (manual)");
        Ok(())
    }

    /// Store/update vCard only if source is 'atproto_auto' or no vCard exists.
    ///
    /// Used for ATProto auto-population to avoid overwriting manual user edits (FR-1.3).
    /// Returns `Ok(true)` if the vCard was written, `Ok(false)` if skipped (manual edit exists).
    pub async fn set_if_auto(
        &self,
        jid: &jid::BareJid,
        vcard_xml: &str,
    ) -> Result<bool, VCardError> {
        let jid_str = jid.to_string();
        debug!(jid = %jid_str, "Storing vCard (atproto_auto, guarded)");

        let conn = self.get_connection().await?;

        // Check current source — skip if user has manually edited
        let mut rows = conn
            .as_ref()
            .query(
                "SELECT source FROM vcard_storage WHERE jid = ?",
                [jid_str.as_str()],
            )
            .await
            .map_err(db_err)?;

        if let Some(row) = rows.next().await.map_err(db_err)? {
            let source: String = row.get(0).map_err(db_err)?;
            if source == "manual" {
                debug!(jid = %jid_str, "Skipping auto-populate — user has manual vCard");
                return Ok(false);
            }
        }
        // Drop rows before executing write
        drop(rows);

        conn.as_ref()
            .execute(
                r#"
                INSERT INTO vcard_storage (jid, vcard_xml, source, created_at, updated_at)
                VALUES (?, ?, 'atproto_auto', datetime('now'), datetime('now'))
                ON CONFLICT(jid) DO UPDATE SET
                    vcard_xml = excluded.vcard_xml,
                    source = 'atproto_auto',
                    updated_at = datetime('now')
                "#,
                (jid_str.as_str(), vcard_xml),
            )
            .await
            .map_err(db_err)?;

        debug!(jid = %jid_str, "vCard auto-populated from ATProto");
        Ok(true)
    }

    /// Get the source of a vCard ('manual' or 'atproto_auto').
    pub async fn get_source(&self, jid: &jid::BareJid) -> Result<Option<String>, VCardError> {
        let jid_str = jid.to_string();
        let conn = self.get_connection().await?;

        let mut rows = conn
            .as_ref()
            .query(
                "SELECT source FROM vcard_storage WHERE jid = ?",
                [jid_str.as_str()],
            )
            .await
            .map_err(db_err)?;

        match rows.next().await.map_err(db_err)? {
            Some(row) => {
                let source: String = row.get(0).map_err(db_err)?;
                Ok(Some(source))
            }
            None => Ok(None),
        }
    }

    /// Delete the vCard for a user.
    ///
    /// Returns true if a vCard was deleted, false if no vCard existed.
    #[allow(dead_code)]
    pub async fn delete(&self, jid: &jid::BareJid) -> Result<bool, VCardError> {
        let jid_str = jid.to_string();
        debug!(jid = %jid_str, "Deleting vCard");

        let conn = self.get_connection().await?;

        let affected = conn
            .as_ref()
            .execute(
                "DELETE FROM vcard_storage WHERE jid = ?",
                [jid_str.as_str()],
            )
            .await
            .map_err(db_err)?;

        if affected > 0 {
            debug!(jid = %jid_str, "vCard deleted");
            Ok(true)
        } else {
            debug!(jid = %jid_str, "No vCard to delete");
            Ok(false)
        }
    }
}

/// A guard that wraps either a persistent connection (for in-memory databases)
/// or an owned connection (for file-based databases).
///
/// This ensures that in-memory databases always use the persistent connection
/// to maintain data across operations.
enum ConnectionGuard<'a> {
    /// Persistent connection guard for in-memory databases
    Persistent(tokio::sync::MutexGuard<'a, libsql::Connection>),
    /// Owned connection for file-based databases
    Owned(libsql::Connection),
}

impl<'a> ConnectionGuard<'a> {
    /// Get a reference to the underlying connection
    fn as_ref(&self) -> &libsql::Connection {
        match self {
            ConnectionGuard::Persistent(guard) => guard,
            ConnectionGuard::Owned(conn) => conn,
        }
    }
}

/// Helper to convert libsql errors to VCardError.
fn db_err<E: std::fmt::Display>(e: E) -> VCardError {
    VCardError::DatabaseError(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::MigrationRunner;

    async fn create_test_db() -> Arc<Database> {
        let db = Database::in_memory("test-vcard")
            .await
            .expect("Failed to create test database");
        let db = Arc::new(db);

        // Run migrations
        let runner = MigrationRunner::global();
        runner.run(&db).await.expect("Failed to run migrations");

        db
    }

    #[tokio::test]
    async fn test_vcard_store_set_and_get() {
        let db = create_test_db().await;
        let store = VCardStore::new(db);

        let jid: jid::BareJid = "alice@example.com".parse().unwrap();
        let vcard_xml = "<vCard xmlns='vcard-temp'><FN>Alice</FN></vCard>";

        // Store vCard
        store
            .set(&jid, vcard_xml)
            .await
            .expect("Failed to store vCard");

        // Retrieve vCard
        let retrieved = store.get(&jid).await.expect("Failed to get vCard");
        assert_eq!(retrieved, Some(vcard_xml.to_string()));
    }

    #[tokio::test]
    async fn test_vcard_store_get_nonexistent() {
        let db = create_test_db().await;
        let store = VCardStore::new(db);

        let jid: jid::BareJid = "nonexistent@example.com".parse().unwrap();

        let retrieved = store.get(&jid).await.expect("Failed to get vCard");
        assert_eq!(retrieved, None);
    }

    #[tokio::test]
    async fn test_vcard_store_update() {
        let db = create_test_db().await;
        let store = VCardStore::new(db);

        let jid: jid::BareJid = "bob@example.com".parse().unwrap();
        let vcard_xml_v1 = "<vCard xmlns='vcard-temp'><FN>Bob</FN></vCard>";
        let vcard_xml_v2 = "<vCard xmlns='vcard-temp'><FN>Robert</FN></vCard>";

        // Store initial vCard
        store
            .set(&jid, vcard_xml_v1)
            .await
            .expect("Failed to store vCard");

        // Update vCard
        store
            .set(&jid, vcard_xml_v2)
            .await
            .expect("Failed to update vCard");

        // Retrieve should return updated version
        let retrieved = store.get(&jid).await.expect("Failed to get vCard");
        assert_eq!(retrieved, Some(vcard_xml_v2.to_string()));
    }

    #[tokio::test]
    async fn test_vcard_store_delete() {
        let db = create_test_db().await;
        let store = VCardStore::new(db);

        let jid: jid::BareJid = "charlie@example.com".parse().unwrap();
        let vcard_xml = "<vCard xmlns='vcard-temp'><FN>Charlie</FN></vCard>";

        // Store vCard
        store
            .set(&jid, vcard_xml)
            .await
            .expect("Failed to store vCard");

        // Delete vCard
        let deleted = store.delete(&jid).await.expect("Failed to delete vCard");
        assert!(deleted);

        // Retrieve should return None
        let retrieved = store.get(&jid).await.expect("Failed to get vCard");
        assert_eq!(retrieved, None);

        // Delete again should return false
        let deleted = store.delete(&jid).await.expect("Failed to delete vCard");
        assert!(!deleted);
    }

    #[tokio::test]
    async fn test_vcard_store_multiple_users() {
        let db = create_test_db().await;
        let store = VCardStore::new(db);

        let jid1: jid::BareJid = "user1@example.com".parse().unwrap();
        let jid2: jid::BareJid = "user2@example.com".parse().unwrap();
        let vcard1 = "<vCard xmlns='vcard-temp'><FN>User One</FN></vCard>";
        let vcard2 = "<vCard xmlns='vcard-temp'><FN>User Two</FN></vCard>";

        // Store vCards for different users
        store
            .set(&jid1, vcard1)
            .await
            .expect("Failed to store vCard 1");
        store
            .set(&jid2, vcard2)
            .await
            .expect("Failed to store vCard 2");

        // Each user should have their own vCard
        let retrieved1 = store.get(&jid1).await.expect("Failed to get vCard 1");
        let retrieved2 = store.get(&jid2).await.expect("Failed to get vCard 2");

        assert_eq!(retrieved1, Some(vcard1.to_string()));
        assert_eq!(retrieved2, Some(vcard2.to_string()));
    }

    #[tokio::test]
    async fn test_set_if_auto_new_vcard() {
        let db = create_test_db().await;
        let store = VCardStore::new(db);

        let jid: jid::BareJid = "auto@example.com".parse().unwrap();
        let vcard_xml = "<vCard xmlns='vcard-temp'><FN>Auto User</FN></vCard>";

        let written = store
            .set_if_auto(&jid, vcard_xml)
            .await
            .expect("Failed to set_if_auto");
        assert!(written);

        let retrieved = store.get(&jid).await.expect("Failed to get vCard");
        assert_eq!(retrieved, Some(vcard_xml.to_string()));

        let source = store.get_source(&jid).await.expect("Failed to get source");
        assert_eq!(source, Some("atproto_auto".to_string()));
    }

    #[tokio::test]
    async fn test_set_if_auto_existing_auto() {
        let db = create_test_db().await;
        let store = VCardStore::new(db);

        let jid: jid::BareJid = "auto@example.com".parse().unwrap();
        let vcard_v1 = "<vCard xmlns='vcard-temp'><FN>V1</FN></vCard>";
        let vcard_v2 = "<vCard xmlns='vcard-temp'><FN>V2</FN></vCard>";

        store.set_if_auto(&jid, vcard_v1).await.unwrap();
        let written = store.set_if_auto(&jid, vcard_v2).await.unwrap();
        assert!(written);

        let retrieved = store.get(&jid).await.unwrap();
        assert_eq!(retrieved, Some(vcard_v2.to_string()));
    }

    #[tokio::test]
    async fn test_set_if_auto_existing_manual() {
        let db = create_test_db().await;
        let store = VCardStore::new(db);

        let jid: jid::BareJid = "manual@example.com".parse().unwrap();
        let manual_vcard = "<vCard xmlns='vcard-temp'><FN>Manual</FN></vCard>";
        let auto_vcard = "<vCard xmlns='vcard-temp'><FN>Auto</FN></vCard>";

        // User sets their vCard manually
        store.set(&jid, manual_vcard).await.unwrap();

        // ATProto auto-populate should NOT overwrite
        let written = store.set_if_auto(&jid, auto_vcard).await.unwrap();
        assert!(!written);

        // Original manual vCard should be preserved
        let retrieved = store.get(&jid).await.unwrap();
        assert_eq!(retrieved, Some(manual_vcard.to_string()));
        assert_eq!(
            store.get_source(&jid).await.unwrap(),
            Some("manual".to_string())
        );
    }

    #[tokio::test]
    async fn test_set_marks_manual() {
        let db = create_test_db().await;
        let store = VCardStore::new(db);

        let jid: jid::BareJid = "user@example.com".parse().unwrap();
        let vcard = "<vCard xmlns='vcard-temp'><FN>User</FN></vCard>";

        store.set(&jid, vcard).await.unwrap();
        let source = store.get_source(&jid).await.unwrap();
        assert_eq!(source, Some("manual".to_string()));
    }

    #[tokio::test]
    async fn test_set_overrides_auto_source() {
        let db = create_test_db().await;
        let store = VCardStore::new(db);

        let jid: jid::BareJid = "user@example.com".parse().unwrap();
        let auto_vcard = "<vCard xmlns='vcard-temp'><FN>Auto</FN></vCard>";
        let manual_vcard = "<vCard xmlns='vcard-temp'><FN>Manual</FN></vCard>";

        // First auto-populate
        store.set_if_auto(&jid, auto_vcard).await.unwrap();
        assert_eq!(
            store.get_source(&jid).await.unwrap(),
            Some("atproto_auto".to_string())
        );

        // Then user manually sets — should override to manual
        store.set(&jid, manual_vcard).await.unwrap();
        assert_eq!(
            store.get_source(&jid).await.unwrap(),
            Some("manual".to_string())
        );

        // Future auto-populate should be blocked
        let written = store.set_if_auto(&jid, auto_vcard).await.unwrap();
        assert!(!written);
        assert_eq!(
            store.get(&jid).await.unwrap(),
            Some(manual_vcard.to_string())
        );
    }
}
