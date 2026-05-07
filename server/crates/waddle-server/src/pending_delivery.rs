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
use waddle_xmpp::registry::{ConnectionRegistry, SendResult};
use waddle_xmpp::Stanza;
use waddle_xmpp_core::xep0359::StanzaId;

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
    /// Number of rows dropped because the recipient blocked the sender
    /// AFTER the row was inserted (XEP-0191 §2 step 4 flush-time
    /// re-evaluation, issue #209 PR #360). Blocked rows are deleted
    /// from `pending_delivery` since the block is final until lifted.
    pub dropped_blocked: u32,
}

/// Per-flush context bundling the optional / contextual parameters
/// of [`flush_for_resource`]. Carved out of the function signature so
/// adding a new dependency (e.g. a future XEP-0411 hook) doesn't push
/// the parameter count over the clippy threshold and tempt a
/// suppression. Project hard rule (`server/CLAUDE.md`): never add
/// `#[allow(clippy::too_many_arguments)]` (Greptile/Qodo review on
/// PR #360).
pub struct FlushContext<'a, R>
where
    R: ArchiveResolver + ?Sized,
{
    /// JID-form domain stamped onto the `<delay/>` element added to
    /// each replayed stanza per XEP-0203 §4.1.
    pub server_domain: &'a str,
    /// Recovering connection's XEP-0198 stream id when SM is enabled.
    /// `None` falls back to the delete-on-push path (no ack will fire).
    pub sm_session: Option<&'a SmSessionId>,
    /// Live blocking storage for XEP-0191 §2 step 4 flush-time
    /// re-evaluation. `None` skips the check (test fixtures only;
    /// production always wires the real backend).
    pub blocking_storage: Option<&'a Arc<dyn waddle_xmpp::xep::xep0191::BlockingStorage>>,
    /// Resolves Archived `PendingRow` references against MAM.
    pub archive_resolver: &'a R,
}

/// Flush every currently-unclaimed `pending_delivery` row for the
/// given recipient to the given resource.
///
/// Called by the presence handler once `claim_offline_flush()` has
/// returned `true` on the recovering [`ConnectionEntry`] — i.e. the
/// first non-negative-priority presence of a fresh session.
///
/// `ctx` carries the optional / contextual parameters
/// (`sm_session`, `blocking_storage`, `archive_resolver`,
/// `server_domain`) — see [`FlushContext`] for details.
#[instrument(skip(storage, registry, ctx), fields(recipient = %recipient, resource = %resource))]
pub async fn flush_for_resource<R>(
    storage: &Arc<dyn PendingDeliveryStorage>,
    registry: &ConnectionRegistry,
    recipient: &BareJid,
    resource: &FullJid,
    ctx: FlushContext<'_, R>,
) -> FlushOutcome
where
    R: ArchiveResolver + ?Sized,
{
    let FlushContext {
        server_domain,
        sm_session,
        blocking_storage,
        archive_resolver,
    } = ctx;
    // Snapshot the recipient's current blocklist once for the whole
    // flush batch. XEP-0191 §2 step 4: if the recipient blocked the
    // sender AFTER the row was queued, the row must be dropped.
    // Per-batch (not per-row) to avoid hammering the blocking-storage
    // backend; correctness window is the duration of one flush
    // (typically << 1 s). Same fail-closed policy as
    // `interpret.rs::offline_recipient_pass_blocklist_storage_error_skips_recipient_persistence`:
    // on storage error, abort the flush rather than degrade to an
    // empty blocklist (which would let blocked senders through).
    let blocklist: Option<std::collections::HashSet<jid::BareJid>> = match blocking_storage {
        Some(bs) => match bs.list_blocked_jids(recipient).await {
            Ok(jids) => Some(jids.into_iter().collect()),
            Err(error) => {
                warn!(
                    error = %error,
                    "blocklist load failed; aborting flush to preserve fail-closed XEP-0191 policy"
                );
                return FlushOutcome::default();
            }
        },
        None => None,
    };
    // For non-SM sessions, use a transient per-flush session id so the
    // claim row tag is consistent within the batch. The post-push
    // delete path keys on row id, not session id, so the transient
    // value never escapes this function. Only the SM path keeps the
    // claim alive past the push for the SM-ack lifecycle.
    let transient_session_id;
    let session_id_for_claim: &SmSessionId = match sm_session {
        Some(id) => id,
        None => {
            transient_session_id =
                SmSessionId::new(format!("transient:{}:{}", resource, uuid::Uuid::new_v4()));
            &transient_session_id
        }
    };
    let claimed = match storage
        .claim_for_session(recipient, session_id_for_claim)
        .await
    {
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
            waddle_xmpp::prometheus::increment_pending_delivery_unresolved_poison_pill();
            if let Err(error) = storage.delete_row(&row.id).await {
                warn!(
                    row_id = %row.id,
                    error = %error,
                    "pending_delivery delete_row (unresolved poison pill) failed"
                );
            }
            continue;
        };
        // XEP-0191 §2 step 4 flush-time block re-evaluation
        // (issue #209 PR #360): if the recipient blocked the sender
        // after the row was queued, drop it. Block is final until
        // the recipient lifts it — `delete_row` not `release_row`.
        if let Some(blocked) = blocklist.as_ref() {
            let sender_bare = sender_bare_for_payload(&payload);
            if let Some(sender) = sender_bare {
                if blocked.contains(&sender) {
                    debug!(
                        row_id = %row.id,
                        recipient = %recipient,
                        sender = %sender,
                        "pending_delivery flush dropping row: recipient blocked sender post-intake (XEP-0191 §2 step 4)"
                    );
                    outcome.dropped_blocked += 1;
                    if let Err(error) = storage.delete_row(&row.id).await {
                        // Copilot review on PR #360: without a release
                        // here, the row would stay tagged with the
                        // current (still-live) SM session id.
                        // Consequence: the SM-expiry janitor wouldn't
                        // see it as orphaned (its session is alive),
                        // the SM ack wouldn't delete it
                        // (`outbound_sequence` is NULL — never pushed),
                        // and the next flush wouldn't re-claim it
                        // (`flushed_in_session` not NULL). The row
                        // would wedge permanently and consume quota.
                        // Fall back to `release_row` so the next
                        // recovering resource (or this same session
                        // on a later presence transition) can re-claim
                        // it and re-check the blocklist.
                        warn!(
                            row_id = %row.id,
                            error = %error,
                            "pending_delivery delete_row (blocked at flush) failed; \
                             releasing claim so the next flush can re-check the blocklist"
                        );
                        if let Err(release_error) = storage.release_row(&row.id).await {
                            warn!(
                                row_id = %row.id,
                                error = %release_error,
                                "pending_delivery release_row (blocked-at-flush fallback) \
                                 also failed; row may remain wedged until claim-expiry janitor \
                                 sees the session expire"
                            );
                        }
                    }
                    continue;
                }
            }
        }
        let replay = build_replay_stanza(payload, server_domain, row.original_receipt_at);
        let stanza = Stanza::Message(replay);
        // SM-enabled path: tag outbound with row id so the recipient's
        // main loop can stamp `outbound_sequence` post-`record_outbound`.
        // The row stays claimed for the SM-ack lifecycle.
        // Non-SM path: same outbound tag (cheap), but we delete on Sent
        // because there's no SM session to ack against.
        let push_result = if sm_session.is_some() {
            registry
                .send_pending_flush(resource, stanza, row.id.clone(), row.original_receipt_at)
                .await
        } else {
            registry.send_to(resource, stanza).await
        };
        match push_result {
            SendResult::Sent => {
                outcome.pushed += 1;
                if sm_session.is_none() {
                    // Non-SM fallback: delete on push since no `<a h>`
                    // will ever fire (Codex review on PR #358).
                    if let Err(error) = storage.delete_row(&row.id).await {
                        warn!(
                            row_id = %row.id,
                            error = %error,
                            "pending_delivery delete_row (non-SM push) failed; \
                             row may re-deliver on next presence"
                        );
                    }
                }
                // SM-enabled: row stays claimed by `sm_session` with
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
    /// Read the archived stanza by canonical XEP-0359 [`StanzaId`]
    /// (`{ id, by }`). Returns the typed
    /// [`xmpp_parsers::message::Message`] reconstructed from the MAM
    /// row; returns `None` on miss or any non-fatal lookup failure
    /// (the caller treats this as a poison pill and drops the
    /// `pending_delivery` row).
    async fn resolve(&self, stanza_id: &StanzaId) -> Option<xmpp_parsers::message::Message>;
}

/// MAM-backed resolver for production use.
pub struct MamArchiveResolver {
    pub mam_storage: Arc<dyn waddle_xmpp::mam::storage::MamStorage>,
}

#[async_trait::async_trait]
impl ArchiveResolver for MamArchiveResolver {
    async fn resolve(&self, stanza_id: &StanzaId) -> Option<xmpp_parsers::message::Message> {
        // MAM lookup keys on the archive's *bare* JID — XEP-0313 §5
        // archives are per-user / per-room (BareJid), and the canonical
        // `StanzaId.by` Jid carries that information (the MAM writer
        // always stamps with a bare-form Jid).
        let archive_bare = stanza_id.by.to_bare();
        let archived = match self
            .mam_storage
            .get_message_by_archive_or_stanza_id(&archive_bare, stanza_id.as_str())
            .await
        {
            Ok(Some(archived)) => archived,
            Ok(None) => return None,
            Err(error) => {
                warn!(
                    error = %error,
                    archive_jid = %archive_bare,
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
    async fn resolve(&self, _stanza_id: &StanzaId) -> Option<xmpp_parsers::message::Message> {
        None
    }
}

async fn materialize<R>(row: &PendingRow, resolver: &R) -> Option<MaterializedPayload>
where
    R: ArchiveResolver + ?Sized,
{
    match &row.payload {
        PendingPayload::Transient(_) => MaterializedPayload::from_transient(row),
        PendingPayload::Archived(stanza_id) => {
            let archived = resolver.resolve(stanza_id).await?;
            Some(MaterializedPayload::Archived(Box::new(archived)))
        }
    }
}

/// Extract the sender's bare JID from a materialized payload for the
/// XEP-0191 §2 step 4 flush-time block re-evaluation. Returns `None`
/// when the message has no `from` attribute (server-origin replays
/// have no flesh-and-blood sender to block).
fn sender_bare_for_payload(payload: &MaterializedPayload) -> Option<jid::BareJid> {
    let message: &xmpp_parsers::message::Message = match payload {
        MaterializedPayload::Archived(m) | MaterializedPayload::Transient(m) => m,
    };
    message.from.as_ref().map(|jid| jid.to_bare())
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

/// Sweep [`RecipientLockMap`] entries that no caller is currently
/// holding. Called periodically from the claim-expiry janitor (issue
/// #209 finding #4): the map was previously append-only and grew with
/// every distinct recipient bare-JID seen by the process, leaking an
/// `Arc<Mutex<()>>` per user permanently.
///
/// Race-safe: `remove_if` evaluates the predicate while holding the
/// shard's write lock, and the predicate (`strong_count == 1`) is
/// only true when no other task currently has a clone of the lock.
/// If a clone is in flight we leave the entry alone and the next
/// sweep retries.
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
        // Idempotent column-add migration for the locked Q7b
        // outbound_sequence column. Tables created by an older
        // version of waddle-server (before PR #358) were missing this
        // column, and `CREATE TABLE IF NOT EXISTS` is a no-op when the
        // table already exists — so the SELECT/INSERT/UPDATE statements
        // below would fail with "no such column: outbound_sequence" at
        // first use without this ALTER. (Codex/Qodo review on PR #358.)
        //
        // Both backends support `ADD COLUMN IF NOT EXISTS` syntax in
        // recent versions (SQLite ≥ 3.35.0, Postgres ≥ 9.6); for older
        // SQLite we fall through to a tolerant ALTER + best-effort
        // ignore of the "duplicate column" error.
        let alter_sql = match self.db.driver() {
            DatabaseDriver::Postgres => {
                "ALTER TABLE pending_delivery ADD COLUMN IF NOT EXISTS outbound_sequence INTEGER"
            }
            DatabaseDriver::Sqlite => {
                "ALTER TABLE pending_delivery ADD COLUMN outbound_sequence INTEGER"
            }
        };
        if let Err(error) = self.execute(alter_sql, ()).await {
            // SQLite's `ALTER TABLE … ADD COLUMN` is not idempotent
            // and reports "duplicate column name" when the column
            // already exists. Treat that specific error as a no-op so
            // the migration stays idempotent for both freshly-created
            // tables (where the column exists from CREATE TABLE
            // above) and pre-existing older tables.
            let msg = error.to_string().to_lowercase();
            if msg.contains("duplicate column") || msg.contains("already exists") {
                debug!("pending_delivery.outbound_sequence column already present");
            } else {
                return Err(error);
            }
        }
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
                let archive_jid: jid::Jid = by.into();
                PendingPayload::Archived(StanzaId::new(id_str, archive_jid))
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
        // Defensive: clear outbound_sequence on claim too. release_*
        // already does this, but a future code path that leaves a row
        // half-released should not be able to confuse the SM-ack
        // delete. Targets only newly-claimed rows (flushed_in_session
        // IS NULL filter ensures we don't trample another session's
        // ongoing claim).
        self.execute(
            "UPDATE pending_delivery SET flushed_in_session = ?, outbound_sequence = NULL \
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
        // Clear `outbound_sequence` alongside `flushed_in_session` so a
        // stale sequence from the dead session can't survive a re-claim
        // and trick a later session's SM ack into deleting an unack'd
        // row. (Qodo review on PR #358.)
        self.execute(
            "UPDATE pending_delivery SET flushed_in_session = NULL, \
                                          outbound_sequence = NULL \
             WHERE flushed_in_session = ?",
            crate::db_params![session.as_str().to_string()],
        )
        .await
    }

    async fn release_row(&self, id: &PendingRowId) -> Result<u64, PendingStorageError> {
        self.execute(
            "UPDATE pending_delivery SET flushed_in_session = NULL, \
                                          outbound_sequence = NULL \
             WHERE row_id = ?",
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

    async fn list_orphaned_claims(
        &self,
        live_sessions: &[SmSessionId],
    ) -> Result<Vec<(PendingRowId, SmSessionId)>, PendingStorageError> {
        // SELECT every claimed row, then filter in-memory against the
        // live-set. This avoids generating a `WHERE flushed_in_session
        // NOT IN (?, ?, ?, …)` clause whose parameter count would be
        // unbounded for production deployments. The expected orphan
        // population is small (low hundreds) compared to live-session
        // count, so the filter cost is negligible.
        let mut rows = self
            .query(
                "SELECT row_id, flushed_in_session FROM pending_delivery \
                 WHERE flushed_in_session IS NOT NULL",
                (),
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

    fn sweep_internal_bookkeeping(&self) -> usize {
        sweep_recipient_locks(&self.recipient_locks)
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
            &bare("alice@example.com"),
            &full("alice@example.com/web"),
            FlushContext {
                server_domain: "example.com",
                sm_session: None,
                blocking_storage: None,
                archive_resolver: &NullArchiveResolver,
            },
        )
        .await;
        assert_eq!(outcome, FlushOutcome::default());
    }

    #[tokio::test]
    async fn flush_pushes_transient_rows_and_keeps_them_for_sm_ack() {
        // SM-enabled flush: rows are pushed but stay in storage
        // claimed by the SM session until `delete_acked_through` is
        // called by the SM ack handler.
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        for body in ["one", "two"] {
            storage
                .insert(transient_row("alice@example.com", body))
                .await
                .unwrap();
        }

        let registry = ConnectionRegistry::new();
        let resource = full("alice@example.com/web");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        registry.register(resource.clone(), tx);

        let sm_session = SmSessionId::new("sm-stream-uuid-1");
        let outcome = flush_for_resource(
            &storage,
            &registry,
            &bare("alice@example.com"),
            &resource,
            FlushContext {
                server_domain: "example.com",
                sm_session: Some(&sm_session),
                blocking_storage: None,
                archive_resolver: &NullArchiveResolver,
            },
        )
        .await;
        assert_eq!(outcome.claimed, 2);
        assert_eq!(outcome.pushed, 2);
        assert_eq!(outcome.unresolved, 0);

        let mut received = Vec::new();
        while let Ok(stanza) = rx.try_recv() {
            received.push(stanza);
        }
        assert_eq!(received.len(), 2);

        // Locked Q7b SM-ack lifecycle: rows stay in storage tagged
        // `flushed_in_session` after push; deletion happens on SM
        // `<a h>` ack via `delete_acked_through`, NOT on send.
        assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 2);
        let listed = storage.list(&bare("alice@example.com")).await.unwrap();
        for row in &listed {
            assert_eq!(
                row.flushed_in_session.as_ref(),
                Some(&sm_session),
                "row claimed by the recovering SM session until SM-ack"
            );
        }
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
    async fn flush_non_sm_session_deletes_on_push() {
        // Codex review on PR #358: when the recovering connection has
        // NOT enabled XEP-0198, the SM ack handler will never fire to
        // delete claimed rows. The flush function must fall back to
        // delete-on-push so the queue doesn't leak forever for non-SM
        // clients.
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        for body in ["one", "two"] {
            storage
                .insert(transient_row("alice@example.com", body))
                .await
                .unwrap();
        }

        let registry = ConnectionRegistry::new();
        let resource = full("alice@example.com/web");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        registry.register(resource.clone(), tx);

        let outcome = flush_for_resource(
            &storage,
            &registry,
            &bare("alice@example.com"),
            &resource,
            FlushContext {
                server_domain: "example.com",
                sm_session: None, // ← no SM session: delete-on-push fallback
                blocking_storage: None,
                archive_resolver: &NullArchiveResolver,
            },
        )
        .await;
        assert_eq!(outcome.claimed, 2);
        assert_eq!(outcome.pushed, 2);

        // Both messages were sent on the wire.
        let mut received = Vec::new();
        while let Ok(stanza) = rx.try_recv() {
            received.push(stanza);
        }
        assert_eq!(received.len(), 2);

        // Non-SM fallback: rows are deleted on Sent (no ack will ever
        // fire). Storage is empty.
        assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 0);
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

        let sm_session = SmSessionId::new("sm-stream-uuid-1");
        let outcome = flush_for_resource(
            &storage,
            &registry,
            &bare("alice@example.com"),
            &resource,
            FlushContext {
                server_domain: "example.com",
                sm_session: Some(&sm_session),
                blocking_storage: None,
                archive_resolver: &NullArchiveResolver,
            },
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
            payload: PendingPayload::Archived(StanzaId::new(
                "mam-id",
                jid::Jid::from(recipient.clone()),
            )),
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
    async fn db_storage_archived_full_jid_by_round_trips_as_bare() {
        // Regression: `StanzaId.by` is a `jid::Jid`, so a future call site
        // could legitimately construct one with a resource. The
        // `archive_stanza_by` column is decoded back as a `BareJid`, so the
        // insert path must narrow with `.to_bare()`. Without that fix,
        // round-tripping a Full-JID `StanzaId` through SQL would fail in
        // `decode_row` and poison the recipient's pending queue.
        let storage = DatabasePendingDeliveryStorage::open(None, QuotaPolicy::Unlimited)
            .await
            .expect("open in-memory storage");
        let recipient = bare("alice@example.com");
        let full_by: jid::Jid = "alice@example.com/resource"
            .parse()
            .expect("valid full jid");
        let row = PendingRow {
            id: PendingRowId::fresh(),
            recipient: recipient.clone(),
            original_receipt_at: chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
                1_700_000_000_000,
            )
            .unwrap(),
            payload: PendingPayload::Archived(StanzaId::new("mam-id", full_by)),
            flushed_in_session: None,
            outbound_sequence: None,
        };
        assert_eq!(storage.insert(row).await.unwrap(), InsertOutcome::Inserted);

        let rows = storage.list(&recipient).await.unwrap();
        assert_eq!(rows.len(), 1);
        match &rows[0].payload {
            PendingPayload::Archived(stanza_id) => {
                assert_eq!(stanza_id.id, "mam-id");
                // Decoded `by` must be the bare form even though we
                // inserted a Full JID, so the column round-trips cleanly.
                assert_eq!(stanza_id.by, jid::Jid::from(recipient));
            }
            other => panic!("expected Archived payload, got {other:?}"),
        }
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

        let session_id = waddle_xmpp::pending_delivery::SmSessionId::new("sm-stream-uuid-7");
        let outcome = flush_for_resource(
            &storage,
            &registry,
            &bare("alice@example.com"),
            &resource,
            FlushContext {
                server_domain: "example.com",
                sm_session: Some(&session_id),
                blocking_storage: None,
                archive_resolver: &NullArchiveResolver,
            },
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

        let session_a = waddle_xmpp::pending_delivery::SmSessionId::new("sm-stream-laptop-uuid");
        let outcome = flush_for_resource(
            &storage,
            &registry,
            &bare("alice@example.com"),
            &resource_a,
            FlushContext {
                server_domain: "example.com",
                sm_session: Some(&session_a),
                blocking_storage: None,
                archive_resolver: &NullArchiveResolver,
            },
        )
        .await;
        assert_eq!(outcome.pushed, 1);
        let pushed = rx_a.try_recv().expect("flush stanza pushed");
        let row_id = pushed.pending_row_id.clone().unwrap();
        // Recipient stamped sequence, but no SM-ack arrives.
        storage.record_pushed_at(&row_id, 3).await.unwrap();

        // Session-A dies pre-ack — the SM janitor's release_claim
        // restores the row to the unclaimed pool.
        let released = storage.release_claim(&session_a).await.unwrap();
        assert_eq!(released, 1);

        // Verify release_claim cleared outbound_sequence too — a
        // stale value would let session-B's first ack delete the row
        // before it even pushes (Qodo review on PR #358).
        let after_release = storage.list(&bare("alice@example.com")).await.unwrap();
        assert_eq!(after_release.len(), 1);
        assert!(after_release[0].outbound_sequence.is_none());
        assert!(after_release[0].flushed_in_session.is_none());

        // Second resource comes online and claims for itself with a
        // distinct SM session id (different XEP-0198 stream).
        let resource_b = full("alice@example.com/web");
        let (tx_b, mut rx_b) = tokio::sync::mpsc::channel(8);
        registry.register(resource_b.clone(), tx_b);
        let session_b = waddle_xmpp::pending_delivery::SmSessionId::new("sm-stream-web-uuid");
        let outcome = flush_for_resource(
            &storage,
            &registry,
            &bare("alice@example.com"),
            &resource_b,
            FlushContext {
                server_domain: "example.com",
                sm_session: Some(&session_b),
                blocking_storage: None,
                archive_resolver: &NullArchiveResolver,
            },
        )
        .await;
        assert_eq!(outcome.pushed, 1, "row re-flushed to recovering resource-B");
        let pushed_b = rx_b.try_recv().expect("flush stanza pushed to resource-B");
        assert_eq!(pushed_b.pending_row_id.unwrap(), row_id, "same row");
    }

    #[tokio::test]
    async fn ack_before_record_pushed_at_skips_unsequenced_row() {
        // Greptile review on PR #358: documents the storage-layer
        // contract that motivates the `record_pushed_at` /
        // `delete_acked_through` ordering rule in the websocket main
        // loop. If `delete_acked_through` runs while a freshly-claimed
        // row's `outbound_sequence` is still NULL, the row is skipped
        // (correct: NULL means "not yet pushed, no h-coverage
        // possible"). The websocket main loop guarantees the
        // record_pushed_at completes before the next inbound frame
        // (including the SM ack) is processed by awaiting it inline
        // — this test pins down the storage semantics so a future
        // refactor that re-introduces async stamping breaks visibly.
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        storage
            .insert(transient_row("alice@example.com", "hi"))
            .await
            .unwrap();
        let session = waddle_xmpp::pending_delivery::SmSessionId::new("sm-stream");
        let claimed = storage
            .claim_for_session(&bare("alice@example.com"), &session)
            .await
            .unwrap();
        assert_eq!(claimed.len(), 1);
        let row_id = claimed[0].id.clone();
        // Ack runs before record_pushed_at — outbound_sequence is
        // NULL so the row is skipped. This is the failure mode
        // Greptile flagged when both calls were spawned: the row
        // would persist claimed-but-never-acked until session death.
        let removed = storage.delete_acked_through(&session, 100).await.unwrap();
        assert_eq!(
            removed, 0,
            "NULL outbound_sequence is skipped by delete_acked_through"
        );
        assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 1);

        // Now record_pushed_at fires (inline ordering would have done
        // this BEFORE the ack). A subsequent ack covering the same
        // h DOES delete the row. This proves recovery — the next ack
        // after the stamp completes the cleanup.
        storage.record_pushed_at(&row_id, 50).await.unwrap();
        let removed = storage.delete_acked_through(&session, 50).await.unwrap();
        assert_eq!(removed, 1);
        assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn list_orphaned_claims_returns_only_dead_session_rows() {
        // Issue #209 PR #360 storage-layer contract test for the
        // claim-expiry janitor. Three rows: row-A claimed by
        // session-live, row-B claimed by session-dead, row-C
        // unclaimed. With live=[session-live], the janitor should see
        // only row-B in the orphan list. row-A is recoverable, row-C
        // doesn't need recovery.
        let storage = InMemoryPendingDeliveryStorage::unlimited();
        let alice = bare("alice@example.com");
        for body in ["a", "b", "c"] {
            storage
                .insert(transient_row("alice@example.com", body))
                .await
                .unwrap();
        }
        let session_live = SmSessionId::new("sm-stream-live");
        let session_dead = SmSessionId::new("sm-stream-dead");
        // Claim two rows under each session in turn (claim_for_session
        // takes whatever's currently unclaimed, so call sequentially).
        let claimed_live = storage
            .claim_for_session(&alice, &session_live)
            .await
            .unwrap();
        assert_eq!(claimed_live.len(), 3);
        // Release one row back to unclaimed (simulating partial-success);
        // then "transfer" one to session_dead by releasing all and
        // re-claiming individually.
        for row in &claimed_live {
            storage.release_row(&row.id).await.unwrap();
        }
        // Now manually claim row[0] under session_live, row[1] under
        // session_dead, leave row[2] unclaimed.
        // claim_for_session is all-or-nothing, so build the state via
        // direct inserts on a fresh storage.
        let storage = InMemoryPendingDeliveryStorage::unlimited();
        for (body, session_opt) in [
            ("a", Some(&session_live)),
            ("b", Some(&session_dead)),
            ("c", None),
        ] {
            let mut row = transient_row("alice@example.com", body);
            row.flushed_in_session = session_opt.cloned();
            storage.insert(row).await.unwrap();
        }
        let orphans = storage
            .list_orphaned_claims(std::slice::from_ref(&session_live))
            .await
            .unwrap();
        assert_eq!(orphans.len(), 1, "only the dead-session row is orphaned");
        assert_eq!(
            orphans[0].1, session_dead,
            "orphan tagged with dead session"
        );
        // Releasing the orphan via `release_row` clears the claim.
        storage.release_row(&orphans[0].0).await.unwrap();
        let after = storage
            .list_orphaned_claims(std::slice::from_ref(&session_live))
            .await
            .unwrap();
        assert!(after.is_empty(), "no orphans after release");
        // The live-session and unclaimed rows remain in storage.
        assert_eq!(storage.count(&alice).await.unwrap(), 3);
    }

    #[tokio::test]
    async fn list_orphaned_claims_with_empty_live_set_returns_all_claims() {
        // Startup recovery scenario (issue #209 PR #360): SM registry
        // is empty after a restart, every claim is orphaned. The
        // janitor releases them all so the recovering resources can
        // re-flush.
        let storage = InMemoryPendingDeliveryStorage::unlimited();
        for body in ["a", "b"] {
            let mut row = transient_row("alice@example.com", body);
            row.flushed_in_session = Some(SmSessionId::new("sm-stream-pre-restart"));
            storage.insert(row).await.unwrap();
        }
        let orphans = storage.list_orphaned_claims(&[]).await.unwrap();
        assert_eq!(orphans.len(), 2);
    }

    #[tokio::test]
    async fn flush_drops_pending_row_when_sender_blocked_after_intake() {
        // Locked XEP-0191 §2 step 4 (issue #209 PR #360): if the
        // recipient blocks the sender AFTER the row was queued, the
        // flush MUST drop the row instead of replaying it. The block
        // is final until lifted, so the row is `delete_row`'d (not
        // released) — no retry needed.
        use waddle_xmpp::xep::xep0191::{BlockingStorage, InMemoryBlockingStorage};
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        storage
            .insert(transient_row("alice@example.com", "blocked-after-intake"))
            .await
            .unwrap();
        // Recipient blocks the sender BEFORE flush.
        let blocking = InMemoryBlockingStorage::new();
        blocking.set_blocklist(bare("alice@example.com"), vec![bare("bob@elsewhere")]);
        let blocking_arc: Arc<dyn BlockingStorage> = Arc::new(blocking);
        // Wire a recovering session.
        let registry = ConnectionRegistry::new();
        let resource = full("alice@example.com/web");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        registry.register(resource.clone(), tx);
        let sm_session = SmSessionId::new("sm-stream-block-test");
        let outcome = flush_for_resource(
            &storage,
            &registry,
            &bare("alice@example.com"),
            &resource,
            FlushContext {
                server_domain: "example.com",
                sm_session: Some(&sm_session),
                blocking_storage: Some(&blocking_arc),
                archive_resolver: &NullArchiveResolver,
            },
        )
        .await;
        assert_eq!(outcome.claimed, 1);
        assert_eq!(outcome.pushed, 0, "blocked sender's row not pushed");
        assert_eq!(outcome.dropped_blocked, 1);
        // Row is deleted from storage (block is final until lifted).
        assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 0);
        // Nothing was sent on the wire.
        assert!(
            rx.try_recv().is_err(),
            "no flush stanza pushed for blocked sender"
        );
    }

    #[tokio::test]
    async fn flush_aborts_on_blocking_storage_failure_fail_closed() {
        // Fail-closed semantic (mirrors interpret.rs intake-pass policy):
        // if blocking-storage errors at flush time, the flush MUST abort
        // rather than degrade to an empty blocklist (which would silently
        // let blocked senders through to MAM/inbox via re-delivery).
        use async_trait::async_trait;
        use waddle_xmpp::xep::xep0191::{BlockingStorage, BlockingStorageError};
        #[derive(Debug, thiserror::Error)]
        #[error("simulated backend down")]
        struct SimulatedFailure;
        struct FailingBlocking;
        #[async_trait]
        impl BlockingStorage for FailingBlocking {
            async fn list_blocked_jids(
                &self,
                _user: &BareJid,
            ) -> Result<Vec<BareJid>, BlockingStorageError> {
                Err(BlockingStorageError::new(SimulatedFailure))
            }
        }
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        storage
            .insert(transient_row("alice@example.com", "must-not-leak"))
            .await
            .unwrap();
        let blocking_arc: Arc<dyn BlockingStorage> = Arc::new(FailingBlocking);
        let registry = ConnectionRegistry::new();
        let resource = full("alice@example.com/web");
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        registry.register(resource.clone(), tx);
        let sm_session = SmSessionId::new("sm-stream-fail-closed");
        let outcome = flush_for_resource(
            &storage,
            &registry,
            &bare("alice@example.com"),
            &resource,
            FlushContext {
                server_domain: "example.com",
                sm_session: Some(&sm_session),
                blocking_storage: Some(&blocking_arc),
                archive_resolver: &NullArchiveResolver,
            },
        )
        .await;
        // Fail-closed: nothing claimed, nothing pushed, row stays for retry.
        assert_eq!(outcome.claimed, 0);
        assert_eq!(outcome.pushed, 0);
        assert_eq!(storage.count(&bare("alice@example.com")).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn list_orphaned_claims_excludes_active_session_rows() {
        // Codex/Qodo P1 review on PR #360: the claim-expiry janitor's
        // "live" set MUST include both detached/resumable SM sessions
        // (`sm_session_registry.live_session_ids()`) AND currently-
        // connected active SM sessions (`ConnectionEntry.sm_stream_id`).
        // Without the active half, a row claimed by a connected
        // resource awaiting `<a h>` would be misclassified as orphaned
        // and `release_row`'d, breaking the SM-ack lifecycle and
        // producing a duplicate flush on the next presence transition.
        //
        // This test pins the storage-layer contract: passing the
        // active session id into `list_orphaned_claims`'s `live`
        // argument MUST exclude its rows from the orphan list. The
        // janitor wiring in `start_with_config` builds the union and
        // is exercised by integration coverage above.
        let storage = InMemoryPendingDeliveryStorage::unlimited();
        let mut row = transient_row("alice@example.com", "active");
        let active_session = SmSessionId::new("sm-stream-active");
        row.flushed_in_session = Some(active_session.clone());
        storage.insert(row).await.unwrap();

        // Sweep with active_session in the live set: NOT an orphan.
        let orphans = storage
            .list_orphaned_claims(std::slice::from_ref(&active_session))
            .await
            .unwrap();
        assert!(
            orphans.is_empty(),
            "row claimed by an active SM session must not be flagged as orphaned"
        );

        // Sweep with active session MISSING from the live set: now an
        // orphan. This is the failure mode that prompted the fix —
        // the previous janitor only consulted the detached registry,
        // which would have produced this incorrect result.
        let dead_session = SmSessionId::new("sm-stream-something-else");
        let orphans = storage
            .list_orphaned_claims(std::slice::from_ref(&dead_session))
            .await
            .unwrap();
        assert_eq!(
            orphans.len(),
            1,
            "row IS an orphan when its session is missing from the live set"
        );
        assert_eq!(orphans[0].1, active_session);
    }

    #[tokio::test]
    async fn janitor_releases_rows_with_dead_sessions() {
        // End-to-end exercise of the claim-expiry janitor's data flow
        // (issue #209 PR #360): given a mixture of rows tagged with
        // a live session, a dead session, and an unclaimed row, the
        // janitor's expected sequence (`list_orphaned_claims(live)`
        // → `release_row(orphan)`) MUST release exactly the dead-
        // session rows and leave the live + unclaimed rows alone.
        //
        // The janitor task itself runs in the websocket runtime and
        // is not directly addressable from a unit test; this test
        // pins the storage-layer flow that the janitor relies on.
        // The websocket wiring (live-set union of detached +
        // active SM streams) is verified separately by
        // `list_orphaned_claims_excludes_active_session_rows`.
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        let alice = bare("alice@example.com");

        // Build the state directly (claim_for_session is all-or-nothing,
        // which doesn't fit the test setup of mixed claim states).
        let live_session = SmSessionId::new("sm-stream-live");
        let dead_session_a = SmSessionId::new("sm-stream-dead-a");
        let dead_session_b = SmSessionId::new("sm-stream-dead-b");
        for (body, session_opt, sequence_opt) in [
            ("live-claimed", Some(live_session.clone()), Some(7u32)),
            ("dead-claimed-a", Some(dead_session_a.clone()), Some(3u32)),
            ("dead-claimed-b-no-seq", Some(dead_session_b.clone()), None),
            ("unclaimed", None, None),
        ] {
            let mut row = transient_row("alice@example.com", body);
            row.flushed_in_session = session_opt;
            row.outbound_sequence = sequence_opt;
            storage.insert(row).await.unwrap();
        }
        assert_eq!(storage.count(&alice).await.unwrap(), 4);

        // Janitor sweep step 1: ask for orphans given the live set.
        let orphans = storage
            .list_orphaned_claims(std::slice::from_ref(&live_session))
            .await
            .unwrap();
        assert_eq!(orphans.len(), 2, "two dead-session rows are orphaned");
        let orphan_sessions: std::collections::HashSet<_> =
            orphans.iter().map(|(_, s)| s.clone()).collect();
        assert!(orphan_sessions.contains(&dead_session_a));
        assert!(orphan_sessions.contains(&dead_session_b));

        // Janitor sweep step 2: release each orphan row.
        for (row_id, _) in &orphans {
            storage.release_row(row_id).await.unwrap();
        }

        // Post-sweep assertions:
        // - Live row stays tagged + sequenced (will be deleted by SM ack).
        // - Both dead-session rows are now unclaimed (re-flush eligible).
        // - The originally-unclaimed row is untouched.
        // - No rows were deleted — the janitor only releases.
        assert_eq!(storage.count(&alice).await.unwrap(), 4, "no rows deleted");
        let after = storage.list(&alice).await.unwrap();
        let by_body: std::collections::HashMap<&str, &PendingRow> = after
            .iter()
            .map(|row| {
                let body_marker = match &row.payload {
                    PendingPayload::Transient(m) => {
                        m.bodies.get("").map(|b| b.0.as_str()).unwrap_or("")
                    }
                    _ => "",
                };
                (body_marker, row)
            })
            .collect();
        let live_row = by_body.get("live-claimed").expect("live row present");
        assert_eq!(live_row.flushed_in_session.as_ref(), Some(&live_session));
        assert_eq!(live_row.outbound_sequence, Some(7));
        let dead_a = by_body.get("dead-claimed-a").expect("dead-a present");
        assert!(dead_a.flushed_in_session.is_none(), "released by janitor");
        assert!(
            dead_a.outbound_sequence.is_none(),
            "release_row clears outbound_sequence"
        );
        let dead_b = by_body
            .get("dead-claimed-b-no-seq")
            .expect("dead-b present");
        assert!(dead_b.flushed_in_session.is_none());
        let unclaimed = by_body.get("unclaimed").expect("unclaimed present");
        assert!(unclaimed.flushed_in_session.is_none());
    }

    #[tokio::test]
    async fn flush_blocked_row_releases_claim_when_delete_fails() {
        // Copilot review on PR #360: if `delete_row` fails for a
        // blocked row, the row would otherwise stay tagged with the
        // current (still-live) SM session id. The SM-expiry janitor
        // wouldn't see it as orphaned, the SM ack wouldn't delete it
        // (NULL outbound_sequence), and the next flush wouldn't
        // re-claim it. Permanent wedge + quota leak. Fix: fall back
        // to `release_row` so the next flush can re-check the block.
        use async_trait::async_trait;
        use waddle_xmpp::pending_delivery::storage::PendingStorageError;
        use waddle_xmpp::xep::xep0191::{BlockingStorage, InMemoryBlockingStorage};

        // Wrap an in-memory storage so `delete_row` fails once but
        // every other operation passes through.
        struct DeleteRowFails {
            inner: InMemoryPendingDeliveryStorage,
        }
        #[async_trait]
        impl PendingDeliveryStorage for DeleteRowFails {
            async fn insert(&self, row: PendingRow) -> Result<InsertOutcome, PendingStorageError> {
                self.inner.insert(row).await
            }
            async fn list(
                &self,
                recipient: &BareJid,
            ) -> Result<Vec<PendingRow>, PendingStorageError> {
                self.inner.list(recipient).await
            }
            async fn claim_for_session(
                &self,
                recipient: &BareJid,
                session: &waddle_xmpp::pending_delivery::SmSessionId,
            ) -> Result<Vec<PendingRow>, PendingStorageError> {
                self.inner.claim_for_session(recipient, session).await
            }
            async fn delete_claimed(
                &self,
                session: &waddle_xmpp::pending_delivery::SmSessionId,
            ) -> Result<u64, PendingStorageError> {
                self.inner.delete_claimed(session).await
            }
            async fn delete_row(&self, _id: &PendingRowId) -> Result<u64, PendingStorageError> {
                Err(PendingStorageError::Other(
                    "simulated delete failure".into(),
                ))
            }
            async fn release_claim(
                &self,
                session: &waddle_xmpp::pending_delivery::SmSessionId,
            ) -> Result<u64, PendingStorageError> {
                self.inner.release_claim(session).await
            }
            async fn release_row(&self, id: &PendingRowId) -> Result<u64, PendingStorageError> {
                self.inner.release_row(id).await
            }
            async fn record_pushed_at(
                &self,
                id: &PendingRowId,
                sequence: u32,
            ) -> Result<u64, PendingStorageError> {
                self.inner.record_pushed_at(id, sequence).await
            }
            async fn delete_acked_through(
                &self,
                session: &waddle_xmpp::pending_delivery::SmSessionId,
                sequence_max: u32,
            ) -> Result<u64, PendingStorageError> {
                self.inner.delete_acked_through(session, sequence_max).await
            }
            async fn list_orphaned_claims(
                &self,
                live: &[waddle_xmpp::pending_delivery::SmSessionId],
            ) -> Result<
                Vec<(PendingRowId, waddle_xmpp::pending_delivery::SmSessionId)>,
                PendingStorageError,
            > {
                self.inner.list_orphaned_claims(live).await
            }
            async fn count(&self, recipient: &BareJid) -> Result<u32, PendingStorageError> {
                self.inner.count(recipient).await
            }
            async fn delete_older_than(
                &self,
                cutoff: chrono::DateTime<chrono::Utc>,
            ) -> Result<u64, PendingStorageError> {
                self.inner.delete_older_than(cutoff).await
            }
        }

        let storage: Arc<dyn PendingDeliveryStorage> = Arc::new(DeleteRowFails {
            inner: InMemoryPendingDeliveryStorage::unlimited(),
        });
        storage
            .insert(transient_row("alice@example.com", "blocked-row"))
            .await
            .unwrap();
        let blocking = InMemoryBlockingStorage::new();
        blocking.set_blocklist(bare("alice@example.com"), vec![bare("bob@elsewhere")]);
        let blocking_arc: Arc<dyn BlockingStorage> = Arc::new(blocking);
        let registry = ConnectionRegistry::new();
        let resource = full("alice@example.com/web");
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        registry.register(resource.clone(), tx);
        let sm_session = SmSessionId::new("sm-stream-wedge-test");

        let outcome = flush_for_resource(
            &storage,
            &registry,
            &bare("alice@example.com"),
            &resource,
            FlushContext {
                server_domain: "example.com",
                sm_session: Some(&sm_session),
                blocking_storage: Some(&blocking_arc),
                archive_resolver: &NullArchiveResolver,
            },
        )
        .await;
        assert_eq!(outcome.dropped_blocked, 1);

        // Row stays in storage (delete_row failed), but the claim
        // MUST be cleared by the release_row fallback so a future
        // flush can re-evaluate the blocklist or push it.
        let rows = storage.list(&bare("alice@example.com")).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].flushed_in_session.is_none(),
            "release_row fallback cleared the wedged claim"
        );
        assert!(rows[0].outbound_sequence.is_none());
    }

    #[tokio::test]
    async fn xep0160_promoted_stanzas_carry_original_receipt_time_in_delay() {
        // Issue #209 PR #361 dedicated XEP-0160 test (Greptile +
        // Copilot + Qodo P1 review): the SM-promoted-then-replayed
        // path MUST carry the ORIGINAL pending_delivery row's
        // `original_receipt_at` all the way through to the eventual
        // XEP-0203 `<delay/>` stamp on the offline replay, even
        // when the stanza was flushed to a live SM session that
        // disconnected pre-ack and the SM session later expired
        // (Q6 promotion re-creates the pending row).
        //
        // Failure mode this guards against: stamping `Utc::now()`
        // anywhere along the path (flush time, drain time, expiry
        // time) would mean the recipient sees the wrong delivery
        // time on their reconnect.
        //
        // End-to-end flow exercised:
        //   1. Insert pending row with original_receipt_at = T1.
        //   2. flush_for_resource sends OutboundStanza carrying T1.
        //   3. Recipient's main loop records into SM unacked queue
        //      with T1 (via record_outbound_with_receipt_at).
        //   4. Convert SM state → DetachedSession (simulates
        //      disconnect + detach at T2 >> T1).
        //   5. promote_session_unacked re-creates a pending row.
        //   6. Verify the new row's original_receipt_at == T1.
        use waddle_xmpp::stream_management::{DetachedSessionSnapshot, StreamManagementState};
        let storage: Arc<dyn PendingDeliveryStorage> =
            Arc::new(InMemoryPendingDeliveryStorage::unlimited());
        let registry = ConnectionRegistry::new();
        let alice_bare = bare("alice@example.com");
        let alice_jid = full("alice@example.com/laptop");

        // T1 = the original failed-delivery time (a year ago).
        let t1 = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(1_700_000_000_000)
            .expect("valid millis");
        let mut row = transient_row("alice@example.com", "missed-while-offline");
        row.original_receipt_at = t1;
        let row_id = row.id.clone();
        storage.insert(row).await.unwrap();

        // Wire alice's recovering resource as the recipient.
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        registry.register(alice_jid.clone(), tx);

        // Step 2: flush_for_resource through the SM-enabled path.
        let sm_session_id =
            waddle_xmpp::pending_delivery::SmSessionId::new("sm-stream-receipt-e2e");
        let outcome = flush_for_resource(
            &storage,
            &registry,
            &alice_bare,
            &alice_jid,
            FlushContext {
                server_domain: "example.com",
                sm_session: Some(&sm_session_id),
                blocking_storage: None,
                archive_resolver: &NullArchiveResolver,
            },
        )
        .await;
        assert_eq!(outcome.pushed, 1);

        // Step 3: pluck the OutboundStanza from the channel — it
        // MUST carry T1 as pending_row_original_receipt_at.
        let pushed = rx.try_recv().expect("flush stanza pushed");
        assert_eq!(
            pushed.pending_row_id.as_ref(),
            Some(&row_id),
            "OutboundStanza tagged with source row id"
        );
        assert_eq!(
            pushed.pending_row_original_receipt_at,
            Some(t1),
            "OutboundStanza carries the source row's original_receipt_at"
        );

        // Step 4: simulate the recipient's main loop recording the
        // outbound stanza into its SM unacked queue WITH T1, then
        // converting state → DetachedSession (i.e. transport drops).
        let mut sm_state = StreamManagementState::new();
        sm_state.enable("sm-stream-receipt-e2e".to_string(), true, Some(300));
        let xml = match &pushed.stanza {
            waddle_xmpp::Stanza::Message(m) => {
                let element: xmpp_parsers::minidom::Element = m.clone().into();
                let mut buf = Vec::new();
                element.write_to(&mut buf).unwrap();
                String::from_utf8(buf).unwrap()
            }
            _ => panic!("expected Message"),
        };
        sm_state.record_outbound_with_receipt_at(xml, t1);

        // Convert to detached session (simulates transport drop).
        let detached = sm_state
            .to_detached_session(DetachedSessionSnapshot {
                user_id: "alice".to_string(),
                jid: alice_jid.clone(),
                carbons_enabled: false,
                roster_interested: false,
                presence_available: false,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
            })
            .expect("session resumable");

        // Verify the detached snapshot preserved T1.
        assert_eq!(detached.unacked_stanzas.len(), 1);
        assert_eq!(detached.unacked_stanzas[0].original_receipt_at, t1);

        // Clear the original pending row so we observe only the
        // promoted row (the original would have been deleted by
        // SM-ack in production; here we simulate).
        storage.delete_row(&row_id).await.unwrap();

        // Step 5: SM-expiry promotion re-creates the pending row.
        let summary = crate::sm_promotion::promote_session_unacked(
            &detached,
            &registry,
            &storage,
            &waddle_xmpp::protocol::session_state::Blocklist::empty(),
        )
        .await;
        assert_eq!(summary.queued, 1);

        // Step 6: the new pending row carries T1, NOT flush/expiry time.
        let rows = storage.list(&alice_bare).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].original_receipt_at, t1,
            "promoted row's original_receipt_at MUST be the source row's T1, \
             NOT the flush/drain/expiry wall-clock"
        );
    }

    #[tokio::test]
    async fn xep0160_pending_delivery_survives_server_restart() {
        // Locked Q8 = B (issue #209): `pending_delivery` rows MUST
        // survive a process restart. This is the actual restart-
        // durability test (Codex P2 review on PR #362: the
        // waddle-xmpp pointer test only exercised read-after-write
        // through the same in-memory handle, which is not a
        // restart-equivalent).
        //
        // Real restart simulation: open a SQLite-backed storage
        // against a tempdir path, insert a row, drop the storage
        // handle (closes the connection), reopen against the SAME
        // path, assert the row is still present.
        //
        // Use `tempdir()` + `path.join()` rather than
        // `NamedTempFile`: NamedTempFile keeps an open OS file
        // handle alive for its lifetime, which can interfere with
        // SQLite's file-locking semantics on some platforms
        // (Copilot review on PR #362). The tempdir version creates
        // only a directory; the SQLite file inside it has no other
        // open handles.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir
            .path()
            .join("pending_delivery.sqlite")
            .to_str()
            .expect("utf-8 path")
            .to_string();
        let url = format!("sqlite://{path}");

        // Boot 1: write a row + drop the handle to close the connection.
        {
            let storage = DatabasePendingDeliveryStorage::open(Some(&url), QuotaPolicy::Unlimited)
                .await
                .expect("open file-backed storage");
            let outcome = storage
                .insert(transient_row("alice@example.com", "across-restart"))
                .await
                .expect("insert before restart");
            assert_eq!(outcome, InsertOutcome::Inserted);
        }

        // Boot 2: reopen against the SAME path (process restart
        // semantics). The row MUST still be there.
        let storage = DatabasePendingDeliveryStorage::open(Some(&url), QuotaPolicy::Unlimited)
            .await
            .expect("reopen file-backed storage");
        let rows = storage
            .list(&bare("alice@example.com"))
            .await
            .expect("list after restart");
        assert_eq!(
            rows.len(),
            1,
            "row durably persisted across the process-restart boundary"
        );
        let body = match &rows[0].payload {
            PendingPayload::Transient(m) => m.bodies.get("").map(|b| b.0.as_str()),
            _ => None,
        };
        assert_eq!(body, Some("across-restart"));
    }
}
