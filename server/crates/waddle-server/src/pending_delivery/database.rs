use super::*;

mod schema;

use super::codec::{decode_row, serialize_message, PAYLOAD_KIND_ARCHIVED, PAYLOAD_KIND_TRANSIENT};
use crate::db::Transaction;

use waddle_xmpp::ownership::{CurrentNodeIdentityGuard, Entity, EntityType, SharedNodeIdentity};
use waddle_xmpp::stream_management::persistence::SmClaimFence;

// ---------------------------------------------------------------------------
// Database-backed PendingDeliveryStorage (issue #209, slice (b) production
// backend).
// ---------------------------------------------------------------------------

/// Per-recipient mutex map serializing inserts so the
/// `INSERT … SELECT … WHERE COUNT < cap` quota check is strict on
/// Postgres too. SQLite serializes writers globally so this is
/// belt-and-suspenders for SQLite, but on Postgres with the default
/// READ-COMMITTED isolation level two concurrent inserts can both
/// observe the same `COUNT(*)` snapshot and exceed `max_rows`. The
/// app-level lock per recipient bare-JID closes that race
/// portably across drivers.
///
/// Lock granularity is per recipient — concurrent inserts for
/// *different* recipients don't contend.
type RecipientLockMap = dashmap::DashMap<BareJid, std::sync::Arc<tokio::sync::Mutex<()>>>;

/// Sweep [`RecipientLockMap`] entries that no caller is currently
/// holding. Called periodically from the claim-expiry janitor so the
/// per-recipient insert-lock map does not grow forever with every
/// distinct recipient seen by the process.
pub fn sweep_recipient_locks(
    locks: &dashmap::DashMap<BareJid, std::sync::Arc<tokio::sync::Mutex<()>>>,
) -> usize {
    let mut removed = 0;
    locks.retain(|_, lock| {
        if std::sync::Arc::strong_count(lock) > 1 {
            true
        } else {
            removed += 1;
            false
        }
    });
    removed
}

/// libSQL/Postgres-backed [`PendingDeliveryStorage`] implementation.
///
/// Schema:
///
/// ```sql
/// CREATE TABLE pending_delivery (
///     row_id TEXT PRIMARY KEY,            -- UUID v7 — sortable for FIFO
///     recipient_jid TEXT NOT NULL,
///     original_receipt_at BIGINT NOT NULL, -- ms since unix epoch (i64;
///                                          -- SQLite collapses to INTEGER)
///     payload_kind TEXT NOT NULL,         -- 'archived' | 'transient'
///     archive_stanza_by TEXT,             -- bare jid stamping `<stanza-id/>`
///     archive_stanza_id TEXT,             -- XEP-0359 id (Archived rows)
///     transient_xml TEXT,                 -- serialized minidom (Transient)
///     flushed_in_session TEXT,
///     outbound_sequence INTEGER           -- XEP-0198 outbound counter,
///                                         -- stamped post-record_outbound
///                                         -- so SM `<a h>` can range-delete
///                                         -- (locked Q7b SM-ack lifecycle)
/// );
/// ```
///
/// `row_id` is a UUID v7 — sortable by time of generation, so an
/// `ORDER BY row_id` reproduces FIFO without driver-specific
/// auto-increment syntax (SQLite: `AUTOINCREMENT`; Postgres:
/// `BIGSERIAL`).
#[derive(Clone)]
pub struct DatabasePendingDeliveryStorage {
    db: Database,
    quota: QuotaPolicy,
    /// Per-recipient insert serialization to make
    /// `INSERT … SELECT … WHERE COUNT < cap` strict on Postgres
    /// (READ-COMMITTED snapshots can't see uncommitted concurrent
    /// inserts, so two writers can both pass the cap check). SQLite
    /// already serializes writers; this is portable defense.
    recipient_locks: std::sync::Arc<RecipientLockMap>,
    /// Present only when clustering is enabled and this database is
    /// co-located with the clustering claims table. Exact-fence methods use
    /// the caller-supplied immutable SM fence; they never acquire/rebind it.
    fencing: Option<PendingDeliveryFencing>,
}

/// Identity context needed to reject a fence from an obsolete node
/// incarnation before the transaction mutates pending delivery.
#[derive(Clone)]
struct PendingDeliveryFencing {
    node_identity: SharedNodeIdentity,
}

impl PendingDeliveryFencing {
    async fn assert_exact_sm_fence(
        &self,
        tx: &mut Transaction<'_>,
        stream_id: &str,
        fence: &SmClaimFence,
    ) -> Result<CurrentNodeIdentityGuard, PendingStorageError> {
        let entity = Entity::new(EntityType::SmSession, stream_id.to_string());
        let Some(identity_guard) = self.node_identity.guard_if_current(fence.owner()).await else {
            return Err(PendingStorageError::NotOwner { entity });
        };
        let entity_key = format!("{}:{stream_id}", EntityType::SmSession.as_db_str());
        let mut rows = tx
            .query(
                "SELECT 1 FROM clustering_claims \
                 WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ? \
                 FOR SHARE",
                crate::db_params![
                    entity_key,
                    fence.owner().node_id.clone(),
                    fence.owner().node_epoch.clone(),
                    fence.epoch().0,
                ],
            )
            .await
            .map_err(|error| PendingStorageError::Other(error.to_string()))?;
        let held = rows
            .next()
            .await
            .map_err(|error| PendingStorageError::Other(error.to_string()))?
            .is_some();
        if !held {
            return Err(PendingStorageError::NotOwner { entity });
        }
        Ok(identity_guard)
    }
}

fn required_sm_fence<'a>(
    stream_id: &SmSessionId,
    fence: Option<&'a SmClaimFence>,
) -> Result<&'a SmClaimFence, PendingStorageError> {
    fence.ok_or_else(|| PendingStorageError::NotOwner {
        entity: Entity::new(EntityType::SmSession, stream_id.as_str().to_string()),
    })
}

/// Column values for one `pending_delivery` INSERT — the shared output of
/// [`DatabasePendingDeliveryStorage::prepare_insert_row`], named rather
/// than a bare tuple (clippy `type_complexity`).
struct PreparedInsertRow {
    row_id: String,
    receipt_ms: i64,
    kind: &'static str,
    by: Option<String>,
    sid: Option<String>,
    xml: Option<String>,
}

impl DatabasePendingDeliveryStorage {
    /// Open a backing database (or in-memory fallback when no URL is
    /// supplied). Mirrors [`crate::inbox::DatabaseInboxStorage::open`].
    /// Never fenced — see [`open_for_cluster_mode`] for the entry point
    /// that attaches clustering fencing.
    pub async fn open(
        database_url: Option<&str>,
        quota: QuotaPolicy,
    ) -> Result<Self, PendingStorageError> {
        let db = match database_url {
            Some(url) => {
                let driver = if url.starts_with("postgres://") || url.starts_with("postgresql://") {
                    DatabaseDriver::Postgres
                } else {
                    DatabaseDriver::Sqlite
                };
                Database::from_config(
                    "pending_delivery",
                    &DatabaseConfig::new(driver, url.to_string()),
                )
                .await
                .map_err(|e| PendingStorageError::Other(e.to_string()))?
            }
            None => Database::in_memory("pending_delivery")
                .await
                .map_err(|e| PendingStorageError::Other(e.to_string()))?,
        };
        let storage = Self {
            db,
            quota,
            recipient_locks: std::sync::Arc::new(RecipientLockMap::new()),
            fencing: None,
        };
        schema::initialize(&storage).await?;
        info!(
            driver = ?storage.db.driver(),
            "pending_delivery storage initialized (XEP-0160)"
        );
        Ok(storage)
    }

    /// Shared row-shape extraction for fenced and unfenced inserts: the row
    /// id (freshly minted if the caller left it empty), the receipt
    /// timestamp in ms, and the payload-kind/by/stanza-id/xml column
    /// values. Factored out so the fenced and unfenced insert paths issue
    /// byte-identical SQL parameters, never two independently-maintained
    /// copies that could drift.
    fn prepare_insert_row(row: &PendingRow) -> Result<PreparedInsertRow, PendingStorageError> {
        let row_id = if row.id.as_str().is_empty() {
            PendingRowId::fresh().as_str().to_string()
        } else {
            row.id.as_str().to_string()
        };
        let receipt_ms = row.original_receipt_at.timestamp_millis();
        let (kind, by, sid, xml) = match &row.payload {
            PendingPayload::Archived(stanza_id) => (
                PAYLOAD_KIND_ARCHIVED,
                // The decode side parses `archive_stanza_by` as a `BareJid`
                // (XEP-0313 archives are scoped per bare JID, see
                // `MamArchiveResolver::resolve` which narrows via
                // `.to_bare()`). Narrow on write too so a `StanzaId.by` that
                // happens to carry a resource cannot poison a row that
                // `decode_row` would later refuse to parse.
                Some(stanza_id.by.to_bare().to_string()),
                Some(stanza_id.id.clone()),
                None,
            ),
            PendingPayload::Transient(message) => {
                let serialized = serialize_message(message)?;
                (PAYLOAD_KIND_TRANSIENT, None, None, Some(serialized))
            }
        };
        Ok(PreparedInsertRow {
            row_id,
            receipt_ms,
            kind,
            by,
            sid,
            xml,
        })
    }

    /// Attach clustering fencing for caller-supplied exact SM claim fences.
    /// Infallible — the co-location invariant is [`open_for_cluster_mode`]'s
    /// job, checked before this storage was ever opened; call this
    /// directly only if you have already verified co-location yourself.
    ///
    /// `#[cfg(feature = "clustering")]`: its sole caller
    /// ([`super::open_for_cluster_mode`]'s clustering-gated branch) is
    /// itself feature-gated, so this would otherwise be dead code on a
    /// build without the `clustering` Cargo feature.
    #[cfg(feature = "clustering")]
    pub(crate) fn with_cluster_fencing(mut self, node_identity: SharedNodeIdentity) -> Self {
        self.fencing = Some(PendingDeliveryFencing { node_identity });
        self
    }

    async fn execute(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<u64, PendingStorageError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        conn.execute(sql, params)
            .await
            .map_err(|e| PendingStorageError::Other(e.to_string()))
    }

    async fn query(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<crate::db::Rows, PendingStorageError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        conn.query(sql, params)
            .await
            .map_err(|e| PendingStorageError::Other(e.to_string()))
    }
}

#[async_trait]
impl PendingDeliveryStorage for DatabasePendingDeliveryStorage {
    #[instrument(skip(self, row), fields(recipient = %row.recipient))]
    async fn insert(&self, row: PendingRow) -> Result<InsertOutcome, PendingStorageError> {
        // Per-recipient lock to serialize concurrent inserts for the
        // same recipient. This makes the `INSERT … SELECT … WHERE
        // COUNT < cap` quota check strict on Postgres (where
        // READ-COMMITTED snapshots can otherwise let two concurrent
        // inserts both pass the cap and exceed `max_rows`). SQLite
        // serializes writers globally, so for SQLite this is
        // belt-and-suspenders.
        let recipient_lock = self
            .recipient_locks
            .entry(row.recipient.clone())
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _guard = recipient_lock.lock().await;

        let PreparedInsertRow {
            row_id,
            receipt_ms,
            kind,
            by,
            sid,
            xml,
        } = Self::prepare_insert_row(&row)?;
        // Atomic-with-quota INSERT: the WHERE clause runs in the same
        // SQL statement as the insert. Combined with the per-recipient
        // lock above this gives strict cap enforcement portably across
        // SQLite (single-writer) and Postgres (READ-COMMITTED).
        // Affected row count differentiates accepted (1) from
        // quota-rejected (0).
        let affected = match self.quota {
            QuotaPolicy::Unlimited => {
                self.execute(
                    "INSERT INTO pending_delivery (\
                        row_id, recipient_jid, original_receipt_at, payload_kind, \
                        archive_stanza_by, archive_stanza_id, transient_xml, \
                        flushed_in_session, outbound_sequence \
                     ) VALUES (?, ?, ?, ?, ?, ?, ?, NULL, NULL)",
                    crate::db_params![
                        row_id,
                        row.recipient.to_string(),
                        receipt_ms,
                        kind,
                        by,
                        sid,
                        xml,
                    ],
                )
                .await?
            }
            QuotaPolicy::CountCap { max_rows } => {
                self.execute(
                    "INSERT INTO pending_delivery (\
                        row_id, recipient_jid, original_receipt_at, payload_kind, \
                        archive_stanza_by, archive_stanza_id, transient_xml, \
                        flushed_in_session, outbound_sequence \
                     ) \
                     SELECT ?, ?, ?, ?, ?, ?, ?, NULL, NULL \
                     WHERE (SELECT COUNT(*) FROM pending_delivery WHERE recipient_jid = ?) < ?",
                    crate::db_params![
                        row_id,
                        row.recipient.to_string(),
                        receipt_ms,
                        kind,
                        by,
                        sid,
                        xml,
                        row.recipient.to_string(),
                        i64::from(max_rows),
                    ],
                )
                .await?
            }
        };
        if affected == 0 {
            Ok(InsertOutcome::QuotaExceeded)
        } else {
            Ok(InsertOutcome::Inserted)
        }
    }

    #[instrument(skip(self, row, fence), fields(recipient = %row.recipient, origin_stream_id = %origin_session))]
    async fn insert_under_sm_fence(
        &self,
        row: PendingRow,
        origin_session: &SmSessionId,
        fence: Option<&SmClaimFence>,
    ) -> Result<InsertOutcome, PendingStorageError> {
        let Some(fencing) = &self.fencing else {
            return self.insert(row).await;
        };
        let fence = required_sm_fence(origin_session, fence)?;

        let recipient_lock = self
            .recipient_locks
            .entry(row.recipient.clone())
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _guard = recipient_lock.lock().await;

        let PreparedInsertRow {
            row_id,
            receipt_ms,
            kind,
            by,
            sid,
            xml,
        } = Self::prepare_insert_row(&row)?;

        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;

        let identity_guard = fencing
            .assert_exact_sm_fence(&mut tx, origin_session.as_str(), fence)
            .await?;

        // Same two INSERT shapes as `insert`, issued on `tx` instead of a
        // pooled single-statement guard, so the fencing check and the
        // write commit or roll back together.
        let affected = match self.quota {
            QuotaPolicy::Unlimited => tx
                .execute(
                    "INSERT INTO pending_delivery (\
                        row_id, recipient_jid, original_receipt_at, payload_kind, \
                        archive_stanza_by, archive_stanza_id, transient_xml, \
                        flushed_in_session, outbound_sequence \
                     ) VALUES (?, ?, ?, ?, ?, ?, ?, NULL, NULL)",
                    crate::db_params![
                        row_id,
                        row.recipient.to_string(),
                        receipt_ms,
                        kind,
                        by,
                        sid,
                        xml,
                    ],
                )
                .await
                .map_err(|e| PendingStorageError::Other(e.to_string()))?,
            QuotaPolicy::CountCap { max_rows } => tx
                .execute(
                    "INSERT INTO pending_delivery (\
                        row_id, recipient_jid, original_receipt_at, payload_kind, \
                        archive_stanza_by, archive_stanza_id, transient_xml, \
                        flushed_in_session, outbound_sequence \
                     ) \
                     SELECT ?, ?, ?, ?, ?, ?, ?, NULL, NULL \
                     WHERE (SELECT COUNT(*) FROM pending_delivery WHERE recipient_jid = ?) < ?",
                    crate::db_params![
                        row_id,
                        row.recipient.to_string(),
                        receipt_ms,
                        kind,
                        by,
                        sid,
                        xml,
                        row.recipient.to_string(),
                        i64::from(max_rows),
                    ],
                )
                .await
                .map_err(|e| PendingStorageError::Other(e.to_string()))?,
        };

        tx.commit()
            .await
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        drop(identity_guard);

        if affected == 0 {
            Ok(InsertOutcome::QuotaExceeded)
        } else {
            Ok(InsertOutcome::Inserted)
        }
    }

    async fn list(&self, recipient: &BareJid) -> Result<Vec<PendingRow>, PendingStorageError> {
        let mut rows = self
            .query(
                "SELECT row_id, recipient_jid, original_receipt_at, payload_kind, \
                        archive_stanza_by, archive_stanza_id, transient_xml, \
                        flushed_in_session, outbound_sequence \
                 FROM pending_delivery \
                 WHERE recipient_jid = ? \
                 ORDER BY row_id ASC",
                crate::db_params![recipient.to_string()],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| PendingStorageError::Other(e.to_string()))?
        {
            out.push(decode_row(&row)?);
        }
        Ok(out)
    }

    async fn list_claimed_by_session(
        &self,
        session: &SmSessionId,
    ) -> Result<Vec<PendingRow>, PendingStorageError> {
        let mut rows = self
            .query(
                "SELECT row_id, recipient_jid, original_receipt_at, payload_kind, \
                        archive_stanza_by, archive_stanza_id, transient_xml, \
                        flushed_in_session, outbound_sequence \
                 FROM pending_delivery \
                 WHERE flushed_in_session = ? \
                 ORDER BY row_id ASC",
                crate::db_params![session.as_str().to_string()],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| PendingStorageError::Other(e.to_string()))?
        {
            out.push(decode_row(&row)?);
        }
        Ok(out)
    }

    async fn list_unclaimed_after(
        &self,
        recipient: &BareJid,
        after: Option<&PendingRowId>,
        limit: usize,
    ) -> Result<Vec<PendingRow>, PendingStorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut rows = if let Some(after) = after {
            self.query(
                "SELECT row_id, recipient_jid, original_receipt_at, payload_kind, \
                        archive_stanza_by, archive_stanza_id, transient_xml, \
                        flushed_in_session, outbound_sequence \
                 FROM pending_delivery \
                 WHERE recipient_jid = ? \
                   AND flushed_in_session IS NULL \
                   AND row_id > ? \
                 ORDER BY row_id ASC \
                 LIMIT ?",
                crate::db_params![
                    recipient.to_string(),
                    after.as_str().to_string(),
                    limit as i64,
                ],
            )
            .await?
        } else {
            self.query(
                "SELECT row_id, recipient_jid, original_receipt_at, payload_kind, \
                        archive_stanza_by, archive_stanza_id, transient_xml, \
                        flushed_in_session, outbound_sequence \
                 FROM pending_delivery \
                 WHERE recipient_jid = ? \
                   AND flushed_in_session IS NULL \
                 ORDER BY row_id ASC \
                 LIMIT ?",
                crate::db_params![recipient.to_string(), limit as i64],
            )
            .await?
        };
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| PendingStorageError::Other(e.to_string()))?
        {
            out.push(decode_row(&row)?);
        }
        Ok(out)
    }

    async fn list_unoutboxed_archived(
        &self,
        limit: usize,
    ) -> Result<Vec<PendingRow>, PendingStorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut rows = self
            .query(
                "SELECT row_id, recipient_jid, original_receipt_at, payload_kind, \
                        archive_stanza_by, archive_stanza_id, transient_xml, \
                        flushed_in_session, outbound_sequence \
                 FROM pending_delivery \
                 WHERE payload_kind = 'archived' \
                   AND flushed_in_session IS NULL \
                   AND notification_outboxed_at_ms IS NULL \
                 ORDER BY row_id ASC \
                 LIMIT ?",
                crate::db_params![limit as i64],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| PendingStorageError::Other(e.to_string()))?
        {
            out.push(decode_row(&row)?);
        }
        Ok(out)
    }

    async fn mark_notification_outboxed(
        &self,
        id: &PendingRowId,
    ) -> Result<u64, PendingStorageError> {
        self.execute(
            "UPDATE pending_delivery \
             SET notification_outboxed_at_ms = ? \
             WHERE row_id = ? AND notification_outboxed_at_ms IS NULL",
            crate::db_params![
                chrono::Utc::now().timestamp_millis(),
                id.as_str().to_string()
            ],
        )
        .await
    }

    async fn claim_for_session(
        &self,
        recipient: &BareJid,
        session: &SmSessionId,
    ) -> Result<Vec<PendingRow>, PendingStorageError> {
        // Atomic-ish: a single UPDATE … WHERE flushed_in_session IS NULL
        // tags every currently-unclaimed row, then a SELECT pulls the
        // newly-claimed set. Two concurrent calls for the same recipient
        // both run the UPDATE; whichever wins the row-level lock first
        // tags the rows, the loser's UPDATE finds zero matches and the
        // loser's SELECT returns rows already tagged for the other
        // session — filtered out by the WHERE.
        // Defensive: clear outbound_sequence on claim too. release_*
        // already does this, but a future code path that leaves a row
        // half-released should not be able to confuse the SM-ack
        // delete. Targets only newly-claimed rows (flushed_in_session
        // IS NULL filter ensures we don't trample another session's
        // ongoing claim).
        self.execute(
            "UPDATE pending_delivery SET flushed_in_session = ?, outbound_sequence = NULL, \
                                          claimed_at_ms = ? \
             WHERE recipient_jid = ? AND flushed_in_session IS NULL",
            crate::db_params![
                session.as_str().to_string(),
                chrono::Utc::now().timestamp_millis(),
                recipient.to_string()
            ],
        )
        .await?;
        // Return ONLY the rows this claim just tagged, identified by
        // `outbound_sequence IS NULL`. A row already pushed on this same
        // session in an earlier flush keeps `flushed_in_session = session`
        // AND a non-NULL `outbound_sequence` (the UPDATE above only touched
        // IS NULL rows), so filtering on the null sequence excludes it.
        // Without this, a re-flush of the same SM session (issue #1122:
        // `reset_offline_flush` re-opens the once-per-connection CAS after a
        // transient MAM error defers rows mid-batch) would re-select and
        // re-push an already-pushed-but-unacked row, duplicating delivery
        // and overwriting its `outbound_sequence`. This matches the
        // in-memory backend, which only returns freshly-claimed rows.
        let mut rows = self
            .query(
                "SELECT row_id, recipient_jid, original_receipt_at, payload_kind, \
                        archive_stanza_by, archive_stanza_id, transient_xml, \
                        flushed_in_session, outbound_sequence \
                 FROM pending_delivery \
                 WHERE recipient_jid = ? AND flushed_in_session = ? \
                   AND outbound_sequence IS NULL \
                 ORDER BY row_id ASC",
                crate::db_params![recipient.to_string(), session.as_str().to_string()],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| PendingStorageError::Other(e.to_string()))?
        {
            out.push(decode_row(&row)?);
        }
        Ok(out)
    }

    async fn claim_batch_for_session(
        &self,
        recipient: &BareJid,
        session: &SmSessionId,
        after: Option<&PendingRowId>,
        limit: usize,
    ) -> Result<Vec<PendingRow>, PendingStorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        // Claim ONLY the FIFO prefix: at most `limit` currently-unclaimed rows
        // whose row_id sorts after the cursor. The inner SELECT + LIMIT picks
        // the prefix; the UPDATE tags exactly those and `RETURNING` hands back
        // exactly the rows it transitioned.
        //
        // `RETURNING` (not a separate select-back) is load-bearing for
        // correctness (issue #1220 review). The recipient's connection task
        // stamps `outbound_sequence` asynchronously (see `record_pushed_at`),
        // so a select-back keyed on `(flushed_in_session, outbound_sequence IS
        // NULL)` would ALSO match rows a PRIOR flush pass on the same session
        // already pushed-but-not-yet-stamped — and re-deliver them on a
        // `reset_offline_flush` retry (transient MAM failure). `RETURNING`
        // scopes the result to this UPDATE's rows only, matching the
        // in-memory backend, which returns only rows it transitioned from
        // unclaimed.
        // The outer `flushed_in_session IS NULL` is load-bearing under
        // concurrent flushes of the SAME recipient (two resources of one user
        // recovering at once) on a READ COMMITTED backend (Postgres,
        // clustering). Two sessions can both evaluate the inner SELECT and see
        // the same unclaimed prefix; without the outer re-check, the loser's
        // UPDATE would still match those row_ids and overwrite the winner's
        // claim, and RETURNING would emit rows the other session already
        // claimed — a double delivery. Re-checking `flushed_in_session IS
        // NULL` at the outer level makes the loser's UPDATE a no-op for
        // already-claimed rows, so RETURNING yields only the rows THIS call
        // actually transitioned (first-caller-wins, matching the in-memory
        // mutex and the original single-shot `claim_for_session`). SQLite
        // serializes writers so it is safe there too. (Issue #1220 review.)
        let mut rows = match after {
            Some(after) => {
                self.query(
                    "UPDATE pending_delivery SET flushed_in_session = ?, outbound_sequence = NULL, \
                                                 claimed_at_ms = ? \
                     WHERE flushed_in_session IS NULL AND row_id IN ( \
                         SELECT row_id FROM pending_delivery \
                         WHERE recipient_jid = ? AND flushed_in_session IS NULL AND row_id > ? \
                         ORDER BY row_id ASC LIMIT ? \
                     ) \
                     RETURNING row_id, recipient_jid, original_receipt_at, payload_kind, \
                               archive_stanza_by, archive_stanza_id, transient_xml, \
                               flushed_in_session, outbound_sequence",
                    crate::db_params![
                        session.as_str().to_string(),
                        chrono::Utc::now().timestamp_millis(),
                        recipient.to_string(),
                        after.as_str().to_string(),
                        limit as i64,
                    ],
                )
                .await?
            }
            None => {
                self.query(
                    "UPDATE pending_delivery SET flushed_in_session = ?, outbound_sequence = NULL, \
                                                 claimed_at_ms = ? \
                     WHERE flushed_in_session IS NULL AND row_id IN ( \
                         SELECT row_id FROM pending_delivery \
                         WHERE recipient_jid = ? AND flushed_in_session IS NULL \
                         ORDER BY row_id ASC LIMIT ? \
                     ) \
                     RETURNING row_id, recipient_jid, original_receipt_at, payload_kind, \
                               archive_stanza_by, archive_stanza_id, transient_xml, \
                               flushed_in_session, outbound_sequence",
                    crate::db_params![
                        session.as_str().to_string(),
                        chrono::Utc::now().timestamp_millis(),
                        recipient.to_string(),
                        limit as i64,
                    ],
                )
                .await?
            }
        };
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| PendingStorageError::Other(e.to_string()))?
        {
            out.push(decode_row(&row)?);
        }
        // `RETURNING` row order is undefined; the flush loop needs FIFO
        // (row_id ASC — UUID v7, so lexical == chronological) both to preserve
        // XEP-0160 order of receipt and because it advances the batch cursor
        // from `batch.last()`.
        out.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        Ok(out)
    }

    async fn delete_claimed(&self, session: &SmSessionId) -> Result<u64, PendingStorageError> {
        self.execute(
            "DELETE FROM pending_delivery WHERE flushed_in_session = ?",
            crate::db_params![session.as_str().to_string()],
        )
        .await
    }

    async fn delete_row(&self, id: &PendingRowId) -> Result<u64, PendingStorageError> {
        self.execute(
            "DELETE FROM pending_delivery WHERE row_id = ?",
            crate::db_params![id.as_str().to_string()],
        )
        .await
    }

    async fn delete_row_if_session(
        &self,
        id: &PendingRowId,
        expected_session: &SmSessionId,
    ) -> Result<u64, PendingStorageError> {
        self.execute(
            "DELETE FROM pending_delivery \
             WHERE row_id = ? AND flushed_in_session = ?",
            crate::db_params![
                id.as_str().to_string(),
                expected_session.as_str().to_string(),
            ],
        )
        .await
    }

    async fn delete_row_under_sm_fence(
        &self,
        id: &PendingRowId,
        origin_stream_id: &SmSessionId,
        fence: Option<&SmClaimFence>,
    ) -> Result<u64, PendingStorageError> {
        let Some(fencing) = &self.fencing else {
            return self.delete_row_if_session(id, origin_stream_id).await;
        };
        let fence = required_sm_fence(origin_stream_id, fence)?;
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|error| PendingStorageError::Other(error.to_string()))?;
        let identity_guard = fencing
            .assert_exact_sm_fence(&mut tx, origin_stream_id.as_str(), fence)
            .await?;
        let removed = tx
            .execute(
                "DELETE FROM pending_delivery \
                 WHERE row_id = ? AND flushed_in_session = ?",
                crate::db_params![
                    id.as_str().to_string(),
                    origin_stream_id.as_str().to_string(),
                ],
            )
            .await
            .map_err(|error| PendingStorageError::Other(error.to_string()))?;
        tx.commit()
            .await
            .map_err(|error| PendingStorageError::Other(error.to_string()))?;
        drop(identity_guard);
        Ok(removed)
    }

    async fn release_claim(&self, session: &SmSessionId) -> Result<u64, PendingStorageError> {
        // Clear `outbound_sequence` alongside `flushed_in_session` so a
        // stale sequence from the dead session can't survive a re-claim
        // and trick a later session's SM ack into deleting an unack'd
        // row. (Qodo review on PR #358.)
        self.execute(
            "UPDATE pending_delivery SET flushed_in_session = NULL, \
                                          outbound_sequence = NULL, \
                                          claimed_at_ms = NULL \
             WHERE flushed_in_session = ?",
            crate::db_params![session.as_str().to_string()],
        )
        .await
    }

    async fn release_claim_under_sm_fence(
        &self,
        session: &SmSessionId,
        fence: Option<&SmClaimFence>,
    ) -> Result<u64, PendingStorageError> {
        let Some(fencing) = &self.fencing else {
            return self.release_claim(session).await;
        };
        let fence = required_sm_fence(session, fence)?;
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|error| PendingStorageError::Other(error.to_string()))?;
        let identity_guard = fencing
            .assert_exact_sm_fence(&mut tx, session.as_str(), fence)
            .await?;
        let released = tx
            .execute(
                "UPDATE pending_delivery SET flushed_in_session = NULL, \
                                              outbound_sequence = NULL, \
                                              claimed_at_ms = NULL \
                 WHERE flushed_in_session = ?",
                crate::db_params![session.as_str().to_string()],
            )
            .await
            .map_err(|error| PendingStorageError::Other(error.to_string()))?;
        tx.commit()
            .await
            .map_err(|error| PendingStorageError::Other(error.to_string()))?;
        drop(identity_guard);
        Ok(released)
    }

    fn requires_exact_sm_fence(&self) -> bool {
        self.fencing.is_some()
    }

    async fn release_row(&self, id: &PendingRowId) -> Result<u64, PendingStorageError> {
        self.execute(
            "UPDATE pending_delivery SET flushed_in_session = NULL, \
                                          outbound_sequence = NULL, \
                                          claimed_at_ms = NULL \
             WHERE row_id = ?",
            crate::db_params![id.as_str().to_string()],
        )
        .await
    }

    async fn release_row_if_session(
        &self,
        id: &PendingRowId,
        expected_session: &SmSessionId,
    ) -> Result<u64, PendingStorageError> {
        // Conditional release for the claim-expiry janitor. The
        // (row_id, session) snapshot returned by list_orphaned_claims can
        // be stale if a fresh bind re-claims the row before release.
        self.execute(
            "UPDATE pending_delivery SET flushed_in_session = NULL, \
                                          outbound_sequence = NULL, \
                                          claimed_at_ms = NULL \
             WHERE row_id = ? AND flushed_in_session = ?",
            crate::db_params![
                id.as_str().to_string(),
                expected_session.as_str().to_string()
            ],
        )
        .await
    }

    async fn record_pushed_at(
        &self,
        id: &PendingRowId,
        sequence: u32,
    ) -> Result<u64, PendingStorageError> {
        self.execute(
            "UPDATE pending_delivery SET outbound_sequence = ? WHERE row_id = ?",
            crate::db_params![i64::from(sequence), id.as_str().to_string()],
        )
        .await
    }

    async fn delete_acked_in_window(
        &self,
        session: &SmSessionId,
        from_exclusive: u32,
        to_inclusive: u32,
    ) -> Result<u64, PendingStorageError> {
        // SQL mirror of `waddle_xmpp::pending_delivery::sequence_in_ack_window`
        // (mod-2^32 interval `(from, to]`): plain `> from AND <= to` when
        // the window doesn't wrap, `> from OR <= to` when it spans the
        // u32 wrap. `outbound_sequence IS NOT NULL` keeps claimed-but-
        // unpushed rows untouched in both branches.
        let sql = if from_exclusive <= to_inclusive {
            "DELETE FROM pending_delivery \
             WHERE flushed_in_session = ? \
               AND outbound_sequence IS NOT NULL \
               AND outbound_sequence > ? \
               AND outbound_sequence <= ?"
        } else {
            "DELETE FROM pending_delivery \
             WHERE flushed_in_session = ? \
               AND outbound_sequence IS NOT NULL \
               AND (outbound_sequence > ? OR outbound_sequence <= ?)"
        };
        self.execute(
            sql,
            crate::db_params![
                session.as_str().to_string(),
                i64::from(from_exclusive),
                i64::from(to_inclusive)
            ],
        )
        .await
    }

    async fn list_orphaned_claims(
        &self,
        live_sessions: &[SmSessionId],
        claimed_before_ms: i64,
    ) -> Result<Vec<(PendingRowId, SmSessionId)>, PendingStorageError> {
        // SELECT every claimed row, then filter in-memory against the
        // live-set. This avoids generating a `WHERE flushed_in_session
        // NOT IN (?, ?, ?, …)` clause whose parameter count would be
        // unbounded for production deployments. The expected orphan
        // population is small (low hundreds) compared to live-session
        // count, so the filter cost is negligible.
        //
        // #1124 recency floor: claims stamped after `claimed_before_ms`
        // belong to in-flight flushes (a `transient:` non-SM flush is
        // never in the live-set) and are skipped in SQL. Rows with NO
        // stamp are skipped too — "recency unknown" (a pre-#1124
        // binary during a rolling deploy) must not be treated as old,
        // or the mid-flight release re-opens exactly during the
        // deploy. The janitor adopts unstamped claims first via
        // `stamp_unstamped_claims`, so they age into eligibility.
        let mut rows = self
            .query(
                "SELECT row_id, flushed_in_session FROM pending_delivery \
                 WHERE flushed_in_session IS NOT NULL \
                   AND claimed_at_ms IS NOT NULL \
                   AND claimed_at_ms <= ?",
                crate::db_params![claimed_before_ms],
            )
            .await?;
        let live: std::collections::HashSet<&str> =
            live_sessions.iter().map(SmSessionId::as_str).collect();
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| PendingStorageError::Other(e.to_string()))?
        {
            let row_id: String = row
                .get(0)
                .map_err(|e| PendingStorageError::Other(e.to_string()))?;
            let session: String = row
                .get(1)
                .map_err(|e| PendingStorageError::Other(e.to_string()))?;
            if !live.contains(session.as_str()) {
                out.push((PendingRowId::new(row_id), SmSessionId::new(session)));
            }
        }
        Ok(out)
    }

    async fn stamp_unstamped_claims(&self, now_ms: i64) -> Result<u64, PendingStorageError> {
        self.execute(
            "UPDATE pending_delivery SET claimed_at_ms = ? \
             WHERE flushed_in_session IS NOT NULL AND claimed_at_ms IS NULL",
            crate::db_params![now_ms],
        )
        .await
    }

    async fn count(&self, recipient: &BareJid) -> Result<u32, PendingStorageError> {
        let mut rows = self
            .query(
                "SELECT COUNT(*) FROM pending_delivery WHERE recipient_jid = ?",
                crate::db_params![recipient.to_string()],
            )
            .await?;
        let row = rows
            .next()
            .await
            .map_err(|e| PendingStorageError::Other(e.to_string()))?
            .ok_or_else(|| PendingStorageError::Other("COUNT(*) returned no row".into()))?;
        let count: i64 = row
            .get(0)
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        Ok(count.max(0) as u32)
    }

    async fn delete_older_than(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, PendingStorageError> {
        self.execute(
            "DELETE FROM pending_delivery WHERE original_receipt_at < ?",
            crate::db_params![cutoff.timestamp_millis()],
        )
        .await
    }

    async fn scrub_for_tombstone(
        &self,
        target: &waddle_xmpp::tombstone::TombstoneTarget,
    ) -> Result<u64, PendingStorageError> {
        // Archived pointers: exact (stanza-id, archive-by) match —
        // pure SQL. The MAM row was tombstoned, so the pointer must
        // not flush a stub for a message the recipient never saw.
        let mut removed = self
            .execute(
                "DELETE FROM pending_delivery \
                 WHERE payload_kind = ? \
                   AND archive_stanza_id = ? \
                   AND archive_stanza_by = ?",
                crate::db_params![
                    PAYLOAD_KIND_ARCHIVED,
                    target.id().to_string(),
                    target.archive_jid().to_string(),
                ],
            )
            .await?;
        // Transient rows carry inline XML — match in Rust with the
        // shared XEP-0424/0425 predicate, then delete by row_id.
        // COST NOTE: scans every transient row; scrubs are rare
        // (retraction / moderation only), so a full listing is
        // acceptable — mirrors the SM registry's durable sweep.
        let mut rows = self
            .query(
                "SELECT row_id, transient_xml FROM pending_delivery WHERE payload_kind = ?",
                crate::db_params![PAYLOAD_KIND_TRANSIENT],
            )
            .await?;
        let mut matched_ids: Vec<String> = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| PendingStorageError::Other(e.to_string()))?
        {
            let row_id: String = row
                .get(0)
                .map_err(|e| PendingStorageError::Other(e.to_string()))?;
            let transient_xml: Option<String> = row
                .get(1)
                .map_err(|e| PendingStorageError::Other(e.to_string()))?;
            let Some(xml) = transient_xml else {
                continue;
            };
            let Ok(element) = xml.parse::<xmpp_parsers::minidom::Element>() else {
                // Undecodable rows are skipped, matching the shared
                // matcher's parse-error semantics.
                continue;
            };
            if target.matches_message_element(&element) {
                matched_ids.push(row_id);
            }
        }
        for row_id in matched_ids {
            removed += self
                .execute(
                    "DELETE FROM pending_delivery WHERE row_id = ?",
                    crate::db_params![row_id],
                )
                .await?;
        }
        Ok(removed)
    }

    fn sweep_internal_bookkeeping(&self) -> usize {
        sweep_recipient_locks(&self.recipient_locks)
    }
}
