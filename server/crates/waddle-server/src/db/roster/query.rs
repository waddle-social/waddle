use crate::db::IntoParams;
use jid::BareJid;
use tokio::time::sleep;
use tracing::{debug, instrument};
use waddle_xmpp_core::roster::RosterVersion;

use super::retry::{is_sqlite_lock_error, now_utc_text, retry_delay, MAX_LOCK_RETRIES};
use super::{DatabaseRosterStorage, RosterItemRow, RosterStorageError};

impl DatabaseRosterStorage {
    /// Atomic roster snapshot: read all items and the current `RosterVersion`
    /// under the per-user lock so the returned pair is mutually consistent.
    ///
    /// Without the shared lock, a `get_roster` + `get_roster_version` pair can
    /// straddle a concurrent mutation and produce a snapshot whose items reflect
    /// version V+1 but whose `ver` reads as V. A client caching that pair would
    /// believe it was up-to-date when it had in fact missed an item, since on
    /// reconnect the server would respond to a `ver=V` query with an empty
    /// result. That breaks the XEP-0237 §2.6 invariant that `ver` identifies
    /// the roster state.
    ///
    /// Synthesises a fresh version if none exists, matching
    /// [`get_or_create_roster_version`].
    #[instrument(skip(self, user_jid), fields(user = %user_jid))]
    pub async fn snapshot_roster(
        &self,
        user_jid: &BareJid,
    ) -> Result<(Vec<RosterItemRow>, RosterVersion), RosterStorageError> {
        let lock = self.user_lock(user_jid);
        let _guard = lock.lock().await;

        let items = self.get_roster(user_jid).await?;
        let version = if let Some(stored) = self.get_roster_version(user_jid).await? {
            stored.parse::<RosterVersion>().map_err(|_| {
                RosterStorageError::InvalidStoredVersion {
                    value: stored.clone(),
                }
            })?
        } else {
            let v = RosterVersion::generate();
            self.write_roster_version(user_jid, &v).await?;
            v
        };
        Ok((items, version))
    }

    async fn write_roster_version(
        &self,
        user_jid: &BareJid,
        version: &RosterVersion,
    ) -> Result<(), RosterStorageError> {
        let user_jid_s = user_jid.to_string();
        let version_s = version.as_str().to_string();
        let now = now_utc_text();
        self.execute_with_retry(
            r#"
            INSERT INTO roster_versions (user_jid, version, updated_at)
            VALUES (?, ?, ?)
            ON CONFLICT(user_jid) DO UPDATE SET
                version = excluded.version,
                updated_at = excluded.updated_at
            "#,
            || crate::db_params![user_jid_s.clone(), version_s.clone(), now.clone()],
        )
        .await?;
        Ok(())
    }

    /// Get all roster items for a user.
    #[instrument(skip(self, user_jid), fields(user = %user_jid))]
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
    #[instrument(skip(self, user_jid, contact_jid), fields(user = %user_jid, contact = %contact_jid))]
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
    #[instrument(skip(self, user_jid), fields(user = %user_jid))]
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
    #[instrument(skip(self, user_jid), fields(user = %user_jid))]
    pub async fn get_or_create_roster_version(
        &self,
        user_jid: &BareJid,
    ) -> Result<RosterVersion, RosterStorageError> {
        if let Some(existing) = self.get_roster_version(user_jid).await? {
            return existing.parse::<RosterVersion>().map_err(|_| {
                RosterStorageError::InvalidStoredVersion {
                    value: existing.clone(),
                }
            });
        }

        let lock = self.user_lock(user_jid);
        let _guard = lock.lock().await;
        // Re-check under the lock in case another writer raced us.
        if let Some(existing) = self.get_roster_version(user_jid).await? {
            return existing.parse::<RosterVersion>().map_err(|_| {
                RosterStorageError::InvalidStoredVersion {
                    value: existing.clone(),
                }
            });
        }
        let version = RosterVersion::generate();
        self.write_roster_version(user_jid, &version).await?;
        Ok(version)
    }

    /// Check if a roster item exists.
    #[instrument(skip(self, user_jid, contact_jid), fields(user = %user_jid, contact = %contact_jid))]
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
    /// Returns contacts with subscription=from or subscription=both as
    /// typed `BareJid` values — per the typed-payloads hard rule, the
    /// untyped `String` form from storage is parsed exactly once at the
    /// boundary so handler code never touches raw JID strings. Rows
    /// that fail to parse are dropped with a debug log rather than
    /// surfaced as a per-row error, since corrupted JIDs in the roster
    /// are unactionable for the caller.
    #[instrument(skip(self, user_jid), fields(user = %user_jid))]
    pub async fn get_presence_subscribers(
        &self,
        user_jid: &BareJid,
    ) -> Result<Vec<BareJid>, RosterStorageError> {
        let mut rows = self.query_with_persistent(
            "SELECT contact_jid FROM roster_items WHERE user_jid = ? AND subscription IN ('from', 'both')",
            crate::db_params![user_jid.to_string()],
        ).await?;

        let mut jids: Vec<BareJid> = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| RosterStorageError::QueryFailed(format!("Failed to read row: {}", e)))?
        {
            let raw: String = row.get(0).map_err(|e| {
                RosterStorageError::QueryFailed(format!("Failed to get jid: {}", e))
            })?;
            match raw.parse::<BareJid>() {
                Ok(jid) => jids.push(jid),
                Err(error) => {
                    debug!(
                        error = %error,
                        raw = %raw,
                        user = %user_jid,
                        "Skipping un-parseable contact_jid row in get_presence_subscribers"
                    );
                }
            }
        }

        debug!(count = jids.len(), "Retrieved presence subscribers");
        Ok(jids)
    }

    /// Get all roster items where the user receives presence updates.
    ///
    /// Returns contacts with subscription=to or subscription=both.
    #[cfg(test)]
    #[instrument(skip(self, user_jid), fields(user = %user_jid))]
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
