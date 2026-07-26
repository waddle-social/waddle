use jid::BareJid;
use tokio::time::sleep;
use tracing::instrument;
use waddle_xmpp_core::roster::RosterVersion;

use super::retry::{is_sqlite_lock_error, now_utc_text, retry_delay, MAX_LOCK_RETRIES};
use super::{
    DatabaseRosterStorage, RosterRowChange, RosterRowMutation, RosterRowMutationKind,
    RosterStorageError, UserMutationLock,
};

pub(super) const COMMIT_SUBSCRIPTION_UPDATE_SQL: &str = r#"
            INSERT INTO roster_items (user_jid, contact_jid, subscription, ask, approved, groups, updated_at)
            VALUES (?, ?, ?, ?, FALSE, '[]', ?)
            ON CONFLICT(user_jid, contact_jid) DO UPDATE SET
                subscription = excluded.subscription,
                ask = excluded.ask,
                approved = excluded.approved,
                updated_at = excluded.updated_at
            "#;

pub(super) const COMMIT_ROSTER_UPSERT_SQL: &str = r#"
                    INSERT INTO roster_items (user_jid, contact_jid, name, subscription, ask, approved, groups, updated_at)
                    VALUES (?, ?, ?, ?, ?, (? <> 0), ?, ?)
                    ON CONFLICT(user_jid, contact_jid) DO UPDATE SET
                        name = excluded.name,
                        subscription = excluded.subscription,
                        ask = excluded.ask,
                        approved = excluded.approved,
                        groups = excluded.groups,
                        updated_at = excluded.updated_at
                    "#;

impl DatabaseRosterStorage {
    /// Atomic roster mutation: write the row, bump the user's roster version,
    /// return the new version along with a [`UserMutationLock`] guard.
    ///
    /// Both the row write and the version bump run inside a single database
    /// transaction so a partial failure cannot commit the row without the
    /// version (which would break XEP-0237 §2.6's "ver identifies the roster
    /// state" invariant — a client whose cached `ver` happens to equal the
    /// stale stored value would receive an empty result while having missed
    /// the change).
    ///
    /// Acquires the per-user lock, performs the transaction, and returns
    /// both the mutation outcome and the lock guard. The caller MUST keep
    /// the guard alive while it enqueues roster pushes that announce this
    /// mutation's version — otherwise a concurrent mutation could fan out
    /// its own pushes onto the same recipient socket in interleaved order,
    /// violating §2.6's "pushes MUST occur in order of modification"
    /// invariant.
    ///
    /// Returns [`RosterStorageError::ItemNotFound`] if a `Remove` targets a
    /// non-existent item.
    #[instrument(skip(self, change, user_jid), fields(user = %user_jid))]
    pub async fn apply_roster_change(
        &self,
        user_jid: &BareJid,
        change: RosterRowChange,
    ) -> Result<(RosterRowMutation, UserMutationLock), RosterStorageError> {
        let mutex = self.user_lock(user_jid);
        let guard = mutex.lock_owned().await;

        // Pre-check existence so we can classify Added vs Updated outside the
        // transaction — the existence read does not need to be transactional
        // (the per-user lock makes the result race-free).
        let existed_for_upsert = if let RosterRowChange::Upsert(row) = &change {
            let contact: BareJid =
                row.contact_jid
                    .parse()
                    .map_err(|source| RosterStorageError::InvalidStoredJid {
                        value: row.contact_jid.clone(),
                        source,
                    })?;
            Some(self.has_roster_item(user_jid, &contact).await?)
        } else {
            None
        };

        let version = RosterVersion::generate();
        let kind = self
            .commit_mutation(user_jid, change, existed_for_upsert, &version)
            .await?;

        Ok((RosterRowMutation { kind, version }, guard))
    }

    /// Run the row write + version bump as a single transaction. Caller
    /// supplies the pre-checked existence flag for `Upsert` (used to
    /// classify Added vs Updated).
    async fn commit_mutation(
        &self,
        user_jid: &BareJid,
        change: RosterRowChange,
        existed_for_upsert: Option<bool>,
        version: &RosterVersion,
    ) -> Result<RosterRowMutationKind, RosterStorageError> {
        for attempt in 0..=MAX_LOCK_RETRIES {
            let result = self
                .commit_mutation_once(user_jid, &change, existed_for_upsert, version)
                .await;
            match result {
                Ok(kind) => return Ok(kind),
                Err(e) if is_sqlite_lock_error(&e) && attempt < MAX_LOCK_RETRIES => {
                    sleep(retry_delay(attempt)).await;
                }
                Err(e) => return Err(e),
            }
        }
        Err(RosterStorageError::QueryFailed(
            "Transaction failed after lock retries".to_string(),
        ))
    }

    async fn commit_mutation_once(
        &self,
        user_jid: &BareJid,
        change: &RosterRowChange,
        existed_for_upsert: Option<bool>,
        version: &RosterVersion,
    ) -> Result<RosterRowMutationKind, RosterStorageError> {
        let mut tx = self.db.begin().await?;
        let now = now_utc_text();

        let kind = match change {
            RosterRowChange::Upsert(row) => {
                let groups_json = serde_json::to_string(&row.groups)
                    .map_err(RosterStorageError::SerializationError)?;
                tx.execute(
                    COMMIT_ROSTER_UPSERT_SQL,
                    crate::db_params![
                        user_jid.to_string(),
                        row.contact_jid.clone(),
                        row.name.clone(),
                        row.subscription.clone(),
                        row.ask.clone(),
                        row.approved,
                        groups_json,
                        now.clone(),
                    ],
                )
                .await?;
                if existed_for_upsert.unwrap_or(false) {
                    RosterRowMutationKind::Updated(row.clone())
                } else {
                    RosterRowMutationKind::Added(row.clone())
                }
            }
            RosterRowChange::Remove(contact_jid) => {
                let rows = tx
                    .execute(
                        "DELETE FROM roster_items WHERE user_jid = ? AND contact_jid = ?",
                        crate::db_params![user_jid.to_string(), contact_jid.to_string()],
                    )
                    .await?;
                if rows == 0 {
                    // Drop tx (auto-rolls-back) and surface ItemNotFound.
                    return Err(RosterStorageError::ItemNotFound);
                }
                RosterRowMutationKind::Removed(contact_jid.clone())
            }
        };

        tx.execute(
            r#"
            INSERT INTO roster_versions (user_jid, version, updated_at)
            VALUES (?, ?, ?)
            ON CONFLICT(user_jid) DO UPDATE SET
                version = excluded.version,
                updated_at = excluded.updated_at
            "#,
            crate::db_params![user_jid.to_string(), version.as_str().to_string(), now],
        )
        .await?;

        tx.commit().await?;
        Ok(kind)
    }

    /// Atomic subscription update: read-modify-write of just the subscription/ask
    /// fields on an existing (or implicit) roster item, plus version bump.
    /// Returns the mutation result and the [`UserMutationLock`] guard the
    /// caller must hold across roster-push enqueue (see
    /// [`apply_roster_change`]).
    ///
    /// Used by the RFC 6121 presence subscription state machine when it needs
    /// to flip subscription/ask without disturbing name/groups. The row write
    /// and version bump run inside a single database transaction under the
    /// per-user lock.
    #[instrument(skip(self, user_jid, contact_jid), fields(user = %user_jid, contact = %contact_jid))]
    pub async fn apply_subscription_update(
        &self,
        user_jid: &BareJid,
        contact_jid: &BareJid,
        subscription: &str,
        ask: Option<&str>,
    ) -> Result<(RosterRowMutation, UserMutationLock), RosterStorageError> {
        let mutex = self.user_lock(user_jid);
        let guard = mutex.lock_owned().await;

        let existed = self.has_roster_item(user_jid, contact_jid).await?;
        let version = RosterVersion::generate();

        for attempt in 0..=MAX_LOCK_RETRIES {
            match self
                .commit_subscription_update_once(user_jid, contact_jid, subscription, ask, &version)
                .await
            {
                Ok(()) => break,
                Err(e) if is_sqlite_lock_error(&e) && attempt < MAX_LOCK_RETRIES => {
                    sleep(retry_delay(attempt)).await;
                }
                Err(e) => return Err(e),
            }
        }

        let row = self
            .get_roster_item(user_jid, contact_jid)
            .await?
            .ok_or_else(|| {
                RosterStorageError::QueryFailed("Item missing after upsert".to_string())
            })?;

        let kind = if existed {
            RosterRowMutationKind::Updated(row)
        } else {
            RosterRowMutationKind::Added(row)
        };
        Ok((RosterRowMutation { kind, version }, guard))
    }

    async fn commit_subscription_update_once(
        &self,
        user_jid: &BareJid,
        contact_jid: &BareJid,
        subscription: &str,
        ask: Option<&str>,
        version: &RosterVersion,
    ) -> Result<(), RosterStorageError> {
        let mut tx = self.db.begin().await?;
        let now = now_utc_text();
        tx.execute(
            COMMIT_SUBSCRIPTION_UPDATE_SQL,
            crate::db_params![
                user_jid.to_string(),
                contact_jid.to_string(),
                subscription.to_string(),
                ask.map(|s| s.to_string()),
                now.clone(),
            ],
        )
        .await?;
        tx.execute(
            r#"
            INSERT INTO roster_versions (user_jid, version, updated_at)
            VALUES (?, ?, ?)
            ON CONFLICT(user_jid) DO UPDATE SET
                version = excluded.version,
                updated_at = excluded.updated_at
            "#,
            crate::db_params![user_jid.to_string(), version.as_str().to_string(), now],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}
