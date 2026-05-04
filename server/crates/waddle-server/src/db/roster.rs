//! Database-backed roster storage for RFC 6121 compliance.
//!
//! This module implements the `RosterStorage` trait from `waddle-xmpp` using
//! the internal SQLx-backed database adapter for persistent storage.

use crate::db::IntoParams;
use dashmap::DashMap;
use jid::BareJid;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::sleep;
use tracing::{debug, instrument};
use waddle_xmpp_core::roster::RosterVersion;

use super::Database;

// `RosterVersion::generate()` lives in `waddle-xmpp-core` and uses uuid::v4 internally;
// no Uuid import needed here.

/// A roster mutation request at the row layer.
#[derive(Debug, Clone)]
pub enum RosterRowChange {
    /// Add or update an item. Storage decides Added vs Updated based on
    /// whether the row already exists.
    Upsert(RosterItemRow),
    /// Remove the item with the given contact JID.
    Remove(BareJid),
}

/// Outcome of an atomic roster mutation: the classified row-layer result and
/// the post-mutation roster version, computed under the same per-user lock.
///
/// XEP-0237 §2.6 requires every roster push to carry the post-mutation
/// version, those versions to be unique, and pushes to occur in modification
/// order. Returning the version from the same call that performed the
/// mutation is what makes those MUSTs holdable under concurrency.
#[derive(Debug, Clone)]
pub struct RosterRowMutation {
    /// Classified row-layer result (Added / Updated / Removed).
    pub kind: RosterRowMutationKind,
    /// Roster version after this mutation.
    pub version: RosterVersion,
}

/// Row-layer outcome classification.
#[derive(Debug, Clone)]
pub enum RosterRowMutationKind {
    /// Item was newly inserted.
    Added(RosterItemRow),
    /// Existing item was overwritten.
    Updated(RosterItemRow),
    /// Item was deleted.
    Removed(BareJid),
}

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
    db: Database,
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

    fn user_lock(&self, user_jid: &BareJid) -> Arc<Mutex<()>> {
        self.user_locks
            .entry(user_jid.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Atomic roster mutation: write the row, bump the user's roster version,
    /// return the new version.
    ///
    /// Acquires a per-user lock, performs the data write, then writes a fresh
    /// [`RosterVersion`]. All three steps observe the same lock so concurrent
    /// callers see distinct, ordered versions.
    ///
    /// Returns `Err(RosterStorageError::QueryFailed("Item not found"))` if a
    /// `Remove` targets a non-existent item — callers map this to
    /// `<item-not-found/>`.
    #[instrument(skip(self, change), fields(user = %user_jid))]
    pub async fn apply_roster_change(
        &self,
        user_jid: &BareJid,
        change: RosterRowChange,
    ) -> Result<RosterRowMutation, RosterStorageError> {
        let lock = self.user_lock(user_jid);
        let _guard = lock.lock().await;

        let kind = match change {
            RosterRowChange::Upsert(row) => {
                let contact: BareJid = row.contact_jid.parse().map_err(|e| {
                    RosterStorageError::QueryFailed(format!("Invalid contact JID: {}", e))
                })?;
                let existed = self.has_roster_item(user_jid, &contact).await?;
                self.write_roster_item(user_jid, &row).await?;
                if existed {
                    RosterRowMutationKind::Updated(row)
                } else {
                    RosterRowMutationKind::Added(row)
                }
            }
            RosterRowChange::Remove(contact_jid) => {
                let removed = self.delete_roster_item(user_jid, &contact_jid).await?;
                if !removed {
                    return Err(RosterStorageError::QueryFailed(
                        "Item not found".to_string(),
                    ));
                }
                RosterRowMutationKind::Removed(contact_jid)
            }
        };

        let version = RosterVersion::generate();
        self.write_roster_version(user_jid, &version).await?;
        Ok(RosterRowMutation { kind, version })
    }

    /// Atomic subscription update: read-modify-write of just the subscription/ask
    /// fields on an existing (or implicit) roster item, plus version bump.
    ///
    /// Used by the RFC 6121 presence subscription state machine when it needs
    /// to flip subscription/ask without disturbing name/groups. The whole
    /// read-modify-write happens under the per-user lock so it composes safely
    /// with [`apply_roster_change`].
    #[instrument(skip(self), fields(user = %user_jid, contact = %contact_jid))]
    pub async fn apply_subscription_update(
        &self,
        user_jid: &BareJid,
        contact_jid: &BareJid,
        subscription: &str,
        ask: Option<&str>,
    ) -> Result<RosterRowMutation, RosterStorageError> {
        let lock = self.user_lock(user_jid);
        let _guard = lock.lock().await;

        let existed = self.has_roster_item(user_jid, contact_jid).await?;
        let user_jid_s = user_jid.to_string();
        let contact_jid_s = contact_jid.to_string();
        let subscription_s = subscription.to_string();
        let ask_s = ask.map(|s| s.to_string());

        self.execute_with_retry(
            r#"
            INSERT INTO roster_items (user_jid, contact_jid, subscription, ask, approved, groups, updated_at)
            VALUES (?, ?, ?, ?, 0, '[]', datetime('now'))
            ON CONFLICT(user_jid, contact_jid) DO UPDATE SET
                subscription = excluded.subscription,
                ask = excluded.ask,
                approved = excluded.approved,
                updated_at = datetime('now')
            "#,
            || {
                crate::db_params![
                    user_jid_s.clone(),
                    contact_jid_s.clone(),
                    subscription_s.clone(),
                    ask_s.clone(),
                ]
            },
        )
        .await?;

        let row = self
            .get_roster_item(user_jid, contact_jid)
            .await?
            .ok_or_else(|| {
                RosterStorageError::QueryFailed("Item missing after upsert".to_string())
            })?;

        let version = RosterVersion::generate();
        self.write_roster_version(user_jid, &version).await?;

        let kind = if existed {
            RosterRowMutationKind::Updated(row)
        } else {
            RosterRowMutationKind::Added(row)
        };
        Ok(RosterRowMutation { kind, version })
    }

    async fn write_roster_item(
        &self,
        user_jid: &BareJid,
        item: &RosterItemRow,
    ) -> Result<(), RosterStorageError> {
        let groups_json = serde_json::to_string(&item.groups)
            .map_err(|e| RosterStorageError::SerializationError(e.to_string()))?;

        let user_jid_s = user_jid.to_string();
        let contact_jid_s = item.contact_jid.clone();
        let name = item.name.clone();
        let subscription = item.subscription.clone();
        let ask = item.ask.clone();
        let approved = item.approved;
        let groups_json_param = groups_json.clone();

        self.execute_with_retry(
            r#"
            INSERT INTO roster_items (user_jid, contact_jid, name, subscription, ask, approved, groups, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, datetime('now'))
            ON CONFLICT(user_jid, contact_jid) DO UPDATE SET
                name = excluded.name,
                subscription = excluded.subscription,
                ask = excluded.ask,
                approved = excluded.approved,
                groups = excluded.groups,
                updated_at = datetime('now')
            "#,
            || {
                crate::db_params![
                    user_jid_s.clone(),
                    contact_jid_s.clone(),
                    name.clone(),
                    subscription.clone(),
                    ask.clone(),
                    approved,
                    groups_json_param.clone(),
                ]
            },
        )
        .await?;
        Ok(())
    }

    async fn delete_roster_item(
        &self,
        user_jid: &BareJid,
        contact_jid: &BareJid,
    ) -> Result<bool, RosterStorageError> {
        let user_jid_s = user_jid.to_string();
        let contact_jid_s = contact_jid.to_string();
        let result = self
            .execute_with_retry(
                "DELETE FROM roster_items WHERE user_jid = ? AND contact_jid = ?",
                || crate::db_params![user_jid_s.clone(), contact_jid_s.clone()],
            )
            .await?;
        Ok(result > 0)
    }

    async fn write_roster_version(
        &self,
        user_jid: &BareJid,
        version: &RosterVersion,
    ) -> Result<(), RosterStorageError> {
        let user_jid_s = user_jid.to_string();
        let version_s = version.as_str().to_string();
        self.execute_with_retry(
            r#"
            INSERT INTO roster_versions (user_jid, version, updated_at)
            VALUES (?, ?, datetime('now'))
            ON CONFLICT(user_jid) DO UPDATE SET
                version = excluded.version,
                updated_at = datetime('now')
            "#,
            || crate::db_params![user_jid_s.clone(), version_s.clone()],
        )
        .await?;
        Ok(())
    }

    /// Get all roster items for a user.
    #[instrument(skip(self), fields(user = %user_jid))]
    pub async fn get_roster(
        &self,
        user_jid: &BareJid,
    ) -> Result<Vec<RosterItemRow>, RosterStorageError> {
        let mut rows = self.query_with_persistent(
            "SELECT contact_jid, name, subscription, ask, approved, groups FROM roster_items WHERE user_jid = ?",
            crate::db_params![user_jid.to_string()],
        ).await?;

        let mut items = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| RosterStorageError::QueryFailed(format!("Failed to read row: {}", e)))?
        {
            let contact_jid: String = row.get(0).map_err(|e| {
                RosterStorageError::QueryFailed(format!("Failed to get contact_jid: {}", e))
            })?;
            let name: Option<String> = row.get(1).ok();
            let subscription: String = row.get(2).map_err(|e| {
                RosterStorageError::QueryFailed(format!("Failed to get subscription: {}", e))
            })?;
            let ask: Option<String> = row.get(3).ok();
            let approved: bool = row.get(4).unwrap_or(false);
            let groups_json: Option<String> = row.get(5).ok();

            let groups: Vec<String> = groups_json
                .and_then(|json| serde_json::from_str(&json).ok())
                .unwrap_or_default();

            items.push(RosterItemRow {
                contact_jid,
                name,
                subscription,
                ask,
                approved,
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
        let mut rows = self.query_with_persistent(
            "SELECT contact_jid, name, subscription, ask, approved, groups FROM roster_items WHERE user_jid = ? AND contact_jid = ?",
            crate::db_params![user_jid.to_string(), contact_jid.to_string()],
        ).await?;

        match rows
            .next()
            .await
            .map_err(|e| RosterStorageError::QueryFailed(format!("Failed to read row: {}", e)))?
        {
            Some(row) => {
                let contact_jid: String = row.get(0).map_err(|e| {
                    RosterStorageError::QueryFailed(format!("Failed to get contact_jid: {}", e))
                })?;
                let name: Option<String> = row.get(1).ok();
                let subscription: String = row.get(2).map_err(|e| {
                    RosterStorageError::QueryFailed(format!("Failed to get subscription: {}", e))
                })?;
                let ask: Option<String> = row.get(3).ok();
                let approved: bool = row.get(4).unwrap_or(false);
                let groups_json: Option<String> = row.get(5).ok();

                let groups: Vec<String> = groups_json
                    .and_then(|json| serde_json::from_str(&json).ok())
                    .unwrap_or_default();

                Ok(Some(RosterItemRow {
                    contact_jid,
                    name,
                    subscription,
                    ask,
                    approved,
                    groups,
                }))
            }
            None => Ok(None),
        }
    }

    /// Get the current roster version for a user.
    #[instrument(skip(self), fields(user = %user_jid))]
    pub async fn get_roster_version(
        &self,
        user_jid: &BareJid,
    ) -> Result<Option<String>, RosterStorageError> {
        let mut rows = self
            .query_with_persistent(
                "SELECT version FROM roster_versions WHERE user_jid = ?",
                crate::db_params![user_jid.to_string()],
            )
            .await?;

        match rows
            .next()
            .await
            .map_err(|e| RosterStorageError::QueryFailed(format!("Failed to read row: {}", e)))?
        {
            Some(row) => {
                let version: String = row.get(0).map_err(|e| {
                    RosterStorageError::QueryFailed(format!("Failed to get version: {}", e))
                })?;
                Ok(Some(version))
            }
            None => Ok(None),
        }
    }

    /// Get the current roster version, synthesising one for an otherwise empty
    /// roster so first-sync responses can carry a `ver` attribute.
    ///
    /// Acquires the per-user lock and writes a fresh version when none exists,
    /// matching the atomicity guarantees of [`apply_roster_change`].
    #[instrument(skip(self), fields(user = %user_jid))]
    pub async fn get_or_create_roster_version(
        &self,
        user_jid: &BareJid,
    ) -> Result<RosterVersion, RosterStorageError> {
        if let Some(existing) = self.get_roster_version(user_jid).await? {
            return existing.parse::<RosterVersion>().map_err(|e| {
                RosterStorageError::QueryFailed(format!("Stored roster version is invalid: {}", e))
            });
        }

        let lock = self.user_lock(user_jid);
        let _guard = lock.lock().await;
        // Re-check under the lock in case another writer raced us.
        if let Some(existing) = self.get_roster_version(user_jid).await? {
            return existing.parse::<RosterVersion>().map_err(|e| {
                RosterStorageError::QueryFailed(format!("Stored roster version is invalid: {}", e))
            });
        }
        let version = RosterVersion::generate();
        self.write_roster_version(user_jid, &version).await?;
        Ok(version)
    }

    /// Check if a roster item exists.
    #[instrument(skip(self), fields(user = %user_jid, contact = %contact_jid))]
    pub async fn has_roster_item(
        &self,
        user_jid: &BareJid,
        contact_jid: &BareJid,
    ) -> Result<bool, RosterStorageError> {
        let mut rows = self
            .query_with_persistent(
                "SELECT 1 FROM roster_items WHERE user_jid = ? AND contact_jid = ?",
                crate::db_params![user_jid.to_string(), contact_jid.to_string()],
            )
            .await?;

        let exists = rows
            .next()
            .await
            .map_err(|e| RosterStorageError::QueryFailed(format!("Failed to read row: {}", e)))?
            .is_some();

        Ok(exists)
    }

    /// Get all roster items where the user should send presence updates.
    ///
    /// Returns contacts with subscription=from or subscription=both.
    #[instrument(skip(self), fields(user = %user_jid))]
    pub async fn get_presence_subscribers(
        &self,
        user_jid: &BareJid,
    ) -> Result<Vec<String>, RosterStorageError> {
        let mut rows = self.query_with_persistent(
            "SELECT contact_jid FROM roster_items WHERE user_jid = ? AND subscription IN ('from', 'both')",
            crate::db_params![user_jid.to_string()],
        ).await?;

        let mut jids = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| RosterStorageError::QueryFailed(format!("Failed to read row: {}", e)))?
        {
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
    #[cfg(test)]
    #[instrument(skip(self), fields(user = %user_jid))]
    pub async fn get_presence_subscriptions(
        &self,
        user_jid: &BareJid,
    ) -> Result<Vec<String>, RosterStorageError> {
        let mut rows = self.query_with_persistent(
            "SELECT contact_jid FROM roster_items WHERE user_jid = ? AND subscription IN ('to', 'both')",
            crate::db_params![user_jid.to_string()],
        ).await?;

        let mut jids = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| RosterStorageError::QueryFailed(format!("Failed to read row: {}", e)))?
        {
            let jid: String = row.get(0).map_err(|e| {
                RosterStorageError::QueryFailed(format!("Failed to get jid: {}", e))
            })?;
            jids.push(jid);
        }

        debug!(count = jids.len(), "Retrieved presence subscriptions");
        Ok(jids)
    }

    /// Execute a query using a connection guard from the database.
    async fn query_with_persistent(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<crate::db::Rows, RosterStorageError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|e| RosterStorageError::ConnectionFailed(e.to_string()))?;
        conn.query(sql, params)
            .await
            .map_err(|e| RosterStorageError::QueryFailed(e.to_string()))
    }

    /// Execute a statement using a connection guard from the database.
    async fn execute_with_persistent(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<u64, RosterStorageError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|e| RosterStorageError::ConnectionFailed(e.to_string()))?;
        conn.execute(sql, params)
            .await
            .map_err(|e| RosterStorageError::QueryFailed(e.to_string()))
    }

    /// Execute a statement with retries for transient sqlite lock contention.
    async fn execute_with_retry<P, F>(
        &self,
        sql: &str,
        params: F,
    ) -> Result<u64, RosterStorageError>
    where
        P: IntoParams,
        F: Fn() -> P,
    {
        const MAX_LOCK_RETRIES: usize = 6;

        for attempt in 0..=MAX_LOCK_RETRIES {
            match self.execute_with_persistent(sql, params()).await {
                Ok(rows_affected) => return Ok(rows_affected),
                Err(e) if is_sqlite_lock_error(&e) && attempt < MAX_LOCK_RETRIES => {
                    sleep(retry_delay(attempt)).await;
                }
                Err(e) => return Err(e),
            }
        }

        Err(RosterStorageError::QueryFailed(
            "Execute failed after lock retries".to_string(),
        ))
    }
}

fn is_sqlite_lock_error(error: &RosterStorageError) -> bool {
    let msg = error.to_string().to_ascii_lowercase();
    msg.contains("database is locked")
        || msg.contains("sqlite_busy")
        || msg.contains("database busy")
}

fn retry_delay(attempt: usize) -> Duration {
    // Exponential backoff with a short ceiling for local sqlite contention.
    let base_ms = 10_u64;
    let max_ms = 320_u64;
    let delay_ms = (base_ms << attempt.min(5)).min(max_ms);
    Duration::from_millis(delay_ms)
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
    /// Whether the contact is pre-approved for a future subscription request.
    pub approved: bool,
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

    fn make_row(contact_jid: &str, subscription: &str) -> RosterItemRow {
        RosterItemRow {
            contact_jid: contact_jid.to_string(),
            name: None,
            subscription: subscription.to_string(),
            ask: None,
            approved: false,
            groups: vec![],
        }
    }

    #[tokio::test]
    async fn test_apply_roster_change_upsert_and_remove() {
        let db = setup_test_db().await;
        let storage = DatabaseRosterStorage::new(db);

        let user_jid: BareJid = "alice@example.com".parse().unwrap();
        let contact_jid: BareJid = "bob@example.com".parse().unwrap();

        let roster = storage.get_roster(&user_jid).await.unwrap();
        assert!(roster.is_empty());

        let row = RosterItemRow {
            contact_jid: contact_jid.to_string(),
            name: Some("Bob".to_string()),
            subscription: "none".to_string(),
            ask: None,
            approved: false,
            groups: vec!["Friends".to_string()],
        };
        let added = storage
            .apply_roster_change(&user_jid, RosterRowChange::Upsert(row))
            .await
            .unwrap();
        assert!(matches!(added.kind, RosterRowMutationKind::Added(_)));

        let updated_row = RosterItemRow {
            contact_jid: contact_jid.to_string(),
            name: Some("Robert".to_string()),
            subscription: "both".to_string(),
            ask: None,
            approved: false,
            groups: vec!["Friends".to_string(), "Work".to_string()],
        };
        let updated = storage
            .apply_roster_change(&user_jid, RosterRowChange::Upsert(updated_row))
            .await
            .unwrap();
        assert!(matches!(updated.kind, RosterRowMutationKind::Updated(_)));
        assert_ne!(added.version, updated.version);

        let removed = storage
            .apply_roster_change(&user_jid, RosterRowChange::Remove(contact_jid.clone()))
            .await
            .unwrap();
        assert!(matches!(removed.kind, RosterRowMutationKind::Removed(_)));
        assert_ne!(updated.version, removed.version);

        assert!(!storage
            .has_roster_item(&user_jid, &contact_jid)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_apply_roster_change_remove_missing_returns_error() {
        let db = setup_test_db().await;
        let storage = DatabaseRosterStorage::new(db);

        let user_jid: BareJid = "alice@example.com".parse().unwrap();
        let contact_jid: BareJid = "ghost@example.com".parse().unwrap();

        let result = storage
            .apply_roster_change(&user_jid, RosterRowChange::Remove(contact_jid))
            .await;
        assert!(matches!(result, Err(RosterStorageError::QueryFailed(_))));
    }

    #[tokio::test]
    async fn test_apply_subscription_update_bumps_version() {
        let db = setup_test_db().await;
        let storage = DatabaseRosterStorage::new(db);

        let user_jid: BareJid = "alice@example.com".parse().unwrap();
        let contact_jid: BareJid = "bob@example.com".parse().unwrap();

        let first = storage
            .apply_subscription_update(&user_jid, &contact_jid, "none", Some("subscribe"))
            .await
            .unwrap();
        match &first.kind {
            RosterRowMutationKind::Added(row) => {
                assert_eq!(row.subscription, "none");
                assert_eq!(row.ask, Some("subscribe".to_string()));
            }
            other => panic!("expected Added, got {:?}", other),
        }

        let second = storage
            .apply_subscription_update(&user_jid, &contact_jid, "to", None)
            .await
            .unwrap();
        match &second.kind {
            RosterRowMutationKind::Updated(row) => {
                assert_eq!(row.subscription, "to");
                assert!(row.ask.is_none());
            }
            other => panic!("expected Updated, got {:?}", other),
        }
        assert_ne!(first.version, second.version);
    }

    #[tokio::test]
    async fn test_presence_queries() {
        let db = setup_test_db().await;
        let storage = DatabaseRosterStorage::new(db);

        let user_jid: BareJid = "alice@example.com".parse().unwrap();

        for (contact, subscription) in [
            ("bob@example.com", "to"),
            ("carol@example.com", "from"),
            ("dan@example.com", "both"),
            ("eve@example.com", "none"),
        ] {
            storage
                .apply_roster_change(
                    &user_jid,
                    RosterRowChange::Upsert(make_row(contact, subscription)),
                )
                .await
                .unwrap();
        }

        let subscribers = storage.get_presence_subscribers(&user_jid).await.unwrap();
        assert_eq!(subscribers.len(), 2);
        assert!(subscribers.contains(&"carol@example.com".to_string()));
        assert!(subscribers.contains(&"dan@example.com".to_string()));

        let subscriptions = storage.get_presence_subscriptions(&user_jid).await.unwrap();
        assert_eq!(subscriptions.len(), 2);
        assert!(subscriptions.contains(&"bob@example.com".to_string()));
        assert!(subscriptions.contains(&"dan@example.com".to_string()));
    }

    #[tokio::test]
    async fn test_get_or_create_roster_version_synthesises_for_empty_roster() {
        let db = setup_test_db().await;
        let storage = DatabaseRosterStorage::new(db);

        let user_jid: BareJid = "alice@example.com".parse().unwrap();
        assert!(storage
            .get_roster_version(&user_jid)
            .await
            .unwrap()
            .is_none());

        let v1 = storage
            .get_or_create_roster_version(&user_jid)
            .await
            .unwrap();
        let v2 = storage
            .get_or_create_roster_version(&user_jid)
            .await
            .unwrap();
        assert_eq!(v1, v2, "second call should return the same version");
    }

    /// XEP-0237 §2.6 conformance regression test (T6 in PR #336).
    ///
    /// Concurrent mutations against the same user must yield distinct versions —
    /// "the version contained in a roster push MUST be unique" / "in order of
    /// modification". The per-user lock in `apply_roster_change` is what holds
    /// these MUSTs. If a future change splits the mutation+version-bump into
    /// two awaits without serialisation, this test will start failing under
    /// load.
    #[tokio::test]
    async fn apply_roster_change_emits_unique_versions_under_concurrency() {
        let db = setup_test_db().await;
        let storage = DatabaseRosterStorage::new(db);
        let user_jid: BareJid = "alice@example.com".parse().unwrap();

        const N: usize = 16;
        let mut handles = Vec::with_capacity(N);
        for i in 0..N {
            let storage = storage.clone();
            let user_jid = user_jid.clone();
            let row = make_row(&format!("contact{i}@example.com"), "none");
            handles.push(tokio::spawn(async move {
                storage
                    .apply_roster_change(&user_jid, RosterRowChange::Upsert(row))
                    .await
                    .unwrap()
                    .version
            }));
        }

        let mut versions = Vec::with_capacity(N);
        for h in handles {
            versions.push(h.await.unwrap());
        }

        let unique: std::collections::HashSet<_> =
            versions.iter().map(|v| v.as_str().to_string()).collect();
        assert_eq!(
            unique.len(),
            N,
            "all {N} concurrent mutations must produce distinct versions; got {versions:?}"
        );
    }
}
