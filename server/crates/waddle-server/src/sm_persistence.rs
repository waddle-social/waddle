//! libSQL/Postgres-backed XEP-0198 stream-management persistence
//! (issue #209 slice (d) phase 2).
//!
//! `waddle_xmpp::stream_management::persistence::SmPersistenceStorage`
//! defines the typed contract; this module provides the production
//! database backend. Mirrors the `crate::pending_delivery` module's
//! storage shape: `Database::open` opens SQLite or Postgres; per-stream
//! locks make multi-statement quota / append paths atomic across both
//! drivers.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use jid::FullJid;
use tracing::{debug, info, instrument};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::{
    PersistedSession, PersistedUnackedStanza, SmPersistenceError, SmPersistenceStorage,
    SmUnackedStanzaPurpose,
};
use waddle_xmpp::Stanza;
use xmpp_parsers::presence::Show;

use crate::db::{Database, DatabaseConfig, DatabaseDriver, IntoParams};

mod atomic_store;
/// `pub(crate)` (rather than private, the norm for this module's
/// siblings) so ADR-0017 Phase 3 Slice 4's `PostgresFencedSmPersistence`
/// (`crate::sm_persistence_fenced`, a sibling top-level module — not a
/// submodule of this one, per the "not a decorator" file-layout rule) can
/// reuse the same wire encode/decode logic against the byte-identical
/// `sm_sessions`/`sm_unacked` schema, instead of forking a second copy
/// that could silently drift from this one.
pub(crate) mod codec;
mod joined_sessions;
mod schema;

use codec::{
    decode_session, decode_unacked, decode_unacked_join_row, serialize_presence_payloads,
    serialize_stanza, show_wire_str, unacked_purpose_wire_str,
};

/// Per-stream insert/update mutex map. Serializes writes for the same
/// session so the multi-statement append + ack paths are linearizable
/// across SQLite (single-writer) and Postgres (READ-COMMITTED).
type StreamLockMap = dashmap::DashMap<SmSessionId, Arc<tokio::sync::Mutex<()>>>;

/// libSQL/Postgres-backed [`SmPersistenceStorage`].
///
/// Schema:
///
/// ```sql
/// CREATE TABLE sm_sessions (
///     stream_id TEXT PRIMARY KEY,
///     user_id TEXT NOT NULL,
///     full_jid TEXT NOT NULL,
///     inbound_count BIGINT NOT NULL,
///     outbound_count BIGINT NOT NULL,
///     last_acked BIGINT NOT NULL,
///     max_resume_secs BIGINT,
///     detached_at_ms BIGINT NOT NULL,
///     max_resume_duration_ms BIGINT NOT NULL,
///     carbons_enabled INTEGER NOT NULL,
///     roster_interested INTEGER NOT NULL,
///     presence_available INTEGER NOT NULL,
///     presence_show TEXT,
///     presence_status TEXT,
///     presence_priority INTEGER NOT NULL,
///     replay_gap_through BIGINT
/// );
///
/// CREATE TABLE sm_unacked (
///     stream_id TEXT NOT NULL,
///     sequence BIGINT NOT NULL,
///     stanza_xml TEXT NOT NULL,
///     original_receipt_at_ms BIGINT NOT NULL,
///     purpose TEXT NOT NULL,
///     PRIMARY KEY (stream_id, sequence)
/// );
/// ```
///
/// `sm_unacked.stream_id` is logically a foreign key into
/// `sm_sessions.stream_id`. We do NOT declare a hard FK because
/// SQLite's default `PRAGMA foreign_keys = OFF` makes them advisory
/// anyway, and we want `delete_session` to drain unacked rows
/// explicitly via two statements (so the trait's lifecycle is
/// observable in tests against the in-memory fake too).
#[derive(Clone)]
pub struct DatabaseSmPersistence {
    db: Database,
    stream_locks: Arc<StreamLockMap>,
}

impl DatabaseSmPersistence {
    /// Open against `database_url`. Supported schemes:
    ///
    /// - `postgres://…` / `postgresql://…` → Postgres adapter
    /// - `sqlite://…`, bare path, or `:memory:` → SQLite adapter
    ///
    /// `None` falls back to in-memory SQLite suitable only for tests.
    /// `libsql://…` URLs are NOT supported by the current
    /// [`crate::db::DatabaseDriver`] enum and would silently route to
    /// the SQLite adapter (which doesn't speak the libSQL wire
    /// protocol). Reject them explicitly so misconfigured deployments
    /// fail loudly at startup. (Copilot review on PR #344.)
    pub async fn open(database_url: Option<&str>) -> Result<Self, SmPersistenceError> {
        let db = match database_url {
            Some(url) => {
                let driver = if url.starts_with("postgres://") || url.starts_with("postgresql://") {
                    DatabaseDriver::Postgres
                } else if url.starts_with("libsql://") || url.starts_with("libsql+") {
                    return Err(SmPersistenceError::Other(format!(
                        "libsql:// URL '{url}' is not supported by the current \
                         crate::db::DatabaseDriver (SQLite or Postgres only); \
                         use sqlite:// or postgres:// instead"
                    )));
                } else {
                    DatabaseDriver::Sqlite
                };
                Database::from_config(
                    "sm_persistence",
                    &DatabaseConfig::new(driver, url.to_string()),
                )
                .await
                .map_err(|e| SmPersistenceError::Other(e.to_string()))?
            }
            None => Database::in_memory("sm_persistence")
                .await
                .map_err(|e| SmPersistenceError::Other(e.to_string()))?,
        };
        let storage = Self {
            db,
            stream_locks: Arc::new(StreamLockMap::new()),
        };
        schema::initialize(&storage).await?;
        info!(driver = ?storage.db.driver(), "SM persistence storage initialized (XEP-0198)");
        Ok(storage)
    }

    async fn execute(&self, sql: &str, params: impl IntoParams) -> Result<u64, SmPersistenceError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        conn.execute(sql, params)
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))
    }

    async fn query(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<crate::db::Rows, SmPersistenceError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        conn.query(sql, params)
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))
    }

    fn lock_for(&self, stream_id: &SmSessionId) -> Arc<tokio::sync::Mutex<()>> {
        self.stream_locks
            .entry(stream_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    fn drop_stream_lock(&self, stream_id: &SmSessionId) {
        self.stream_locks
            .remove_if(stream_id, |_, lock| Arc::strong_count(lock) == 1);
    }
}

#[async_trait]
impl SmPersistenceStorage for DatabaseSmPersistence {
    #[instrument(skip(self, session), fields(stream_id = %session.stream_id))]
    async fn upsert_session(&self, session: PersistedSession) -> Result<(), SmPersistenceError> {
        let lock = self.lock_for(&session.stream_id);
        let _guard = lock.lock().await;

        let max_resume_duration_ms = i64::try_from(session.max_resume_duration.as_millis())
            .map_err(|_| SmPersistenceError::Other("max_resume_duration overflows i64".into()))?;
        let detached_at_ms = session.detached_at.timestamp_millis();
        let presence_show_str = session.presence_show.as_ref().map(show_wire_str);
        let presence_payloads_xml = serialize_presence_payloads(&session.presence_payloads)?;

        // Portable upsert: SQLite and Postgres both support
        // INSERT ... ON CONFLICT (col) DO UPDATE SET ...
        self.execute(
            r#"
            INSERT INTO sm_sessions (
                stream_id, user_id, full_jid, inbound_count, outbound_count,
                last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms,
                carbons_enabled, roster_interested, blocklist_interested, presence_available,
                presence_show, presence_status, presence_priority, replay_gap_through,
                presence_payloads
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT (stream_id) DO UPDATE SET
                user_id = excluded.user_id,
                full_jid = excluded.full_jid,
                inbound_count = excluded.inbound_count,
                outbound_count = excluded.outbound_count,
                last_acked = excluded.last_acked,
                max_resume_secs = excluded.max_resume_secs,
                detached_at_ms = excluded.detached_at_ms,
                max_resume_duration_ms = excluded.max_resume_duration_ms,
                carbons_enabled = excluded.carbons_enabled,
                roster_interested = excluded.roster_interested,
                blocklist_interested = excluded.blocklist_interested,
                presence_available = excluded.presence_available,
                presence_show = excluded.presence_show,
                presence_status = excluded.presence_status,
                presence_priority = excluded.presence_priority,
                replay_gap_through = excluded.replay_gap_through,
                presence_payloads = excluded.presence_payloads
            "#,
            crate::db_params![
                session.stream_id.as_str().to_string(),
                session.user_id,
                session.jid.to_string(),
                i64::from(session.inbound_count),
                i64::from(session.outbound_count),
                i64::from(session.last_acked),
                session.max_resume_time.map(i64::from),
                detached_at_ms,
                max_resume_duration_ms,
                i64::from(session.carbons_enabled),
                i64::from(session.roster_interested),
                i64::from(session.blocklist_interested),
                i64::from(session.presence_available),
                presence_show_str.map(str::to_string),
                session.presence_status,
                i64::from(session.presence_priority),
                session.replay_gap_through.map(i64::from),
                presence_payloads_xml,
            ],
        )
        .await?;
        Ok(())
    }

    async fn get_session(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Option<PersistedSession>, SmPersistenceError> {
        let mut rows = self
            .query(
                "SELECT stream_id, user_id, full_jid, inbound_count, outbound_count, \
                        last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms, \
                        carbons_enabled, roster_interested, blocklist_interested, presence_available, \
                        presence_show, presence_status, presence_priority, replay_gap_through, \
                        presence_payloads \
                 FROM sm_sessions WHERE stream_id = ?",
                crate::db_params![stream_id.as_str().to_string()],
            )
            .await?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?
        {
            Ok(Some(decode_session(&row).map_err(|error| {
                SmPersistenceError::Corrupt {
                    stream_id: stream_id.clone(),
                    detail: error.to_string(),
                }
            })?))
        } else {
            Ok(None)
        }
    }

    #[instrument(skip(self), fields(stream_id = %stream_id))]
    async fn record_promotion_failure(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<u32, SmPersistenceError> {
        let lock = self.lock_for(stream_id);
        let _guard = lock.lock().await;
        let updated = self
            .execute(
                "UPDATE sm_sessions SET promotion_attempts = promotion_attempts + 1 \
                 WHERE stream_id = ?",
                crate::db_params![stream_id.as_str().to_string()],
            )
            .await?;
        if updated == 0 {
            return Ok(0);
        }
        let mut rows = self
            .query(
                "SELECT promotion_attempts FROM sm_sessions WHERE stream_id = ?",
                crate::db_params![stream_id.as_str().to_string()],
            )
            .await?;
        let count = match rows
            .next()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?
        {
            Some(row) => row
                .get::<i64>(0)
                .map_err(|e| SmPersistenceError::Other(e.to_string()))?,
            None => 0,
        };
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    #[instrument(skip(self), fields(stream_id = %stream_id))]
    async fn delete_session(&self, stream_id: &SmSessionId) -> Result<(), SmPersistenceError> {
        let lock = self.lock_for(stream_id);
        let _guard = lock.lock().await;
        // Two statements rather than ON DELETE CASCADE so the trait's
        // observable lifecycle matches the in-memory fake.
        self.execute(
            "DELETE FROM sm_unacked WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await?;
        self.execute(
            "DELETE FROM sm_sessions WHERE stream_id = ?",
            crate::db_params![stream_id.as_str().to_string()],
        )
        .await?;
        drop(_guard);
        drop(lock);
        self.drop_stream_lock(stream_id);
        Ok(())
    }

    #[instrument(skip(self, stanza), fields(stream_id = %stanza.stream_id, seq = stanza.sequence))]
    async fn append_unacked(
        &self,
        stanza: PersistedUnackedStanza,
    ) -> Result<(), SmPersistenceError> {
        let lock = self.lock_for(&stanza.stream_id);
        let _guard = lock.lock().await;
        let xml = serialize_stanza(&stanza.stanza)?;
        let receipt_ms = stanza.original_receipt_at.timestamp_millis();
        self.execute(
            "INSERT INTO sm_unacked (stream_id, sequence, stanza_xml, original_receipt_at_ms, purpose) \
             VALUES (?, ?, ?, ?, ?)",
            crate::db_params![
                stanza.stream_id.as_str().to_string(),
                i64::from(stanza.sequence),
                xml,
                receipt_ms,
                unacked_purpose_wire_str(stanza.purpose).to_string(),
            ],
        )
        .await?;
        Ok(())
    }

    async fn ack_through(
        &self,
        stream_id: &SmSessionId,
        up_to_sequence: u32,
    ) -> Result<u64, SmPersistenceError> {
        let lock = self.lock_for(stream_id);
        let _guard = lock.lock().await;
        self.execute(
            "DELETE FROM sm_unacked WHERE stream_id = ? AND sequence <= ?",
            crate::db_params![stream_id.as_str().to_string(), i64::from(up_to_sequence)],
        )
        .await
    }

    #[instrument(skip(self, sequences), fields(stream_id = %stream_id, count = sequences.len()))]
    async fn delete_unacked(
        &self,
        stream_id: &SmSessionId,
        sequences: &[u32],
    ) -> Result<u64, SmPersistenceError> {
        let lock = self.lock_for(stream_id);
        let _guard = lock.lock().await;
        let mut removed = 0u64;
        // One PK-targeted statement per sequence: tombstone scrubs
        // remove a single stanza per session in practice, so the
        // per-row round-trip is cheaper than building a dynamic IN
        // clause the fixed-arity param binder can't express.
        for sequence in sequences {
            removed += self
                .execute(
                    "DELETE FROM sm_unacked WHERE stream_id = ? AND sequence = ?",
                    crate::db_params![stream_id.as_str().to_string(), i64::from(*sequence)],
                )
                .await?;
        }
        Ok(removed)
    }

    async fn list_unacked(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Vec<PersistedUnackedStanza>, SmPersistenceError> {
        let mut rows = self
            .query(
                "SELECT stream_id, sequence, stanza_xml, original_receipt_at_ms, purpose \
                 FROM sm_unacked WHERE stream_id = ? \
                 ORDER BY sequence ASC",
                crate::db_params![stream_id.as_str().to_string()],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?
        {
            out.push(
                decode_unacked(&row).map_err(|error| SmPersistenceError::Corrupt {
                    stream_id: stream_id.clone(),
                    detail: error.to_string(),
                })?,
            );
        }
        Ok(out)
    }

    async fn list_expired_sessions(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<PersistedSession>, SmPersistenceError> {
        let now_ms = now.timestamp_millis();
        // Filter via SQL where possible (detached_at + duration <= now)
        // and double-check in Rust to handle clock-skew / partial-row
        // edge cases.
        let mut rows = self
            .query(
                "SELECT stream_id, user_id, full_jid, inbound_count, outbound_count, \
                        last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms, \
                        carbons_enabled, roster_interested, blocklist_interested, presence_available, \
                        presence_show, presence_status, presence_priority, replay_gap_through, \
                        presence_payloads \
                 FROM sm_sessions WHERE detached_at_ms + max_resume_duration_ms <= ?",
                crate::db_params![now_ms],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?
        {
            out.push(decode_session(&row)?);
        }
        Ok(out)
    }

    async fn list_all_sessions(&self) -> Result<Vec<PersistedSession>, SmPersistenceError> {
        let mut rows = self
            .query(
                "SELECT stream_id, user_id, full_jid, inbound_count, outbound_count, \
                        last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms, \
                        carbons_enabled, roster_interested, blocklist_interested, presence_available, \
                        presence_show, presence_status, presence_priority, replay_gap_through, \
                        presence_payloads \
                 FROM sm_sessions",
                (),
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?
        {
            out.push(decode_session(&row)?);
        }
        Ok(out)
    }

    /// Single-query JOIN that fetches every persisted SM session
    /// AND its unacked queue in one round-trip (issue #209 PR #405).
    async fn list_all_sessions_with_unacked(
        &self,
    ) -> Result<Vec<(PersistedSession, Vec<PersistedUnackedStanza>)>, SmPersistenceError> {
        joined_sessions::list_all_sessions_with_unacked(self).await
    }

    /// Atomically write a session record + its unacked queue (issue
    /// #209 PR #405). Wraps the upsert and N appends in a single
    /// `Database::begin` transaction so a panic / process crash
    /// mid-batch leaves the durable view consistent.
    async fn store_session_atomic(
        &self,
        session: PersistedSession,
        unacked: Vec<PersistedUnackedStanza>,
    ) -> Result<(), SmPersistenceError> {
        atomic_store::store_session_atomic(self, session, unacked).await
    }
}

/// Choose between the portable [`DatabaseSmPersistence`] and the
/// Postgres-fenced `PostgresFencedSmPersistence` (ADR-0017 Phase 3 Slice
/// 4), gated on `clustering.enabled` — matching element 1's "cluster mode
/// selects a Postgres-only fenced implementation... The fenced impl is
/// chosen when `clustering.enabled`" verbatim.
///
/// All `clustering`-feature conditional compilation is contained in this
/// one function (rather than scattered across the call site in
/// `server/http.rs`) so ordinary construction code stays a single,
/// unconditional call regardless of build configuration — "config-driven,
/// no cfg-flag leak into the hot path."
///
/// `claim_pair` is `Some((claim_store, node_identity))` only when the
/// clustering subsystem actually started and handed back live handles
/// (`clustering::ClusteringHandles::claim_pair`); it is `None` whenever
/// clustering is disabled, this binary lacks the `clustering` feature, or
/// (defensively) the subsystem started without producing handles. Any of
/// those cases when clustering is disabled. When clustering is enabled,
/// Postgres co-location and live claim handles are mandatory: falling back
/// to portable persistence would make the claim reaper query a different
/// (or nonexistent) `sm_sessions` table and violate ownership fencing.
///
/// `global_db` is the same [`Database`] handle `clustering::start_if_enabled`
/// itself received (`db_pool.global()`) — FIX 4: the fenced impl is
/// constructed by cloning *this* handle, never by opening a second,
/// independently-resolved pool from `database_url`. Before that clone even
/// happens, `database_url`'s resolved DSN is compared against
/// `global_db.database_url()`; a mismatch while clustering is enabled fails
/// startup outright with [`SmPersistenceError::ClusterColocationMismatch`]
/// (both URLs redacted first — DSNs commonly carry credentials) rather than
/// silently fencing `sm_sessions`/`sm_unacked` writes against a
/// `clustering_claims` table that may not even exist in whatever database
/// `database_url` actually points at.
pub async fn open_for_cluster_mode(
    database_url: Option<&str>,
    clustering_enabled: bool,
    claim_pair: Option<(
        std::sync::Arc<dyn waddle_xmpp::ownership::ClaimStore>,
        waddle_xmpp::ownership::SharedNodeIdentity,
    )>,
    global_db: &Database,
) -> Result<Arc<dyn SmPersistenceStorage>, SmPersistenceError> {
    #[cfg(feature = "clustering")]
    {
        if clustering_enabled {
            let resolved_sm_url = database_url
                .filter(|url| url.starts_with("postgres://") || url.starts_with("postgresql://"))
                .ok_or_else(|| SmPersistenceError::ClusterRequiresPostgres {
                    sm_database_url: database_url
                        .map(crate::db::redact_database_url)
                        .unwrap_or_else(|| "<unset>".to_string()),
                })?;
            // FIX 4 — co-location invariant, checked before anything else
            // in this branch runs: clustered SM persistence and the
            // clustering claims tables must live in the same Postgres
            // database. Deliberately an EXACT string comparison, no DSN
            // normalization: two cosmetically-different-but-equivalent
            // URLs fail closed at startup with a clear error, which is
            // cheaper than parsing DSN equivalence and never unsafe.
            if resolved_sm_url != global_db.database_url() {
                return Err(SmPersistenceError::ClusterColocationMismatch {
                    sm_database_url: crate::db::redact_database_url(resolved_sm_url),
                    global_database_url: crate::db::redact_database_url(global_db.database_url()),
                });
            }
            let (claim_store, node_identity) =
                claim_pair.ok_or(SmPersistenceError::ClusterClaimHandlesUnavailable)?;
            let fenced = crate::sm_persistence_fenced::PostgresFencedSmPersistence::open(
                global_db.clone(),
                claim_store,
                node_identity,
            )
            .await?;
            return Ok(Arc::new(fenced));
        }
    }
    #[cfg(not(feature = "clustering"))]
    {
        let _ = (claim_pair, global_db);
        if clustering_enabled {
            return Err(SmPersistenceError::ClusterClaimHandlesUnavailable);
        }
    }
    Ok(Arc::new(DatabaseSmPersistence::open(database_url).await?))
}

#[cfg(test)]
mod tests;
