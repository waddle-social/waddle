//! Typed CRUD on top of [`super::schema`]'s two tables.
//!
//! This module is deliberately "dumb": it knows how to enqueue, fetch a due
//! batch, mark a row done, record a per-row failure, and insert a judgment
//! result. Retry/backoff *policy* (when to give up on a row) lives in
//! [`super::drain`], not here.

use jid::Jid;
use waddle_xmpp::muc::durable::WaddleId;
use waddle_xmpp_core::xep0359::StanzaId;

use super::MessageJudgmentOutboxError;
use crate::db::{Database, DatabaseDriver, Row};

/// `message_judgments.judgment_name` value for this phase's `is_question`
/// judgment. A real column (not hard-coded into the table shape) so future
/// judgment kinds reuse `message_judgments` without a schema change.
pub const IS_QUESTION_JUDGMENT_NAME: &str = "is_question";

/// Cap on the persisted body snapshot. This table is not the canonical
/// message store — just enough context for the judge call — so the
/// snapshot is truncated (on a `char` boundary) before insert.
pub const MAX_BODY_SNAPSHOT_CHARS: usize = 4_000;

/// Typed id for one `message_judgment_outbox` row. A UUID (v4), matching
/// this codebase's existing convention for outbox row ids
/// (`pending_delivery`'s `row_id`, `notification_outbox`'s `job_id`) rather
/// than a driver-specific autoincrement column.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MessageJudgmentOutboxId(String);

impl MessageJudgmentOutboxId {
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    fn from_stored(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Input to [`enqueue_pending`]. `body` is the raw, untruncated message
/// body; truncation to [`MAX_BODY_SNAPSHOT_CHARS`] happens inside
/// `enqueue_pending`, not at the call site.
pub struct PendingJudgmentInput {
    pub waddle_id: WaddleId,
    pub stanza_id: StanzaId,
    pub body: String,
    pub now_ms: i64,
}

/// One due row from `message_judgment_outbox`.
#[derive(Debug, Clone)]
pub struct PendingJudgmentRow {
    pub id: MessageJudgmentOutboxId,
    pub waddle_id: WaddleId,
    pub stanza_id: StanzaId,
    pub body_snapshot: String,
    pub available_at_ms: i64,
    pub attempt_count: i64,
    pub last_error: Option<String>,
    pub created_at_ms: i64,
}

/// Input to [`insert_judgment`]: one result row for `message_judgments`.
pub struct JudgmentRecord {
    pub waddle_id: WaddleId,
    pub stanza_id: StanzaId,
    pub judgment_name: String,
    pub taxonomy_version: String,
    pub model_version: String,
    pub probability: f64,
    pub confidence: f64,
    pub decided_at_ms: i64,
    pub created_at_ms: i64,
}

const OUTBOX_SELECT_COLUMNS: &str = "id, waddle_id, stanza_id, stanza_by, body_snapshot, \
     available_at_ms, attempt_count, last_error, created_at_ms";

/// Enqueue one message for judgment. Plain insert, no dedup — the queue
/// itself may carry duplicates (e.g. a retried enqueue); `message_judgments`
/// is where idempotency is enforced (see [`insert_judgment`]).
pub async fn enqueue_pending(
    db: &Database,
    input: PendingJudgmentInput,
) -> Result<(), MessageJudgmentOutboxError> {
    let id = MessageJudgmentOutboxId::generate();
    let body_snapshot: String = input.body.chars().take(MAX_BODY_SNAPSHOT_CHARS).collect();
    let connection = db.guard().await?;
    connection
        .execute(
            "INSERT INTO message_judgment_outbox \
             (id, waddle_id, stanza_id, stanza_by, body_snapshot, available_at_ms, \
              attempt_count, last_error, done, created_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, 0, NULL, FALSE, ?)",
            crate::db_params![
                id.as_str(),
                input.waddle_id.as_str(),
                input.stanza_id.id.as_str(),
                input.stanza_id.by.to_string(),
                body_snapshot,
                input.now_ms,
                input.now_ms,
            ],
        )
        .await?;
    Ok(())
}

/// Fetch up to `limit` not-yet-done rows whose `available_at_ms` has
/// elapsed, oldest first.
pub async fn fetch_due_batch(
    db: &Database,
    limit: i64,
    now_ms: i64,
) -> Result<Vec<PendingJudgmentRow>, MessageJudgmentOutboxError> {
    let connection = db.guard().await?;
    let mut rows = connection
        .query(
            &format!(
                "SELECT {OUTBOX_SELECT_COLUMNS} FROM message_judgment_outbox \
                 WHERE NOT done AND available_at_ms <= ? \
                 ORDER BY available_at_ms, id \
                 LIMIT ?"
            ),
            crate::db_params![now_ms, limit.clamp(1, 1_000)],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(decode_pending_row(&row)?);
    }
    Ok(out)
}

/// Mark a row done after a successful judgment — permanently removing it
/// from the due set without touching `last_error` (there is none to record).
/// A row that instead exhausted its retry budget is dead-lettered via
/// [`dead_letter`], not this function, so its terminal failure reason is
/// captured too. Rows are never deleted, only marked done, so the queue
/// table also serves as a durable audit trail.
pub async fn mark_done(
    db: &Database,
    id: &MessageJudgmentOutboxId,
) -> Result<(), MessageJudgmentOutboxError> {
    let connection = db.guard().await?;
    connection
        .execute(
            "UPDATE message_judgment_outbox SET done = TRUE WHERE id = ?",
            crate::db_params![id.as_str()],
        )
        .await?;
    Ok(())
}

/// Dead-letter a row that exceeded the retry budget: increments
/// `attempt_count` and stores the terminal `error` (unlike [`mark_done`],
/// which leaves `last_error` untouched), then marks the row done in the same
/// statement. Without this, the row's final failure reason would never be
/// persisted — only earlier attempts' errors (recorded by
/// [`record_failure`]) would appear in the durable audit trail, silently
/// dropping the one error that actually caused the row to stop being
/// retried.
pub async fn dead_letter(
    db: &Database,
    id: &MessageJudgmentOutboxId,
    error: &str,
) -> Result<(), MessageJudgmentOutboxError> {
    let connection = db.guard().await?;
    connection
        .execute(
            "UPDATE message_judgment_outbox \
             SET attempt_count = attempt_count + 1, last_error = ?, done = TRUE \
             WHERE id = ?",
            crate::db_params![error, id.as_str()],
        )
        .await?;
    Ok(())
}

/// Record one failed judgment attempt: increments `attempt_count`, stores
/// `error`, and reschedules `available_at_ms` to `next_attempt_at_ms`. Does
/// not itself decide whether the row should be dead-lettered instead — that
/// policy lives in `drain::drain_once`, which calls [`dead_letter`] directly
/// once `drain::MAX_ATTEMPTS` is reached rather than calling this function.
pub async fn record_failure(
    db: &Database,
    id: &MessageJudgmentOutboxId,
    error: &str,
    next_attempt_at_ms: i64,
) -> Result<(), MessageJudgmentOutboxError> {
    let connection = db.guard().await?;
    connection
        .execute(
            "UPDATE message_judgment_outbox \
             SET attempt_count = attempt_count + 1, last_error = ?, available_at_ms = ? \
             WHERE id = ?",
            crate::db_params![error, next_attempt_at_ms, id.as_str()],
        )
        .await?;
    Ok(())
}

/// Insert one judgment result. Idempotent under replay: a second insert for
/// the same `(stanza_id, judgment_name, model_version)` is a no-op (never a
/// duplicate row, never an error) via `ON CONFLICT ... DO NOTHING`
/// (Postgres) / `INSERT OR IGNORE` (SQLite), backed by the
/// `message_judgments_identity_idx` unique index.
pub async fn insert_judgment(
    db: &Database,
    record: JudgmentRecord,
) -> Result<(), MessageJudgmentOutboxError> {
    let id = uuid::Uuid::new_v4().to_string();
    let sql = match db.driver() {
        DatabaseDriver::Postgres => {
            "INSERT INTO message_judgments \
             (id, waddle_id, stanza_id, stanza_by, judgment_name, taxonomy_version, \
              model_version, probability, confidence, decided_at_ms, created_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (stanza_id, judgment_name, model_version) DO NOTHING"
        }
        DatabaseDriver::Sqlite => {
            "INSERT OR IGNORE INTO message_judgments \
             (id, waddle_id, stanza_id, stanza_by, judgment_name, taxonomy_version, \
              model_version, probability, confidence, decided_at_ms, created_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        }
    };
    let connection = db.guard().await?;
    connection
        .execute(
            sql,
            crate::db_params![
                id,
                record.waddle_id.as_str(),
                record.stanza_id.id.as_str(),
                record.stanza_id.by.to_string(),
                record.judgment_name.as_str(),
                record.taxonomy_version.as_str(),
                record.model_version.as_str(),
                record.probability,
                record.confidence,
                record.decided_at_ms,
                record.created_at_ms,
            ],
        )
        .await?;
    Ok(())
}

fn decode_pending_row(row: &Row) -> Result<PendingJudgmentRow, MessageJudgmentOutboxError> {
    let id: String = row.get(0)?;
    let waddle_id: String = row.get(1)?;
    let stanza_id: String = row.get(2)?;
    let stanza_by: String = row.get(3)?;
    let by: Jid = stanza_by
        .parse()
        .map_err(|_| MessageJudgmentOutboxError::InvalidStanzaByJid(stanza_by.clone()))?;
    Ok(PendingJudgmentRow {
        id: MessageJudgmentOutboxId::from_stored(id),
        waddle_id: WaddleId::new(waddle_id),
        stanza_id: StanzaId::new(stanza_id, by),
        body_snapshot: row.get(4)?,
        available_at_ms: row.get(5)?,
        attempt_count: row.get(6)?,
        last_error: row.get(7)?,
        created_at_ms: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn waddle_id() -> WaddleId {
        WaddleId::new("default".to_string())
    }

    fn stanza(id: &str) -> StanzaId {
        StanzaId::new(
            id.to_string(),
            "room@conference.example.test".parse().expect("room jid"),
        )
    }

    async fn test_db() -> Database {
        Database::in_memory(&format!(
            "message-judgment-outbox-store-{}",
            uuid::Uuid::new_v4()
        ))
        .await
        .expect("in-memory database")
    }

    #[tokio::test]
    async fn schema_initializes_cleanly() {
        let db = test_db().await;
        super::super::schema::initialize(&db)
            .await
            .expect("initialize");
        // Re-running initialize (e.g. a second process boot) must also be
        // a no-op, not an error.
        super::super::schema::initialize(&db)
            .await
            .expect("reinitialize");
    }

    #[tokio::test]
    async fn enqueue_then_fetch_due_batch_round_trips() {
        let db = test_db().await;
        super::super::schema::initialize(&db)
            .await
            .expect("initialize");

        enqueue_pending(
            &db,
            PendingJudgmentInput {
                waddle_id: waddle_id(),
                stanza_id: stanza("stanza-1"),
                body: "is this a question?".to_string(),
                now_ms: 1_000,
            },
        )
        .await
        .expect("enqueue");

        // Not yet due.
        let none_due = fetch_due_batch(&db, 10, 500).await.expect("fetch");
        assert!(none_due.is_empty());

        let due = fetch_due_batch(&db, 10, 1_000).await.expect("fetch");
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].waddle_id.as_str(), "default");
        assert_eq!(due[0].stanza_id.as_str(), "stanza-1");
        assert_eq!(due[0].body_snapshot, "is this a question?");
        assert_eq!(due[0].attempt_count, 0);
        assert!(due[0].last_error.is_none());
    }

    #[tokio::test]
    async fn enqueue_truncates_oversized_body() {
        let db = test_db().await;
        super::super::schema::initialize(&db)
            .await
            .expect("initialize");
        let body = "x".repeat(MAX_BODY_SNAPSHOT_CHARS + 500);

        enqueue_pending(
            &db,
            PendingJudgmentInput {
                waddle_id: waddle_id(),
                stanza_id: stanza("stanza-long"),
                body,
                now_ms: 1_000,
            },
        )
        .await
        .expect("enqueue");

        let due = fetch_due_batch(&db, 10, 1_000).await.expect("fetch");
        assert_eq!(due.len(), 1);
        assert_eq!(
            due[0].body_snapshot.chars().count(),
            MAX_BODY_SNAPSHOT_CHARS
        );
    }

    #[tokio::test]
    async fn mark_done_removes_row_from_due_set() {
        let db = test_db().await;
        super::super::schema::initialize(&db)
            .await
            .expect("initialize");
        enqueue_pending(
            &db,
            PendingJudgmentInput {
                waddle_id: waddle_id(),
                stanza_id: stanza("stanza-done"),
                body: "body".to_string(),
                now_ms: 1_000,
            },
        )
        .await
        .expect("enqueue");
        let due = fetch_due_batch(&db, 10, 1_000).await.expect("fetch");
        assert_eq!(due.len(), 1);

        mark_done(&db, &due[0].id).await.expect("mark done");

        let due_after = fetch_due_batch(&db, 10, 1_000).await.expect("fetch");
        assert!(due_after.is_empty());
    }

    #[tokio::test]
    async fn record_failure_applies_backoff_and_increments_attempt_count() {
        let db = test_db().await;
        super::super::schema::initialize(&db)
            .await
            .expect("initialize");
        enqueue_pending(
            &db,
            PendingJudgmentInput {
                waddle_id: waddle_id(),
                stanza_id: stanza("stanza-fail"),
                body: "body".to_string(),
                now_ms: 1_000,
            },
        )
        .await
        .expect("enqueue");
        let due = fetch_due_batch(&db, 10, 1_000).await.expect("fetch");
        let id = due[0].id.clone();

        record_failure(&db, &id, "transport error", 50_000)
            .await
            .expect("record failure");

        // Not due again until the rescheduled time.
        assert!(fetch_due_batch(&db, 10, 2_000)
            .await
            .expect("fetch")
            .is_empty());
        let due_later = fetch_due_batch(&db, 10, 50_000).await.expect("fetch");
        assert_eq!(due_later.len(), 1);
        assert_eq!(due_later[0].attempt_count, 1);
        assert_eq!(due_later[0].last_error.as_deref(), Some("transport error"));
    }

    #[tokio::test]
    async fn dead_lettered_row_stops_being_returned_but_is_not_deleted() {
        let db = test_db().await;
        super::super::schema::initialize(&db)
            .await
            .expect("initialize");
        enqueue_pending(
            &db,
            PendingJudgmentInput {
                waddle_id: waddle_id(),
                stanza_id: stanza("stanza-dead"),
                body: "body".to_string(),
                now_ms: 1_000,
            },
        )
        .await
        .expect("enqueue");
        let due = fetch_due_batch(&db, 10, 1_000).await.expect("fetch");
        let id = due[0].id.clone();

        // Simulate the drain policy's give-up path directly (drain.rs owns
        // the MAX_ATTEMPTS decision; this exercises the same store-level
        // effect it produces).
        record_failure(&db, &id, "still failing", 2_000)
            .await
            .expect("record failure");
        mark_done(&db, &id).await.expect("dead-letter mark done");

        assert!(fetch_due_batch(&db, 10, i64::MAX)
            .await
            .expect("fetch")
            .is_empty());

        // Not deleted: the row is still present, just done.
        let connection = db.guard().await.expect("guard");
        let mut rows = connection
            .query(
                "SELECT done, attempt_count FROM message_judgment_outbox WHERE id = ?",
                crate::db_params![id.as_str()],
            )
            .await
            .expect("query");
        let row = rows.next().await.expect("row").expect("row present");
        assert!(row.get::<bool>(0).expect("done"));
        assert_eq!(row.get::<i64>(1).expect("attempt_count"), 1);
    }

    fn judgment(stanza_id: &str, model_version: &str) -> JudgmentRecord {
        JudgmentRecord {
            waddle_id: waddle_id(),
            stanza_id: stanza(stanza_id),
            judgment_name: IS_QUESTION_JUDGMENT_NAME.to_string(),
            taxonomy_version: "v1".to_string(),
            model_version: model_version.to_string(),
            probability: 0.9,
            confidence: 0.8,
            decided_at_ms: 1_000,
            created_at_ms: 1_000,
        }
    }

    #[tokio::test]
    async fn insert_judgment_is_idempotent_under_replay() {
        let db = test_db().await;
        super::super::schema::initialize(&db)
            .await
            .expect("initialize");

        insert_judgment(&db, judgment("stanza-idem", "model-a"))
            .await
            .expect("first insert");
        insert_judgment(&db, judgment("stanza-idem", "model-a"))
            .await
            .expect("replayed insert must not error");

        let connection = db.guard().await.expect("guard");
        let mut rows = connection
            .query(
                "SELECT COUNT(*) FROM message_judgments \
                 WHERE stanza_id = ? AND judgment_name = ? AND model_version = ?",
                crate::db_params!["stanza-idem", IS_QUESTION_JUDGMENT_NAME, "model-a"],
            )
            .await
            .expect("count query");
        let count: i64 = rows
            .next()
            .await
            .expect("row")
            .expect("row present")
            .get(0)
            .expect("count");
        assert_eq!(count, 1, "replayed insert must not create a second row");
    }

    #[tokio::test]
    async fn insert_judgment_allows_distinct_model_versions() {
        let db = test_db().await;
        super::super::schema::initialize(&db)
            .await
            .expect("initialize");

        insert_judgment(&db, judgment("stanza-multi", "model-a"))
            .await
            .expect("first insert");
        insert_judgment(&db, judgment("stanza-multi", "model-b"))
            .await
            .expect("distinct model version insert");

        let connection = db.guard().await.expect("guard");
        let mut rows = connection
            .query(
                "SELECT COUNT(*) FROM message_judgments WHERE stanza_id = ?",
                crate::db_params!["stanza-multi"],
            )
            .await
            .expect("count query");
        let count: i64 = rows
            .next()
            .await
            .expect("row")
            .expect("row present")
            .get(0)
            .expect("count");
        assert_eq!(count, 2, "distinct model_version must not collide");
    }
}
