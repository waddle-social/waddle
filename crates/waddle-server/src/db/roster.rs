//! Database-backed roster storage for RFC 6121 compliance.
//!
//! This module implements the `RosterStorage` trait from `waddle-xmpp` using
//! libSQL/Turso for persistent storage.

use jid::BareJid;
use tracing::{debug, instrument};
use uuid::Uuid;

use super::Database;

/// Database-backed roster storage implementation.
///
/// Stores roster items in the `roster_items` table and manages roster
/// versioning via the `roster_versions` table.
#[derive(Clone)]
pub struct DatabaseRosterStorage {
    db: Database,
}

impl DatabaseRosterStorage {
    /// Create a new database roster storage.
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Get all roster items for a user.
    #[instrument(skip(self), fields(user = %user_jid))]
    pub async fn get_roster(
        &self,
        user_jid: &BareJid,
    ) -> Result<Vec<RosterItemRow>, RosterStorageError> {
        let mut rows = self.query_with_persistent(
            "SELECT contact_jid, name, subscription, ask, groups FROM roster_items WHERE user_jid = ?",
            libsql::params![user_jid.to_string()],
        ).await?;

        let mut items = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| {
            RosterStorageError::QueryFailed(format!("Failed to read row: {}", e))
        })? {
            let contact_jid: String = row.get(0).map_err(|e| {
                RosterStorageError::QueryFailed(format!("Failed to get contact_jid: {}", e))
            })?;
            let name: Option<String> = row.get(1).ok();
            let subscription: String = row.get(2).map_err(|e| {
                RosterStorageError::QueryFailed(format!("Failed to get subscription: {}", e))
            })?;
            let ask: Option<String> = row.get(3).ok();
            let groups_json: Option<String> = row.get(4).ok();

            let groups: Vec<String> = groups_json
                .and_then(|json| serde_json::from_str(&json).ok())
                .unwrap_or_default();

            items.push(RosterItemRow {
                contact_jid,
                name,
                subscription,
                ask,
                groups,
            });
        }

        debug!(count = items.len(), "Retrieved roster items");
        Ok(items)
    }

    /// Get a single roster item.
    #[instrument(skip(self), fields(user = %user_jid, contact = %contact_jid))]
    pub async fn get_roster_item(
        &self,
        user_jid: &BareJid,
        contact_jid: &BareJid,
    ) -> Result<Option<RosterItemRow>, RosterStorageError> {
        let conn = self.get_connection().await?;

        let mut rows = conn
            .query(
                "SELECT contact_jid, name, subscription, ask, groups FROM roster_items WHERE user_jid = ? AND contact_jid = ?",
                libsql::params![user_jid.to_string(), contact_jid.to_string()],
            )
            .await
            .map_err(|e| RosterStorageError::QueryFailed(e.to_string()))?;

        match rows.next().await.map_err(|e| {
            RosterStorageError::QueryFailed(format!("Failed to read row: {}", e))
        })? {
            Some(row) => {
                let contact_jid: String = row.get(0).map_err(|e| {
                    RosterStorageError::QueryFailed(format!("Failed to get contact_jid: {}", e))
                })?;
                let name: Option<String> = row.get(1).ok();
                let subscription: String = row.get(2).map_err(|e| {
                    RosterStorageError::QueryFailed(format!("Failed to get subscription: {}", e))
                })?;
                let ask: Option<String> = row.get(3).ok();
                let groups_json: Option<String> = row.get(4).ok();

                let groups: Vec<String> = groups_json
                    .and_then(|json| serde_json::from_str(&json).ok())
                    .unwrap_or_default();

                Ok(Some(RosterItemRow {
                    contact_jid,
                    name,
                    subscription,
                    ask,
                    groups,
                }))
            }
            None => Ok(None),
        }
    }

    /// Add or update a roster item.
    ///
    /// Returns `true` if a new item was created, `false` if an existing item was updated.
    #[instrument(skip(self, item), fields(user = %user_jid, contact = %item.contact_jid))]
    pub async fn set_roster_item(
        &self,
        user_jid: &BareJid,
        item: &RosterItemRow,
    ) -> Result<bool, RosterStorageError> {
        let conn = self.get_connection().await?;

        let groups_json = serde_json::to_string(&item.groups)
            .map_err(|e| RosterStorageError::SerializationError(e.to_string()))?;

        // Use INSERT OR REPLACE to upsert
        let result = conn
            .execute(
                r#"
                INSERT INTO roster_items (user_jid, contact_jid, name, subscription, ask, groups, updated_at)
                VALUES (?, ?, ?, ?, ?, ?, datetime('now'))
                ON CONFLICT(user_jid, contact_jid) DO UPDATE SET
                    name = excluded.name,
                    subscription = excluded.subscription,
                    ask = excluded.ask,
                    groups = excluded.groups,
                    updated_at = datetime('now')
                "#,
                libsql::params![
                    user_jid.to_string(),
                    item.contact_jid.clone(),
                    item.name.clone(),
                    item.subscription.clone(),
                    item.ask.clone(),
                    groups_json,
                ],
            )
            .await
            .map_err(|e| RosterStorageError::QueryFailed(e.to_string()))?;

        // Update roster version
        self.increment_roster_version(user_jid).await?;

        let is_new = result == 1;
        debug!(is_new, "Set roster item");
        Ok(is_new)
    }

    /// Remove a roster item.
    ///
    /// Returns `true` if an item was removed, `false` if it didn't exist.
    #[instrument(skip(self), fields(user = %user_jid, contact = %contact_jid))]
    pub async fn remove_roster_item(
        &self,
        user_jid: &BareJid,
        contact_jid: &BareJid,
    ) -> Result<bool, RosterStorageError> {
        let conn = self.get_connection().await?;

        let result = conn
            .execute(
                "DELETE FROM roster_items WHERE user_jid = ? AND contact_jid = ?",
                libsql::params![user_jid.to_string(), contact_jid.to_string()],
            )
            .await
            .map_err(|e| RosterStorageError::QueryFailed(e.to_string()))?;

        if result > 0 {
            // Update roster version
            self.increment_roster_version(user_jid).await?;
        }

        debug!(removed = result > 0, "Remove roster item");
        Ok(result > 0)
    }

    /// Get the current roster version for a user.
    #[instrument(skip(self), fields(user = %user_jid))]
    pub async fn get_roster_version(
        &self,
        user_jid: &BareJid,
    ) -> Result<Option<String>, RosterStorageError> {
        let conn = self.get_connection().await?;

        let mut rows = conn
            .query(
                "SELECT version FROM roster_versions WHERE user_jid = ?",
                libsql::params![user_jid.to_string()],
            )
            .await
            .map_err(|e| RosterStorageError::QueryFailed(e.to_string()))?;

        match rows.next().await.map_err(|e| {
            RosterStorageError::QueryFailed(format!("Failed to read row: {}", e))
        })? {
            Some(row) => {
                let version: String = row.get(0).map_err(|e| {
                    RosterStorageError::QueryFailed(format!("Failed to get version: {}", e))
                })?;
                Ok(Some(version))
            }
            None => Ok(None),
        }
    }

    /// Check if a roster item exists.
    #[instrument(skip(self), fields(user = %user_jid, contact = %contact_jid))]
    pub async fn has_roster_item(
        &self,
        user_jid: &BareJid,
        contact_jid: &BareJid,
    ) -> Result<bool, RosterStorageError> {
        let conn = self.get_connection().await?;

        let mut rows = conn
            .query(
                "SELECT 1 FROM roster_items WHERE user_jid = ? AND contact_jid = ?",
                libsql::params![user_jid.to_string(), contact_jid.to_string()],
            )
            .await
            .map_err(|e| RosterStorageError::QueryFailed(e.to_string()))?;

        let exists = rows.next().await.map_err(|e| {
            RosterStorageError::QueryFailed(format!("Failed to read row: {}", e))
        })?.is_some();

        Ok(exists)
    }

    /// Update the subscription state for a roster item.
    ///
    /// Creates the roster item if it doesn't exist.
    #[instrument(skip(self), fields(user = %user_jid, contact = %contact_jid))]
    pub async fn update_subscription(
        &self,
        user_jid: &BareJid,
        contact_jid: &BareJid,
        subscription: &str,
        ask: Option<&str>,
    ) -> Result<RosterItemRow, RosterStorageError> {
        let conn = self.get_connection().await?;

        // Upsert the roster item with the new subscription state
        conn.execute(
            r#"
            INSERT INTO roster_items (user_jid, contact_jid, subscription, ask, groups, updated_at)
            VALUES (?, ?, ?, ?, '[]', datetime('now'))
            ON CONFLICT(user_jid, contact_jid) DO UPDATE SET
                subscription = excluded.subscription,
                ask = excluded.ask,
                updated_at = datetime('now')
            "#,
            libsql::params![
                user_jid.to_string(),
                contact_jid.to_string(),
                subscription.to_string(),
                ask.map(|s| s.to_string()),
            ],
        )
        .await
        .map_err(|e| RosterStorageError::QueryFailed(e.to_string()))?;

        // Update roster version
        self.increment_roster_version(user_jid).await?;

        // Return the updated item
        self.get_roster_item(user_jid, contact_jid)
            .await?
            .ok_or_else(|| RosterStorageError::QueryFailed("Item not found after upsert".to_string()))
    }

    /// Get all roster items where the user should send presence updates.
    ///
    /// Returns contacts with subscription=from or subscription=both.
    #[instrument(skip(self), fields(user = %user_jid))]
    pub async fn get_presence_subscribers(
        &self,
        user_jid: &BareJid,
    ) -> Result<Vec<String>, RosterStorageError> {
        let conn = self.get_connection().await?;

        let mut rows = conn
            .query(
                "SELECT contact_jid FROM roster_items WHERE user_jid = ? AND subscription IN ('from', 'both')",
                libsql::params![user_jid.to_string()],
            )
            .await
            .map_err(|e| RosterStorageError::QueryFailed(e.to_string()))?;

        let mut jids = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| {
            RosterStorageError::QueryFailed(format!("Failed to read row: {}", e))
        })? {
            let jid: String = row.get(0).map_err(|e| {
                RosterStorageError::QueryFailed(format!("Failed to get jid: {}", e))
            })?;
            jids.push(jid);
        }

        debug!(count = jids.len(), "Retrieved presence subscribers");
        Ok(jids)
    }

    /// Get all roster items where the user receives presence updates.
    ///
    /// Returns contacts with subscription=to or subscription=both.
    #[instrument(skip(self), fields(user = %user_jid))]
    pub async fn get_presence_subscriptions(
        &self,
        user_jid: &BareJid,
    ) -> Result<Vec<String>, RosterStorageError> {
        let conn = self.get_connection().await?;

        let mut rows = conn
            .query(
                "SELECT contact_jid FROM roster_items WHERE user_jid = ? AND subscription IN ('to', 'both')",
                libsql::params![user_jid.to_string()],
            )
            .await
            .map_err(|e| RosterStorageError::QueryFailed(e.to_string()))?;

        let mut jids = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| {
            RosterStorageError::QueryFailed(format!("Failed to read row: {}", e))
        })? {
            let jid: String = row.get(0).map_err(|e| {
                RosterStorageError::QueryFailed(format!("Failed to get jid: {}", e))
            })?;
            jids.push(jid);
        }

        debug!(count = jids.len(), "Retrieved presence subscriptions");
        Ok(jids)
    }

    /// Get a database connection.
    ///
    /// For file-based databases, creates a new connection.
    /// For in-memory databases, we need to use the same connection to see the data.
    fn get_connection(&self) -> Result<libsql::Connection, RosterStorageError> {
        // Always use connect() which works for both in-memory and file-based DBs
        // For in-memory DBs with :memory:, the Database wrapper ensures all connections
        // share the same underlying database via the persistent_conn field.
        // Note: The caller should use the persistent connection pattern for in-memory DBs
        // to ensure data consistency.
        self.db.connect().map_err(|e| RosterStorageError::ConnectionFailed(e.to_string()))
    }

    /// Execute a query using the persistent connection for in-memory databases.
    /// This ensures data written by migrations is visible to queries.
    async fn query_with_persistent<'a>(
        &self,
        sql: &'a str,
        params: impl libsql::IntoParams,
    ) -> Result<libsql::Rows, RosterStorageError> {
        if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            conn.query(sql, params)
                .await
                .map_err(|e| RosterStorageError::QueryFailed(e.to_string()))
        } else {
            let conn = self.db.connect().map_err(|e| RosterStorageError::ConnectionFailed(e.to_string()))?;
            conn.query(sql, params)
                .await
                .map_err(|e| RosterStorageError::QueryFailed(e.to_string()))
        }
    }

    /// Execute a statement using the persistent connection for in-memory databases.
    async fn execute_with_persistent(
        &self,
        sql: &str,
        params: impl libsql::IntoParams,
    ) -> Result<u64, RosterStorageError> {
        if let Some(persistent) = self.db.persistent_connection() {
            let conn = persistent.lock().await;
            conn.execute(sql, params)
                .await
                .map_err(|e| RosterStorageError::QueryFailed(e.to_string()))
        } else {
            let conn = self.db.connect().map_err(|e| RosterStorageError::ConnectionFailed(e.to_string()))?;
            conn.execute(sql, params)
                .await
                .map_err(|e| RosterStorageError::QueryFailed(e.to_string()))
        }
    }

    /// Increment the roster version for a user.
    async fn increment_roster_version(
        &self,
        user_jid: &BareJid,
    ) -> Result<(), RosterStorageError> {
        let conn = self.get_connection().await?;

        // Generate a new version string (UUID-based)
        let new_version = Uuid::new_v4().to_string();

        conn.execute(
            r#"
            INSERT INTO roster_versions (user_jid, version, updated_at)
            VALUES (?, ?, datetime('now'))
            ON CONFLICT(user_jid) DO UPDATE SET
                version = excluded.version,
                updated_at = datetime('now')
            "#,
            libsql::params![user_jid.to_string(), new_version],
        )
        .await
        .map_err(|e| RosterStorageError::QueryFailed(e.to_string()))?;

        Ok(())
    }
}

/// A roster item row from the database.
#[derive(Debug, Clone)]
pub struct RosterItemRow {
    /// The contact's JID (bare JID string).
    pub contact_jid: String,
    /// Optional display name for the contact.
    pub name: Option<String>,
    /// Subscription state: 'none', 'to', 'from', 'both'.
    pub subscription: String,
    /// Pending subscription request: 'subscribe' or None.
    pub ask: Option<String>,
    /// Groups this contact belongs to.
    pub groups: Vec<String>,
}

/// Errors that can occur during roster storage operations.
#[derive(Debug, thiserror::Error)]
pub enum RosterStorageError {
    #[error("Failed to connect to database: {0}")]
    ConnectionFailed(String),

    #[error("Query failed: {0}")]
    QueryFailed(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup_test_db() -> Database {
        let db = Database::in_memory("test-roster").await.unwrap();
        // Run migrations
        let runner = crate::db::MigrationRunner::global();
        runner.run(&db).await.unwrap();
        db
    }

    #[tokio::test]
    async fn test_roster_item_crud() {
        let db = setup_test_db().await;
        let storage = DatabaseRosterStorage::new(db);

        let user_jid: BareJid = "alice@example.com".parse().unwrap();
        let contact_jid: BareJid = "bob@example.com".parse().unwrap();

        // Initially empty
        let roster = storage.get_roster(&user_jid).await.unwrap();
        assert!(roster.is_empty());

        // Add item
        let item = RosterItemRow {
            contact_jid: contact_jid.to_string(),
            name: Some("Bob".to_string()),
            subscription: "none".to_string(),
            ask: None,
            groups: vec!["Friends".to_string()],
        };
        let is_new = storage.set_roster_item(&user_jid, &item).await.unwrap();
        assert!(is_new);

        // Check it exists
        assert!(storage.has_roster_item(&user_jid, &contact_jid).await.unwrap());

        // Get the roster
        let roster = storage.get_roster(&user_jid).await.unwrap();
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].contact_jid, contact_jid.to_string());
        assert_eq!(roster[0].name, Some("Bob".to_string()));
        assert_eq!(roster[0].groups, vec!["Friends".to_string()]);

        // Update item
        let updated_item = RosterItemRow {
            contact_jid: contact_jid.to_string(),
            name: Some("Robert".to_string()),
            subscription: "both".to_string(),
            ask: None,
            groups: vec!["Friends".to_string(), "Work".to_string()],
        };
        let is_new = storage.set_roster_item(&user_jid, &updated_item).await.unwrap();
        assert!(!is_new); // Should be an update, not new

        // Verify update
        let item = storage.get_roster_item(&user_jid, &contact_jid).await.unwrap().unwrap();
        assert_eq!(item.name, Some("Robert".to_string()));
        assert_eq!(item.subscription, "both");

        // Remove item
        let removed = storage.remove_roster_item(&user_jid, &contact_jid).await.unwrap();
        assert!(removed);

        // Verify removal
        assert!(!storage.has_roster_item(&user_jid, &contact_jid).await.unwrap());
        let roster = storage.get_roster(&user_jid).await.unwrap();
        assert!(roster.is_empty());
    }

    #[tokio::test]
    async fn test_subscription_update() {
        let db = setup_test_db().await;
        let storage = DatabaseRosterStorage::new(db);

        let user_jid: BareJid = "alice@example.com".parse().unwrap();
        let contact_jid: BareJid = "bob@example.com".parse().unwrap();

        // Update subscription (creates item if not exists)
        let item = storage
            .update_subscription(&user_jid, &contact_jid, "none", Some("subscribe"))
            .await
            .unwrap();
        assert_eq!(item.subscription, "none");
        assert_eq!(item.ask, Some("subscribe".to_string()));

        // Update to 'to' state
        let item = storage
            .update_subscription(&user_jid, &contact_jid, "to", None)
            .await
            .unwrap();
        assert_eq!(item.subscription, "to");
        assert_eq!(item.ask, None);
    }

    #[tokio::test]
    async fn test_presence_queries() {
        let db = setup_test_db().await;
        let storage = DatabaseRosterStorage::new(db);

        let user_jid: BareJid = "alice@example.com".parse().unwrap();

        // Add contacts with different subscription states
        let contacts = [
            ("bob@example.com", "to"),      // Alice receives Bob's presence
            ("carol@example.com", "from"),  // Carol receives Alice's presence
            ("dan@example.com", "both"),    // Mutual subscription
            ("eve@example.com", "none"),    // No subscription
        ];

        for (contact, subscription) in contacts {
            let item = RosterItemRow {
                contact_jid: contact.to_string(),
                name: None,
                subscription: subscription.to_string(),
                ask: None,
                groups: vec![],
            };
            storage.set_roster_item(&user_jid, &item).await.unwrap();
        }

        // Get presence subscribers (from or both) - these receive Alice's presence
        let subscribers = storage.get_presence_subscribers(&user_jid).await.unwrap();
        assert_eq!(subscribers.len(), 2);
        assert!(subscribers.contains(&"carol@example.com".to_string()));
        assert!(subscribers.contains(&"dan@example.com".to_string()));

        // Get presence subscriptions (to or both) - Alice receives their presence
        let subscriptions = storage.get_presence_subscriptions(&user_jid).await.unwrap();
        assert_eq!(subscriptions.len(), 2);
        assert!(subscriptions.contains(&"bob@example.com".to_string()));
        assert!(subscriptions.contains(&"dan@example.com".to_string()));
    }

    #[tokio::test]
    async fn test_roster_versioning() {
        let db = setup_test_db().await;
        let storage = DatabaseRosterStorage::new(db);

        let user_jid: BareJid = "alice@example.com".parse().unwrap();
        let contact_jid: BareJid = "bob@example.com".parse().unwrap();

        // Initially no version
        let version = storage.get_roster_version(&user_jid).await.unwrap();
        assert!(version.is_none());

        // Add item (creates version)
        let item = RosterItemRow {
            contact_jid: contact_jid.to_string(),
            name: None,
            subscription: "none".to_string(),
            ask: None,
            groups: vec![],
        };
        storage.set_roster_item(&user_jid, &item).await.unwrap();

        let version1 = storage.get_roster_version(&user_jid).await.unwrap();
        assert!(version1.is_some());

        // Update item (updates version)
        storage
            .update_subscription(&user_jid, &contact_jid, "to", None)
            .await
            .unwrap();

        let version2 = storage.get_roster_version(&user_jid).await.unwrap();
        assert!(version2.is_some());
        assert_ne!(version1, version2);
    }
}
