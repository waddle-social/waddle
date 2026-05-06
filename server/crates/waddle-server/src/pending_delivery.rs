//! XEP-0160 offline-message flush orchestration (issue #209,
//! waddle-server side).
//!
//! [`waddle_xmpp::pending_delivery::flush::build_replay_stanza`] is the
//! pure wire-shape builder. This module ties it to the live system:
//! it reads rows out of the [`PendingDeliveryStorage`], resolves
//! Archived rows against MAM, and pushes the replay stanzas to the
//! recovering resource via the [`ConnectionRegistry`].
//!
//! Locked design points consumed here:
//!
//! - **Q7a/Q7d** — caller (presence handler) gates this on the first
//!   non-negative-priority presence of a fresh session via
//!   [`ConnectionEntry::claim_offline_flush`].
//! - **Q7b** — SM-ack-keyed deletion. The flush no longer deletes
//!   rows on push; it tags each [`OutboundStanza`] with its source
//!   [`PendingRowId`] so the recipient's main loop can stamp the
//!   assigned XEP-0198 outbound counter via
//!   [`PendingDeliveryStorage::record_pushed_at`]. Rows are deleted
//!   only on SM `<a h>` ack via
//!   [`PendingDeliveryStorage::delete_acked_through`].
//! - **Q7c** — `claim_for_session` atomically tags rows with the
//!   recipient's resource so a concurrent presence from another
//!   resource sees an empty pool. On pre-ack session death the SM
//!   janitor / shutdown drain calls
//!   [`PendingDeliveryStorage::release_claim`] to restore the rows
//!   for re-flush by the next recovering resource.
//! - **Q5** — wire shape (`<delay/>` with original receipt time, server
//!   `from`, preserved `to`/extensions, no `<stanza-id/>` for Transient).

use std::sync::Arc;

use async_trait::async_trait;
use jid::{BareJid, FullJid};
use tracing::{debug, info, instrument, warn};
use waddle_xmpp::pending_delivery::flush::{build_replay_stanza, MaterializedPayload};
use waddle_xmpp::pending_delivery::storage::{PendingDeliveryStorage, PendingStorageError};
use waddle_xmpp::pending_delivery::{
    InsertOutcome, PendingPayload, PendingRow, PendingRowId, QuotaPolicy, SmSessionId,
};
use waddle_xmpp::protocol::event::{StanzaIdRef, StanzaIdValue};
use waddle_xmpp::registry::{ConnectionRegistry, SendResult};
use waddle_xmpp::Stanza;

use crate::db::{Database, DatabaseConfig, DatabaseDriver, IntoParams};

/// Outcome of a flush attempt for one resource.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FlushOutcome {
    /// Number of rows claimed from `pending_delivery`.
    pub claimed: u32,
    /// Number of replayed stanzas successfully pushed to the resource.
    pub pushed: u32,
    /// Number of rows the resolver could not materialize (Archived row
    /// whose MAM lookup is not available — happens when MAM storage is
    /// unwired in the test fixture, never in production).
    pub unresolved: u32,
}

/// Flush every currently-unclaimed `pending_delivery` row for the
/// given recipient to the given resource.
///
/// Called by the presence handler once `claim_offline_flush()` has
/// returned `true` on the recovering [`ConnectionEntry`] — i.e. the
/// first non-negative-priority presence of a fresh session.
///
/// The session-id used to claim rows is derived from the recovering
/// full JID (a concrete SM session id is wired in slice (d)).
#[instrument(skip(storage, registry, archive_resolver), fields(recipient = %recipient, resource = %resource))]
pub async fn flush_for_resource<R>(
    storage: &Arc<dyn PendingDeliveryStorage>,
    registry: &ConnectionRegistry,
    server_domain: &str,
    recipient: &BareJid,
    resource: &FullJid,
    archive_resolver: &R,
) -> FlushOutcome
where
    R: ArchiveResolver + ?Sized,
{
    let session_id = SmSessionId::new(resource.to_string());
    let claimed = match storage.claim_for_session(recipient, &session_id).await {
        Ok(rows) => rows,
        Err(error) => {
            warn!(error = %error, "claim_for_session failed; skipping flush");
            return FlushOutcome::default();
        }
    };
    let mut outcome = FlushOutcome {
        claimed: claimed.len() as u32,
        ..FlushOutcome::default()
    };
    if claimed.is_empty() {
        return outcome;
    }

    for row in claimed {
        let Some(payload) = materialize(&row, archive_resolver).await else {
            // Archived row whose MAM lookup failed — the original
            // stanza is unrecoverable. Drop the row instead of
            // releasing it so we don't loop forever on a poison pill.
            // The message is permanently lost from the recipient's
            // perspective; we surface it loudly so production logs
            // can flag MAM corruption / unexpected tombstones.
            outcome.unresolved += 1;
            if let Err(error) = storage.delete_row(&row.id).await {
                warn!(
                    row_id = %row.id,
                    error = %error,
                    "pending_delivery delete_row (unresolved poison pill) failed"
                );
            }
            continue;
        };
        let replay = build_replay_stanza(payload, server_domain, row.original_receipt_at);
        let stanza = Stanza::Message(replay);
        // Locked Q7b SM-ack lifecycle: do NOT delete on push. Tag the
        // outbound stanza with the source row id so the recipient's
        // main loop can bind the assigned XEP-0198 outbound counter
        // back to the row via `record_pushed_at`. The row is then
        // deleted only when an SM `<a h>` from the recovering session
        // covers `outbound_sequence` (handled in the SM ack path,
        // `delete_acked_through`).
        match registry
            .send_pending_flush(resource, stanza, row.id.clone())
            .await
        {
            SendResult::Sent => {
                outcome.pushed += 1;
                // Row stays claimed by `session_id` with
                // `outbound_sequence = NULL` until the recipient's
                // main loop stamps it via `record_pushed_at`. If the
                // session dies before push, `release_claim` clears
                // the claim for re-flush (Q7c).
            }
            other => {
                debug!(?other, row_id = %row.id, "send to recovering resource failed mid-flush");
                // Per-row release so an undelivered row stays eligible
                // for re-claim on the next flush trigger.
                if let Err(error) = storage.release_row(&row.id).await {
                    warn!(
                        row_id = %row.id,
                        error = %error,
                        "pending_delivery release_row (undelivered) failed"
                    );
                }
            }
        }
    }

    outcome
}

/// Resolves Archived `PendingRow` references against MAM.
///
/// Production wiring uses [`MamArchiveResolver`] over a real
/// [`waddle_xmpp::mam::storage::MamStorage`] handle. Tests use
/// [`NullArchiveResolver`] when only Transient rows are exercised.
#[async_trait::async_trait]
pub trait ArchiveResolver: Send + Sync {
    /// Read the archived stanza by recipient bare JID + typed XEP-0359
    /// stanza-id. Returns the typed [`xmpp_parsers::message::Message`]
    /// reconstructed from the MAM row; returns `None` on miss or any
    /// non-fatal lookup failure (the caller treats this as a poison
    /// pill and drops the `pending_delivery` row).
    async fn resolve(
        &self,
        archive_jid: &BareJid,
        stanza_id: &waddle_xmpp::protocol::event::StanzaIdValue,
    ) -> Option<xmpp_parsers::message::Message>;
}

/// MAM-backed resolver for production use.
pub struct MamArchiveResolver {
    pub mam_storage: Arc<dyn waddle_xmpp::mam::storage::MamStorage>,
}

#[async_trait::async_trait]
impl ArchiveResolver for MamArchiveResolver {
    async fn resolve(
        &self,
        archive_jid: &BareJid,
        stanza_id: &waddle_xmpp::protocol::event::StanzaIdValue,
    ) -> Option<xmpp_parsers::message::Message> {
        let archived = match self
            .mam_storage
            .get_message_by_archive_or_stanza_id(archive_jid, stanza_id.as_str())
            .await
        {
            Ok(Some(archived)) => archived,
            Ok(None) => return None,
            Err(error) => {
                warn!(
                    error = %error,
                    archive_jid = %archive_jid,
                    stanza_id = %stanza_id,
                    "MAM lookup failed during flush"
                );
                return None;
            }
        };
        // Parse the preserved wire XML back into a typed Message. The
        // archived row includes server-stamped <stanza-id> by recipient
        // bare, so the parsed Message already carries the XEP-0359
        // identifier required by locked Q5c.
        let stanza_xml = archived.stanza_xml.as_deref()?;
        let element: xmpp_parsers::minidom::Element = stanza_xml.parse().ok()?;
        xmpp_parsers::message::Message::try_from(element).ok()
    }
}

/// No-op resolver for tests that only exercise Transient rows.
#[derive(Debug, Default)]
pub struct NullArchiveResolver;

#[async_trait::async_trait]
impl ArchiveResolver for NullArchiveResolver {
    async fn resolve(
        &self,
        _archive_jid: &BareJid,
        _stanza_id: &waddle_xmpp::protocol::event::StanzaIdValue,
    ) -> Option<xmpp_parsers::message::Message> {
        None
    }
}

async fn materialize<R>(row: &PendingRow, resolver: &R) -> Option<MaterializedPayload>
where
    R: ArchiveResolver + ?Sized,
{
    match &row.payload {
        PendingPayload::Transient(_) => MaterializedPayload::from_transient(row),
        PendingPayload::Archived(stanza_id_ref) => {
            let archived = resolver
                .resolve(&stanza_id_ref.by, &stanza_id_ref.id)
                .await?;
            Some(MaterializedPayload::Archived(Box::new(archived)))
        }
    }
}

// ---------------------------------------------------------------------------
// Database-backed PendingDeliveryStorage (issue #209, slice (b) production
// backend).
// ---------------------------------------------------------------------------

const PAYLOAD_KIND_ARCHIVED: &str = "archived";
const PAYLOAD_KIND_TRANSIENT: &str = "transient";

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

/// libSQL/Postgres-backed [`PendingDeliveryStorage`] implementation.
///
/// Schema:
///
/// ```sql
/// CREATE TABLE pending_delivery (
///     row_id TEXT PRIMARY KEY,            -- UUID v7 — sortable for FIFO
///     recipient_jid TEXT NOT NULL,
///     original_receipt_at INTEGER NOT NULL, -- ms since unix epoch
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
}

impl DatabasePendingDeliveryStorage {
    /// Open a backing database (or in-memory fallback when no URL is
    /// supplied). Mirrors [`crate::inbox::DatabaseInboxStorage::open`].
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
        };
        storage.initialize().await?;
        info!(
            driver = ?storage.db.driver(),
            "pending_delivery storage initialized (XEP-0160)"
        );
        Ok(storage)
    }

    async fn initialize(&self) -> Result<(), PendingStorageError> {
        self.execute(
            r#"
            CREATE TABLE IF NOT EXISTS pending_delivery (
                row_id TEXT PRIMARY KEY,
                recipient_jid TEXT NOT NULL,
                original_receipt_at INTEGER NOT NULL,
                payload_kind TEXT NOT NULL,
                archive_stanza_by TEXT,
                archive_stanza_id TEXT,
                transient_xml TEXT,
                flushed_in_session TEXT,
                outbound_sequence INTEGER
            )
            "#,
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_pending_delivery_recipient \
             ON pending_delivery (recipient_jid, row_id)",
            (),
        )
        .await?;
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_pending_delivery_session \
             ON pending_delivery (flushed_in_session)",
            (),
        )
        .await?;
        // UNIQUE partial index on (recipient_jid, archive_stanza_id)
        // for Archived rows. XEP-0359 stanza-ids are unique per
        // archive (recipient bare JID); two pending_delivery rows
        // pointing at the same MAM entry would replay the same
        // message twice. Both SQLite (since 3.8.0) and Postgres
        // support partial indexes; the WHERE clause limits the
        // constraint to Archived rows so multiple Transient inserts
        // for the same recipient remain allowed (the typed
        // PendingPayload::Transient variant has no archive id to
        // collide on).
        self.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_pending_delivery_archived_unique \
             ON pending_delivery (recipient_jid, archive_stanza_id) \
             WHERE payload_kind = 'archived'",
            (),
        )
        .await?;
        Ok(())
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

    fn decode_row(row: &crate::db::Row) -> Result<PendingRow, PendingStorageError> {
        let row_id: String = row
            .get(0)
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let id = PendingRowId::new(row_id);
        let recipient_jid: String = row
            .get(1)
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let recipient: BareJid = recipient_jid
            .parse()
            .map_err(|e: jid::Error| PendingStorageError::Other(e.to_string()))?;
        let original_receipt_at_ms: i64 = row
            .get(2)
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let original_receipt_at =
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(original_receipt_at_ms)
                .ok_or_else(|| PendingStorageError::Other("invalid receipt timestamp".into()))?;
        let payload_kind: String = row
            .get(3)
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let archive_stanza_by: Option<String> = row
            .get(4)
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let archive_stanza_id: Option<String> = row
            .get(5)
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let transient_xml: Option<String> = row
            .get(6)
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let flushed_in_session: Option<String> = row
            .get(7)
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let outbound_sequence_i64: Option<i64> = row
            .get(8)
            .map_err(|e| PendingStorageError::Other(e.to_string()))?;
        let outbound_sequence = outbound_sequence_i64
            .map(|v| u32::try_from(v).map_err(|e| PendingStorageError::Other(e.to_string())))
            .transpose()?;

        let payload = match payload_kind.as_str() {
            PAYLOAD_KIND_ARCHIVED => {
                let by_str = archive_stanza_by.ok_or_else(|| {
                    PendingStorageError::Other("archived row missing archive_stanza_by".into())
                })?;
                let by: BareJid = by_str
                    .parse()
                    .map_err(|e: jid::Error| PendingStorageError::Other(e.to_string()))?;
                let id_str = archive_stanza_id.ok_or_else(|| {
                    PendingStorageError::Other("archived row missing archive_stanza_id".into())
                })?;
                PendingPayload::Archived(StanzaIdRef {
                    by,
                    id: StanzaIdValue::new(id_str),
                })
            }
            PAYLOAD_KIND_TRANSIENT => {
                let xml = transient_xml.ok_or_else(|| {
                    PendingStorageError::Other("transient row missing transient_xml".into())
                })?;
                let element: xmpp_parsers::minidom::Element =
                    xml.parse().map_err(|e: xmpp_parsers::minidom::Error| {
                        PendingStorageError::Other(e.to_string())
                    })?;
                let message = xmpp_parsers::message::Message::try_from(element)
                    .map_err(|e| PendingStorageError::Other(e.to_string()))?;
                PendingPayload::Transient(Box::new(message))
            }
            other => {
                return Err(PendingStorageError::Other(format!(
                    "unknown payload_kind '{other}'"
                )))
            }
        };
        Ok(PendingRow {
            id,
            recipient,
            original_receipt_at,
            payload,
            flushed_in_session: flushed_in_session.map(SmSessionId::new),
            outbound_sequence,
        })
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

        let row_id = if row.id.as_str().is_empty() {
            PendingRowId::fresh().as_str().to_string()
        } else {
            row.id.as_str().to_string()
        };
        let receipt_ms = row.original_receipt_at.timestamp_millis();
        let (kind, by, sid, xml) = match &row.payload {
            PendingPayload::Archived(stanza_id_ref) => (
                PAYLOAD_KIND_ARCHIVED,
                Some(stanza_id_ref.by.to_string()),
                Some(stanza_id_ref.id.as_str().to_string()),
                None,
            ),
            PendingPayload::Transient(message) => {
                let serialized = serialize_message(message)?;
                (PAYLOAD_KIND_TRANSIENT, None, None, Some(serialized))
            }
        };
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
            out.push(Self::decode_row(&row)?);
        }
        Ok(out)
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
        self.execute(
            "UPDATE pending_delivery SET flushed_in_session = ? \
             WHERE recipient_jid = ? AND flushed_in_session IS NULL",
            crate::db_params![session.as_str().to_string(), recipient.to_string()],
        )
        .await?;
        let mut rows = self
            .query(
                "SELECT row_id, recipient_jid, original_receipt_at, payload_kind, \
                        archive_stanza_by, archive_stanza_id, transient_xml, \
                        flushed_in_session, outbound_sequence \
                 FROM pending_delivery \
                 WHERE recipient_jid = ? AND flushed_in_session = ? \
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
            out.push(Self::decode_row(&row)?);
        }
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

    async fn release_claim(&self, session: &SmSessionId) -> Result<u64, PendingStorageError> {
        self.execute(
            "UPDATE pending_delivery SET flushed_in_session = NULL \
             WHERE flushed_in_session = ?",
            crate::db_params![session.as_str().to_string()],
        )
        .await
    }

    async fn release_row(&self, id: &PendingRowId) -> Result<u64, PendingStorageError> {
        self.execute(
            "UPDATE pending_delivery SET flushed_in_session = NULL WHERE row_id = ?",
            crate::db_params![id.as_str().to_string()],
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

    async fn delete_acked_through(
        &self,
        session: &SmSessionId,
        sequence_max: u32,
    ) -> Result<u64, PendingStorageError> {
        self.execute(
            "DELETE FROM pending_delivery \
             WHERE flushed_in_session = ? \
               AND outbound_sequence IS NOT NULL \
               AND outbound_sequence <= ?",
            crate::db_params![session.as_str().to_string(), i64::from(sequence_max)],
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
}

fn serialize_message(
    message: &xmpp_parsers::message::Message,
) -> Result<String, PendingStorageError> {
    let element = xmpp_parsers::minidom::Element::from(message.clone());
    let mut buf = Vec::new();
    element
        .write_to(&mut buf)
        .map_err(|e| PendingStorageError::Other(e.to_string()))?;
    String::from_utf8(buf).map_err(|e| PendingStorageError::Other(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage;
    use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow};
    use xmpp_parsers::message::{Body, Message, MessageType};

    fn bare(s: &str) -> BareJid {
        s.parse().expect("bare jid")
    }

    fn full(s: &str) -> FullJid {
        s.parse().expect("full jid")
    }

    fn transient_row(recipient: &str, body: &str) -> PendingRow {
        let mut m = Message::new(Some(recipient.parse::<jid::Jid>().expect("jid")));
        m.from = Some("bob@elsewhere/x".parse::<jid::Jid>().expect("jid"));
        m.type_ = MessageType::Chat;
        m.bodies.insert(String::new(), Body(body.to_string()));
        PendingRow {
            id: PendingRowId::fresh(),
            recipient: bare(recipient),
            original_receipt_at: Utc::now(),
            payload: PendingPayload::Transient(Box::new(m)),
            flushed_in_session: None,
            outbound_sequence: None,
        }
    }

    #[tokio::test]
    async fn flush_with_no_rows_is_noop() {
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        let registry = ConnectionRegistry::new();
        let outcome = flush_for_resource(
            &storage,
            &registry,
            "example.com",
            &bare("alice@example.com"),
            &full("alice@example.com/web"),
            &NullArchiveResolver,
        )
        .await;
        assert_eq!(outcome, FlushOutcome::default());
    }

    #[tokio::test]
    async fn flush_pushes_transient_rows_and_deletes_on_success() {
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        // Insert two transient rows.
        for body in ["one", "two"] {
            storage
                .insert(transient_row("alice@example.com", body))
                .await
                .unwrap();
        }

        // Wire a registered connection so send_to actually has a sink.
        let registry = ConnectionRegistry::new();
        let resource = full("alice@example.com/web");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        registry.register(resource.clone(), tx);

        let outcome = flush_for_resource(
            &storage,
            &registry,
            "example.com",
            &bare("alice@example.com"),
            &resource,
            &NullArchiveResolver,
        )
        .await;
        assert_eq!(outcome.claimed, 2);
        assert_eq!(outcome.pushed, 2);
        assert_eq!(outcome.unresolved, 0);

        // Both messages were sent on the wire.
        let mut received = Vec::new();
        while let Ok(stanza) = rx.try_recv() {
            received.push(stanza);
        }
        assert_eq!(received.len(), 2);

        // Locked Q7b SM-ack lifecycle (issue #209 PR #347): rows
        // stay in storage tagged `flushed_in_session` after push;
        // deletion happens on SM `<a h>` ack via
        // `delete_acked_through`, NOT on send. Verify the rows are
        // claimed by the resource session and have NOT been deleted
        // by the flush itself.
        assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 2);
        let listed = storage.list(&bare("alice@example.com")).await.unwrap();
        for row in &listed {
            assert!(
                row.flushed_in_session.is_some(),
                "row stays claimed by recovering session until SM-ack"
            );
        }
        // Each pushed OutboundStanza carries the row id so the
        // recipient's main loop can stamp `outbound_sequence` after
        // its `record_outbound`. The pending stanzas in `received`
        // correspond to the claimed rows.
        let row_ids: std::collections::HashSet<_> = received
            .iter()
            .filter_map(|o| o.pending_row_id.clone())
            .collect();
        assert_eq!(
            row_ids.len(),
            2,
            "every flush stanza carries its pending_row_id"
        );
    }

    #[tokio::test]
    async fn flush_releases_rows_when_no_push_succeeds() {
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        storage
            .insert(transient_row("alice@example.com", "hi"))
            .await
            .unwrap();

        // No connection registered → send_to returns NotConnected.
        let registry = ConnectionRegistry::new();
        let resource = full("alice@example.com/web");

        let outcome = flush_for_resource(
            &storage,
            &registry,
            "example.com",
            &bare("alice@example.com"),
            &resource,
            &NullArchiveResolver,
        )
        .await;
        assert_eq!(outcome.claimed, 1);
        assert_eq!(outcome.pushed, 0);
        // Row stays in storage but with flushed_in_session cleared so
        // a later flush can retry.
        let rows = storage.list(&bare("alice@example.com")).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].flushed_in_session.is_none());
    }

    // ── DatabasePendingDeliveryStorage integration tests ────────────────

    #[tokio::test]
    async fn db_storage_round_trips_archived_and_transient_rows() {
        let storage = DatabasePendingDeliveryStorage::open(None, QuotaPolicy::Unlimited)
            .await
            .expect("open in-memory storage");
        // Insert one Archived + one Transient
        let recipient = bare("alice@example.com");
        let archived = PendingRow {
            id: PendingRowId::fresh(),
            recipient: recipient.clone(),
            original_receipt_at: chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
                1_700_000_000_000,
            )
            .unwrap(),
            payload: PendingPayload::Archived(StanzaIdRef {
                by: recipient.clone(),
                id: StanzaIdValue::new("mam-id"),
            }),
            flushed_in_session: None,
            outbound_sequence: None,
        };
        let trans = transient_row("alice@example.com", "transient body");
        assert_eq!(
            storage.insert(archived).await.unwrap(),
            InsertOutcome::Inserted
        );
        assert_eq!(
            storage.insert(trans).await.unwrap(),
            InsertOutcome::Inserted
        );

        let rows = storage.list(&recipient).await.unwrap();
        assert_eq!(rows.len(), 2);
        // FIFO: archived inserted first.
        assert!(rows[0].payload.is_archived());
        assert!(rows[1].payload.is_transient());
    }

    #[tokio::test]
    async fn db_storage_quota_returns_quota_exceeded_outcome() {
        let storage =
            DatabasePendingDeliveryStorage::open(None, QuotaPolicy::CountCap { max_rows: 2 })
                .await
                .unwrap();
        let recipient = bare("alice@example.com");
        for n in 0..2 {
            assert_eq!(
                storage
                    .insert(transient_row("alice@example.com", &format!("body-{n}"),))
                    .await
                    .unwrap(),
                InsertOutcome::Inserted
            );
        }
        assert_eq!(
            storage
                .insert(transient_row("alice@example.com", "overflow"))
                .await
                .unwrap(),
            InsertOutcome::QuotaExceeded
        );
        assert_eq!(storage.count(&recipient).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn db_storage_claim_release_delete_lifecycle() {
        let storage = DatabasePendingDeliveryStorage::open(None, QuotaPolicy::Unlimited)
            .await
            .unwrap();
        let recipient = bare("alice@example.com");
        for n in 0..3 {
            storage
                .insert(transient_row("alice@example.com", &format!("body-{n}")))
                .await
                .unwrap();
        }

        let session1 = SmSessionId::new("session-1");
        let claimed1 = storage
            .claim_for_session(&recipient, &session1)
            .await
            .unwrap();
        assert_eq!(claimed1.len(), 3);
        // Concurrent claim by another session sees no unclaimed rows.
        let session2 = SmSessionId::new("session-2");
        let claimed2 = storage
            .claim_for_session(&recipient, &session2)
            .await
            .unwrap();
        assert_eq!(claimed2.len(), 0);

        // Release session1's claim → rows become available for session2.
        let released = storage.release_claim(&session1).await.unwrap();
        assert_eq!(released, 3);
        let claimed2 = storage
            .claim_for_session(&recipient, &session2)
            .await
            .unwrap();
        assert_eq!(claimed2.len(), 3);

        // Delete on SM-ack of session2's flush stanzas.
        let removed = storage.delete_claimed(&session2).await.unwrap();
        assert_eq!(removed, 3);
        assert_eq!(storage.count(&recipient).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn pending_row_deleted_only_after_sm_ack() {
        // Q7b end-to-end (issue #209 PR #347):
        // 1. Insert a transient row.
        // 2. Flush to a registered resource — row is claimed + pushed
        //    (OutboundStanza in the channel) but stays in storage.
        // 3. Simulate the recipient main loop's `record_pushed_at`
        //    after `record_outbound`.
        // 4. Simulate an SM `<a h>` ack via `delete_acked_through`.
        // 5. Verify the row is now gone.
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        storage
            .insert(transient_row("alice@example.com", "hi"))
            .await
            .unwrap();

        let registry = ConnectionRegistry::new();
        let resource = full("alice@example.com/web");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        registry.register(resource.clone(), tx);

        let outcome = flush_for_resource(
            &storage,
            &registry,
            "example.com",
            &bare("alice@example.com"),
            &resource,
            &NullArchiveResolver,
        )
        .await;
        assert_eq!(outcome.pushed, 1);
        // Row stays in storage post-flush, claimed by this session.
        assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 1);
        let pushed = rx.try_recv().expect("flush stanza pushed to channel");
        let row_id = pushed
            .pending_row_id
            .clone()
            .expect("flush stanza carries source row id");

        // Recipient main loop simulation: after `record_outbound`
        // assigns SM outbound counter (say h=7), bind it to the row.
        storage.record_pushed_at(&row_id, 7).await.unwrap();

        // Pre-ack: row is still there.
        assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 1);

        // Pre-ack with h=6 (covers earlier stanzas, not this one).
        let session_id = waddle_xmpp::pending_delivery::SmSessionId::new(resource.to_string());
        let removed = storage.delete_acked_through(&session_id, 6).await.unwrap();
        assert_eq!(removed, 0, "ack(h=6) does not cover h=7 row");
        assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 1);

        // SM ack arrives covering h=7.
        let removed = storage.delete_acked_through(&session_id, 7).await.unwrap();
        assert_eq!(removed, 1);
        assert_eq!(
            storage.count(&bare("alice@example.com")).await.unwrap(),
            0,
            "row deleted only after SM-ack (locked Q7b)"
        );
    }

    #[tokio::test]
    async fn pending_row_released_on_pre_ack_session_death() {
        // Q7c end-to-end (issue #209 PR #347):
        // 1. Insert a row + flush via session-A.
        // 2. Stamp it with outbound_sequence (push happened).
        // 3. Session-A dies BEFORE the recipient's SM `<a h>` ack
        //    arrives (e.g. socket dropped). `release_claim(session_A)`
        //    is called by the SM janitor / shutdown drain.
        // 4. A second resource (session-B) recovers and re-claims —
        //    the released row must be eligible.
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        storage
            .insert(transient_row("alice@example.com", "hi"))
            .await
            .unwrap();

        let registry = ConnectionRegistry::new();
        let resource_a = full("alice@example.com/laptop");
        let (tx_a, mut rx_a) = tokio::sync::mpsc::channel(8);
        registry.register(resource_a.clone(), tx_a);

        let outcome = flush_for_resource(
            &storage,
            &registry,
            "example.com",
            &bare("alice@example.com"),
            &resource_a,
            &NullArchiveResolver,
        )
        .await;
        assert_eq!(outcome.pushed, 1);
        let pushed = rx_a.try_recv().expect("flush stanza pushed");
        let row_id = pushed.pending_row_id.clone().unwrap();
        // Recipient stamped sequence, but no SM-ack arrives.
        storage.record_pushed_at(&row_id, 3).await.unwrap();

        // Session-A dies pre-ack — the SM janitor's release_claim
        // restores the row to the unclaimed pool.
        let session_a = waddle_xmpp::pending_delivery::SmSessionId::new(resource_a.to_string());
        let released = storage.release_claim(&session_a).await.unwrap();
        assert_eq!(released, 1);

        // Second resource comes online and claims for itself.
        let resource_b = full("alice@example.com/web");
        let (tx_b, mut rx_b) = tokio::sync::mpsc::channel(8);
        registry.register(resource_b.clone(), tx_b);
        let outcome = flush_for_resource(
            &storage,
            &registry,
            "example.com",
            &bare("alice@example.com"),
            &resource_b,
            &NullArchiveResolver,
        )
        .await;
        assert_eq!(outcome.pushed, 1, "row re-flushed to recovering resource-B");
        let pushed_b = rx_b.try_recv().expect("flush stanza pushed to resource-B");
        assert_eq!(pushed_b.pending_row_id.unwrap(), row_id, "same row");
    }
}
