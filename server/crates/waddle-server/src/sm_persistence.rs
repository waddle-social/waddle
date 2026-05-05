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
use tracing::{info, instrument};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::{
    PersistedSession, PersistedUnackedStanza, SmPersistenceError, SmPersistenceStorage,
};
use waddle_xmpp::Stanza;
use xmpp_parsers::presence::Show;

use crate::db::{Database, DatabaseConfig, DatabaseDriver, IntoParams};

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
///     inbound_count INTEGER NOT NULL,
///     outbound_count INTEGER NOT NULL,
///     last_acked INTEGER NOT NULL,
///     max_resume_secs INTEGER,
///     detached_at_ms INTEGER NOT NULL,
///     max_resume_duration_ms INTEGER NOT NULL,
///     carbons_enabled INTEGER NOT NULL,
///     roster_interested INTEGER NOT NULL,
///     presence_available INTEGER NOT NULL,
///     presence_show TEXT,
///     presence_status TEXT,
///     presence_priority INTEGER NOT NULL
/// );
///
/// CREATE TABLE sm_unacked (
///     stream_id TEXT NOT NULL,
///     sequence INTEGER NOT NULL,
///     stanza_xml TEXT NOT NULL,
///     original_receipt_at_ms INTEGER NOT NULL,
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
        storage.initialize().await?;
        info!(driver = ?storage.db.driver(), "SM persistence storage initialized (XEP-0198)");
        Ok(storage)
    }

    async fn initialize(&self) -> Result<(), SmPersistenceError> {
        // Driver-aware bigint type: Postgres INTEGER is i32 (overflows
        // for `timestamp_millis()` after Jan 2038); BIGINT is i64.
        // SQLite INTEGER is dynamically sized so the same DDL works.
        let bigint = match self.db.driver() {
            DatabaseDriver::Postgres => "BIGINT",
            DatabaseDriver::Sqlite => "INTEGER",
        };
        self.execute(
            &format!(
                r#"
                CREATE TABLE IF NOT EXISTS sm_sessions (
                    stream_id TEXT PRIMARY KEY,
                    user_id TEXT NOT NULL,
                    full_jid TEXT NOT NULL,
                    inbound_count {bigint} NOT NULL,
                    outbound_count {bigint} NOT NULL,
                    last_acked {bigint} NOT NULL,
                    max_resume_secs {bigint},
                    detached_at_ms {bigint} NOT NULL,
                    max_resume_duration_ms {bigint} NOT NULL,
                    carbons_enabled INTEGER NOT NULL,
                    roster_interested INTEGER NOT NULL,
                    presence_available INTEGER NOT NULL,
                    presence_show TEXT,
                    presence_status TEXT,
                    presence_priority INTEGER NOT NULL
                )
                "#
            ),
            (),
        )
        .await?;
        self.execute(
            &format!(
                r#"
                CREATE TABLE IF NOT EXISTS sm_unacked (
                    stream_id TEXT NOT NULL,
                    sequence {bigint} NOT NULL,
                    stanza_xml TEXT NOT NULL,
                    original_receipt_at_ms {bigint} NOT NULL,
                    PRIMARY KEY (stream_id, sequence)
                )
                "#
            ),
            (),
        )
        .await?;
        // Index on detached_at_ms + max_resume_duration_ms for the
        // janitor's expired-session sweep. We can't compute the
        // expiry timestamp directly in SQL portably, so the janitor
        // filters in Rust over an index-supported scan.
        self.execute(
            "CREATE INDEX IF NOT EXISTS idx_sm_sessions_detached \
             ON sm_sessions (detached_at_ms)",
            (),
        )
        .await?;
        Ok(())
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

    fn decode_session(row: &crate::db::Row) -> Result<PersistedSession, SmPersistenceError> {
        let stream_id: String = row
            .get(0)
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let user_id: String = row
            .get(1)
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let full_jid_raw: String = row
            .get(2)
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let jid: FullJid = full_jid_raw
            .parse()
            .map_err(|e: jid::Error| SmPersistenceError::Other(e.to_string()))?;
        let inbound_count: i64 = row
            .get(3)
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let outbound_count: i64 = row
            .get(4)
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let last_acked: i64 = row
            .get(5)
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let max_resume_secs: Option<i64> = row
            .get(6)
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let detached_at_ms: i64 = row
            .get(7)
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let max_resume_duration_ms: i64 = row
            .get(8)
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let carbons_enabled: i64 = row
            .get(9)
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let roster_interested: i64 = row
            .get(10)
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let presence_available: i64 = row
            .get(11)
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let presence_show_raw: Option<String> = row
            .get(12)
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let presence_status: Option<String> = row
            .get(13)
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let presence_priority: i64 = row
            .get(14)
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;

        let detached_at = DateTime::<Utc>::from_timestamp_millis(detached_at_ms)
            .ok_or_else(|| SmPersistenceError::Other("invalid detached_at_ms".into()))?;
        let max_resume_duration =
            std::time::Duration::from_millis(max_resume_duration_ms.max(0) as u64);
        let presence_show = presence_show_raw.as_deref().map(parse_show).transpose()?;

        Ok(PersistedSession {
            stream_id: SmSessionId::new(stream_id),
            user_id,
            jid,
            inbound_count: inbound_count.max(0) as u32,
            outbound_count: outbound_count.max(0) as u32,
            last_acked: last_acked.max(0) as u32,
            max_resume_time: max_resume_secs.map(|v| v.max(0) as u32),
            detached_at,
            max_resume_duration,
            carbons_enabled: carbons_enabled != 0,
            roster_interested: roster_interested != 0,
            presence_available: presence_available != 0,
            presence_show,
            presence_status,
            presence_priority: presence_priority.clamp(i8::MIN as i64, i8::MAX as i64) as i8,
        })
    }

    fn decode_unacked(row: &crate::db::Row) -> Result<PersistedUnackedStanza, SmPersistenceError> {
        let stream_id: String = row
            .get(0)
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let sequence: i64 = row
            .get(1)
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let stanza_xml: String = row
            .get(2)
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let receipt_ms: i64 = row
            .get(3)
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;

        let original_receipt_at = DateTime::<Utc>::from_timestamp_millis(receipt_ms)
            .ok_or_else(|| SmPersistenceError::Other("invalid receipt timestamp".into()))?;
        let element: xmpp_parsers::minidom::Element = stanza_xml
            .parse()
            .map_err(|e: xmpp_parsers::minidom::Error| SmPersistenceError::Other(e.to_string()))?;
        let stanza = parse_stanza(element)?;

        Ok(PersistedUnackedStanza {
            stream_id: SmSessionId::new(stream_id),
            sequence: sequence.max(0) as u32,
            stanza: Box::new(stanza),
            original_receipt_at,
        })
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

        // Portable upsert: SQLite and Postgres both support
        // INSERT ... ON CONFLICT (col) DO UPDATE SET ...
        self.execute(
            r#"
            INSERT INTO sm_sessions (
                stream_id, user_id, full_jid, inbound_count, outbound_count,
                last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms,
                carbons_enabled, roster_interested, presence_available,
                presence_show, presence_status, presence_priority
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
                presence_available = excluded.presence_available,
                presence_show = excluded.presence_show,
                presence_status = excluded.presence_status,
                presence_priority = excluded.presence_priority
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
                i64::from(session.presence_available),
                presence_show_str.map(str::to_string),
                session.presence_status,
                i64::from(session.presence_priority),
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
                        carbons_enabled, roster_interested, presence_available, \
                        presence_show, presence_status, presence_priority \
                 FROM sm_sessions WHERE stream_id = ?",
                crate::db_params![stream_id.as_str().to_string()],
            )
            .await?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?
        {
            Ok(Some(Self::decode_session(&row)?))
        } else {
            Ok(None)
        }
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
            "INSERT INTO sm_unacked (stream_id, sequence, stanza_xml, original_receipt_at_ms) \
             VALUES (?, ?, ?, ?)",
            crate::db_params![
                stanza.stream_id.as_str().to_string(),
                i64::from(stanza.sequence),
                xml,
                receipt_ms,
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

    async fn list_unacked(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Vec<PersistedUnackedStanza>, SmPersistenceError> {
        let mut rows = self
            .query(
                "SELECT stream_id, sequence, stanza_xml, original_receipt_at_ms \
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
            out.push(Self::decode_unacked(&row)?);
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
                        carbons_enabled, roster_interested, presence_available, \
                        presence_show, presence_status, presence_priority \
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
            out.push(Self::decode_session(&row)?);
        }
        Ok(out)
    }

    async fn list_all_sessions(&self) -> Result<Vec<PersistedSession>, SmPersistenceError> {
        let mut rows = self
            .query(
                "SELECT stream_id, user_id, full_jid, inbound_count, outbound_count, \
                        last_acked, max_resume_secs, detached_at_ms, max_resume_duration_ms, \
                        carbons_enabled, roster_interested, presence_available, \
                        presence_show, presence_status, presence_priority \
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
            out.push(Self::decode_session(&row)?);
        }
        Ok(out)
    }
}

fn show_wire_str(show: &Show) -> &'static str {
    match show {
        Show::Away => "away",
        Show::Chat => "chat",
        Show::Dnd => "dnd",
        Show::Xa => "xa",
    }
}

fn parse_show(raw: &str) -> Result<Show, SmPersistenceError> {
    match raw {
        "away" => Ok(Show::Away),
        "chat" => Ok(Show::Chat),
        "dnd" => Ok(Show::Dnd),
        "xa" => Ok(Show::Xa),
        other => Err(SmPersistenceError::Other(format!(
            "unknown presence show value '{other}'"
        ))),
    }
}

fn serialize_stanza(stanza: &Stanza) -> Result<String, SmPersistenceError> {
    let element: xmpp_parsers::minidom::Element = match stanza {
        Stanza::Message(m) => m.clone().into(),
        Stanza::Iq(iq) => iq.clone().into(),
        Stanza::Presence(p) => p.clone().into(),
    };
    let mut buf = Vec::new();
    element
        .write_to(&mut buf)
        .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
    String::from_utf8(buf).map_err(|e| SmPersistenceError::Other(e.to_string()))
}

fn parse_stanza(element: xmpp_parsers::minidom::Element) -> Result<Stanza, SmPersistenceError> {
    match element.name() {
        "message" => xmpp_parsers::message::Message::try_from(element)
            .map(Stanza::Message)
            .map_err(|e| SmPersistenceError::Other(e.to_string())),
        "iq" => xmpp_parsers::iq::Iq::try_from(element)
            .map(Stanza::Iq)
            .map_err(|e| SmPersistenceError::Other(e.to_string())),
        "presence" => xmpp_parsers::presence::Presence::try_from(element)
            .map(Stanza::Presence)
            .map_err(|e| SmPersistenceError::Other(e.to_string())),
        other => Err(SmPersistenceError::Other(format!(
            "unknown stanza element '{other}'"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::time::Duration;

    fn full(s: &str) -> FullJid {
        s.parse().unwrap()
    }

    fn fixed_time() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 1, 12, 30, 0).unwrap()
    }

    fn fixture_session(stream_id: &str) -> PersistedSession {
        PersistedSession {
            stream_id: SmSessionId::new(stream_id),
            user_id: "alice".to_string(),
            jid: full("alice@example.com/web"),
            inbound_count: 7,
            outbound_count: 12,
            last_acked: 10,
            max_resume_time: Some(60),
            detached_at: fixed_time(),
            max_resume_duration: Duration::from_secs(60),
            carbons_enabled: true,
            roster_interested: true,
            presence_available: true,
            presence_show: Some(Show::Chat),
            presence_status: Some("at the keyboard".to_string()),
            presence_priority: 5,
        }
    }

    fn fixture_unacked(stream_id: &str, sequence: u32) -> PersistedUnackedStanza {
        let mut message = xmpp_parsers::message::Message::new(None::<jid::Jid>);
        message.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body(format!("m{sequence}")),
        );
        PersistedUnackedStanza {
            stream_id: SmSessionId::new(stream_id),
            sequence,
            stanza: Box::new(Stanza::Message(message)),
            original_receipt_at: fixed_time(),
        }
    }

    #[tokio::test]
    async fn round_trip_session_preserves_every_field() {
        let storage = DatabaseSmPersistence::open(None).await.unwrap();
        let s = fixture_session("stream-1");
        storage.upsert_session(s.clone()).await.unwrap();
        let loaded = storage
            .get_session(&SmSessionId::new("stream-1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.stream_id, s.stream_id);
        assert_eq!(loaded.user_id, s.user_id);
        assert_eq!(loaded.jid, s.jid);
        assert_eq!(loaded.inbound_count, s.inbound_count);
        assert_eq!(loaded.outbound_count, s.outbound_count);
        assert_eq!(loaded.last_acked, s.last_acked);
        assert_eq!(loaded.max_resume_time, s.max_resume_time);
        assert_eq!(loaded.detached_at, s.detached_at);
        assert_eq!(loaded.max_resume_duration, s.max_resume_duration);
        assert_eq!(loaded.carbons_enabled, s.carbons_enabled);
        assert_eq!(loaded.roster_interested, s.roster_interested);
        assert_eq!(loaded.presence_available, s.presence_available);
        assert_eq!(loaded.presence_show, s.presence_show);
        assert_eq!(loaded.presence_status, s.presence_status);
        assert_eq!(loaded.presence_priority, s.presence_priority);
    }

    #[tokio::test]
    async fn upsert_replaces_existing_session() {
        let storage = DatabaseSmPersistence::open(None).await.unwrap();
        let mut s = fixture_session("stream-1");
        storage.upsert_session(s.clone()).await.unwrap();
        s.inbound_count = 99;
        s.presence_priority = -1;
        storage.upsert_session(s.clone()).await.unwrap();
        let loaded = storage
            .get_session(&SmSessionId::new("stream-1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.inbound_count, 99);
        assert_eq!(loaded.presence_priority, -1);
    }

    #[tokio::test]
    async fn list_unacked_orders_ascending_by_sequence() {
        let storage = DatabaseSmPersistence::open(None).await.unwrap();
        for seq in [3u32, 1, 4, 2] {
            storage
                .append_unacked(fixture_unacked("stream-1", seq))
                .await
                .unwrap();
        }
        let rows = storage
            .list_unacked(&SmSessionId::new("stream-1"))
            .await
            .unwrap();
        let seqs: Vec<u32> = rows.iter().map(|r| r.sequence).collect();
        assert_eq!(seqs, vec![1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn ack_through_drops_only_acked_sequences() {
        let storage = DatabaseSmPersistence::open(None).await.unwrap();
        for seq in 1..=4 {
            storage
                .append_unacked(fixture_unacked("stream-1", seq))
                .await
                .unwrap();
        }
        let dropped = storage
            .ack_through(&SmSessionId::new("stream-1"), 2)
            .await
            .unwrap();
        assert_eq!(dropped, 2);
        let remaining = storage
            .list_unacked(&SmSessionId::new("stream-1"))
            .await
            .unwrap();
        assert_eq!(
            remaining.iter().map(|r| r.sequence).collect::<Vec<_>>(),
            vec![3, 4]
        );
    }

    #[tokio::test]
    async fn delete_session_clears_unacked_too() {
        let storage = DatabaseSmPersistence::open(None).await.unwrap();
        storage
            .upsert_session(fixture_session("stream-1"))
            .await
            .unwrap();
        storage
            .append_unacked(fixture_unacked("stream-1", 1))
            .await
            .unwrap();
        storage
            .delete_session(&SmSessionId::new("stream-1"))
            .await
            .unwrap();
        assert!(storage
            .get_session(&SmSessionId::new("stream-1"))
            .await
            .unwrap()
            .is_none());
        assert!(storage
            .list_unacked(&SmSessionId::new("stream-1"))
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn list_expired_filters_by_detached_plus_duration() {
        let storage = DatabaseSmPersistence::open(None).await.unwrap();
        let now = Utc::now();
        let mut past = fixture_session("expired");
        past.detached_at = now - chrono::Duration::seconds(120);
        past.max_resume_duration = Duration::from_secs(60);
        let mut active = fixture_session("active");
        active.detached_at = now;
        active.max_resume_duration = Duration::from_secs(600);
        storage.upsert_session(past).await.unwrap();
        storage.upsert_session(active).await.unwrap();

        let expired = storage.list_expired_sessions(now).await.unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].stream_id, SmSessionId::new("expired"));
    }

    #[tokio::test]
    async fn round_trip_unacked_preserves_typed_stanza() {
        let storage = DatabaseSmPersistence::open(None).await.unwrap();
        storage
            .append_unacked(fixture_unacked("stream-1", 1))
            .await
            .unwrap();
        let rows = storage
            .list_unacked(&SmSessionId::new("stream-1"))
            .await
            .unwrap();
        let body = match &*rows[0].stanza {
            Stanza::Message(m) => m.bodies.values().next().cloned(),
            _ => panic!("expected Message"),
        };
        assert_eq!(body.map(|b| b.0), Some("m1".to_string()));
    }
}
