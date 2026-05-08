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

use chrono::Utc;
use tracing::debug;
use xmpp_parsers::minidom::Element;

use crate::db::{Database, Value};

/// Error type for vCard operations.
#[derive(Debug, thiserror::Error)]
pub enum VCardError {
    #[error("Database error: {0}")]
    DatabaseError(String),
    #[error("Stored vCard XML is invalid: {0}")]
    InvalidXml(String),
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
    /// For in-memory databases, this returns a guard from the shared pool so the
    /// store reads and writes through the same logical database handle.
    async fn get_connection(&self) -> Result<crate::db::ConnectionGuard, VCardError> {
        self.db
            .guard()
            .await
            .map_err(|e| VCardError::DatabaseError(e.to_string()))
    }

    /// Get the vCard for a user.
    ///
    /// Returns the vCard element if found, None otherwise.
    pub async fn get(&self, jid: &jid::BareJid) -> Result<Option<Element>, VCardError> {
        let jid_str = jid.to_string();
        debug!(jid = %jid_str, "Getting vCard from storage");

        let conn = self.get_connection().await?;

        let mut rows = conn
            .query(
                "SELECT vcard_xml FROM vcard_storage WHERE jid = ?",
                [jid_str.as_str()],
            )
            .await
            .map_err(db_err)?;

        match rows.next().await.map_err(db_err)? {
            Some(row) => {
                let vcard_xml: String = row.get(0).map_err(db_err)?;
                let vcard = vcard_xml
                    .parse::<Element>()
                    .map_err(|error| VCardError::InvalidXml(error.to_string()))?;
                debug!(jid = %jid_str, "Found vCard");
                Ok(Some(vcard))
            }
            None => {
                debug!(jid = %jid_str, "No vCard found");
                Ok(None)
            }
        }
    }

    /// Store or update the vCard for a user.
    ///
    /// This uses an UPSERT to handle both new vCards and updates.
    pub async fn set(&self, jid: &jid::BareJid, vcard: &Element) -> Result<(), VCardError> {
        let jid_str = jid.to_string();
        debug!(jid = %jid_str, "Storing vCard");

        let conn = self.get_connection().await?;
        let now = Utc::now().to_rfc3339();
        let vcard_xml = String::from(vcard);

        conn.execute(
            upsert_vcard_sql(),
            vec![
                Value::from(jid_str.as_str()),
                Value::from(vcard_xml),
                Value::from(now.as_str()),
                Value::from(now.as_str()),
            ],
        )
        .await
        .map_err(db_err)?;

        debug!(jid = %jid_str, "vCard stored successfully");
        Ok(())
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

fn upsert_vcard_sql() -> &'static str {
    r#"
        INSERT INTO vcard_storage (jid, vcard_xml, created_at, updated_at)
        VALUES (?, ?, ?, ?)
        ON CONFLICT(jid) DO UPDATE SET
            vcard_xml = excluded.vcard_xml,
            updated_at = excluded.updated_at
        "#
}

/// Helper to convert database errors to VCardError.
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

    fn vcard_element(xml: &str) -> Element {
        xml.parse::<Element>().expect("valid vCard XML")
    }

    #[tokio::test]
    async fn test_vcard_store_set_and_get() {
        let db = create_test_db().await;
        let store = VCardStore::new(db);

        let jid: jid::BareJid = "alice@example.com".parse().unwrap();
        let vcard = vcard_element("<vCard xmlns='vcard-temp'><FN>Alice</FN></vCard>");

        // Store vCard
        store
            .set(&jid, &vcard)
            .await
            .expect("Failed to store vCard");

        // Retrieve vCard
        let retrieved = store.get(&jid).await.expect("Failed to get vCard");
        assert_eq!(retrieved, Some(vcard));
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
        let vcard_v1 = vcard_element("<vCard xmlns='vcard-temp'><FN>Bob</FN></vCard>");
        let vcard_v2 = vcard_element("<vCard xmlns='vcard-temp'><FN>Robert</FN></vCard>");

        // Store initial vCard
        store
            .set(&jid, &vcard_v1)
            .await
            .expect("Failed to store vCard");

        // Update vCard
        store
            .set(&jid, &vcard_v2)
            .await
            .expect("Failed to update vCard");

        // Retrieve should return updated version
        let retrieved = store.get(&jid).await.expect("Failed to get vCard");
        assert_eq!(retrieved, Some(vcard_v2));
    }

    #[test]
    fn test_vcard_store_set_sql_uses_bound_timestamps() {
        let sql = upsert_vcard_sql();
        assert!(!sql.contains("datetime("));
        assert!(sql.contains("VALUES (?, ?, ?, ?)"));
        assert!(sql.contains("updated_at = excluded.updated_at"));
    }

    #[tokio::test]
    async fn test_vcard_store_delete() {
        let db = create_test_db().await;
        let store = VCardStore::new(db);

        let jid: jid::BareJid = "charlie@example.com".parse().unwrap();
        let vcard = vcard_element("<vCard xmlns='vcard-temp'><FN>Charlie</FN></vCard>");

        // Store vCard
        store
            .set(&jid, &vcard)
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
        let vcard1 = vcard_element("<vCard xmlns='vcard-temp'><FN>User One</FN></vCard>");
        let vcard2 = vcard_element("<vCard xmlns='vcard-temp'><FN>User Two</FN></vCard>");

        // Store vCards for different users
        store
            .set(&jid1, &vcard1)
            .await
            .expect("Failed to store vCard 1");
        store
            .set(&jid2, &vcard2)
            .await
            .expect("Failed to store vCard 2");

        // Each user should have their own vCard
        let retrieved1 = store.get(&jid1).await.expect("Failed to get vCard 1");
        let retrieved2 = store.get(&jid2).await.expect("Failed to get vCard 2");

        assert_eq!(retrieved1, Some(vcard1));
        assert_eq!(retrieved2, Some(vcard2));
    }
}
