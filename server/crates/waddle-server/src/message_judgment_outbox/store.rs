//! Typed CRUD on top of [`super::schema`]'s two tables.
//!
//! This module is deliberately "dumb": it knows how to enqueue, fetch a due
//! batch, mark a row done, record a per-row failure, and insert a judgment
//! result. Retry/backoff *policy* (when to give up on a row) lives in
//! [`super::drain`], not here.

use jid::Jid;
use waddle_xmpp::muc::durable::WaddleId;
use waddle_xmpp_core::xep0359::StanzaId;

use super::judge::JudgmentKind;
use super::MessageJudgmentOutboxError;
use crate::db::{Database, DatabaseDriver, Row};

/// `message_judgments.judgment_name` value for this phase's `is_question`
/// judgment. A real column (not hard-coded into the table shape) so future
/// judgment kinds reuse `message_judgments` without a schema change.
pub const IS_QUESTION_JUDGMENT_NAME: &str = "is_question";

/// Content-safety `judgment_name` values, one row per category per judged
/// message. Namespaced under `safety:` so they're visibly a family distinct
/// from `is_question` (a community-enrichment signal, not a safety one)
/// when scanning `message_judgments` directly. All five are asked together
/// in one Jev call — see `jev_client.rs`.
pub const SAFETY_HATE_SPEECH_JUDGMENT_NAME: &str = "safety:hate_speech";
pub const SAFETY_EXPLICIT_JUDGMENT_NAME: &str = "safety:explicit";
pub const SAFETY_HARASSMENT_JUDGMENT_NAME: &str = "safety:harassment";
pub const SAFETY_VIOLENCE_JUDGMENT_NAME: &str = "safety:violence";
pub const SAFETY_SELF_HARM_JUDGMENT_NAME: &str = "safety:self_harm";

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
    pub judgment_name: JudgmentKind,
    pub taxonomy_version: String,
    pub model_version: String,
    pub probability: f64,
    /// USD cost of the request that produced this judgment (Jev's
    /// `usage.cost`). Recorded per row rather than only sampled, since
    /// measuring cost-per-thousand-messages is this phase's stated purpose.
    pub cost_usd: f64,
    pub decided_at_ms: i64,
    pub created_at_ms: i64,
}

const OUTBOX_SELECT_COLUMNS: &str = "id, waddle_id, stanza_id, stanza_by, body_snapshot, \
     available_at_ms, attempt_count, last_error, created_at_ms";

const ENQUEUE_SQL: &str = "INSERT INTO message_judgment_outbox \
     (id, waddle_id, stanza_id, stanza_by, body_snapshot, available_at_ms, \
      attempt_count, last_error, done, created_at_ms) \
     VALUES (?, ?, ?, ?, ?, ?, 0, NULL, FALSE, ?)";

/// Enqueue one message for judgment. Plain insert, no dedup — the queue
/// itself may carry duplicates (e.g. a retried enqueue); `message_judgments`
/// is where idempotency is enforced (see [`insert_judgment`]).
///
/// Used by this module's own tests and by any future caller outside a live
/// ingress transaction. The real production enqueue site
/// (`ingress::durable::apply_durable`) uses [`enqueue_pending_in_tx`]
/// instead, so the row and the archive write it accompanies commit or roll
/// back together (#1831 Phase 2) — see that function's docs.
pub async fn enqueue_pending(
    db: &Database,
    input: PendingJudgmentInput,
) -> Result<(), MessageJudgmentOutboxError> {
    let id = MessageJudgmentOutboxId::generate();
    let body_snapshot: String = input.body.chars().take(MAX_BODY_SNAPSHOT_CHARS).collect();
    let connection = db.guard().await?;
    connection
        .execute(
            ENQUEUE_SQL,
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

/// Enqueue one message for judgment inside the caller's own transaction.
///
/// This is the real transactional-outbox seam (#1831 Phase 2): called from
/// `ingress::durable::apply_durable` (via
/// `ingress_uow::MessageJudgmentOutboxRepository::enqueue_in_tx`) on the
/// exact same [`crate::db::Transaction`] that just wrote the archive row,
/// before that transaction commits. Dropping the transaction without
/// committing — the same rollback-on-drop behaviour every other
/// `ingress_uow` repository write relies on — undoes this insert together
/// with the archive write, so the two can never observably diverge:
/// no crash window exists where one is durable and the other is not.
///
/// Mirrors [`enqueue_pending`] exactly (same columns, same no-dedup
/// semantics) but executes against a transaction handle instead of
/// checking out a pooled connection.
pub async fn enqueue_pending_in_tx(
    tx: &mut crate::db::Transaction<'_>,
    input: PendingJudgmentInput,
) -> Result<(), MessageJudgmentOutboxError> {
    let id = MessageJudgmentOutboxId::generate();
    let body_snapshot: String = input.body.chars().take(MAX_BODY_SNAPSHOT_CHARS).collect();
    tx.execute(
        ENQUEUE_SQL,
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
///
/// Guarded on `NOT done`: this outbox tolerates more than one drain worker
/// running concurrently (see the module docs), so it's possible for a
/// different worker to judge this exact row successfully — inserting a
/// judgment and marking it done — *between* this call's judge invocation
/// and this write. Without the guard, this call would win the race anyway
/// and overwrite a completed judgment's row with a stale terminal failure.
/// Returns `true` if this call's write actually applied, `false` if it was
/// a no-op because the row was already done.
pub async fn dead_letter(
    db: &Database,
    id: &MessageJudgmentOutboxId,
    error: &str,
) -> Result<bool, MessageJudgmentOutboxError> {
    let connection = db.guard().await?;
    let affected = connection
        .execute(
            "UPDATE message_judgment_outbox \
             SET attempt_count = attempt_count + 1, last_error = ?, done = TRUE \
             WHERE id = ? AND NOT done",
            crate::db_params![error, id.as_str()],
        )
        .await?;
    Ok(affected > 0)
}

/// Record one failed judgment attempt: increments `attempt_count`, stores
/// `error`, and reschedules `available_at_ms` to `next_attempt_at_ms`. Does
/// not itself decide whether the row should be dead-lettered instead — that
/// policy lives in `drain::drain_once`, which calls [`dead_letter`] directly
/// once `drain::MAX_ATTEMPTS` is reached rather than calling this function.
///
/// Guarded on `NOT done` for the same reason as [`dead_letter`]: a
/// concurrent drain worker can have already completed this row between this
/// call's judge invocation and this write. Returns `true` if this call's
/// write actually applied, `false` if it was a no-op because the row was
/// already done.
pub async fn record_failure(
    db: &Database,
    id: &MessageJudgmentOutboxId,
    error: &str,
    next_attempt_at_ms: i64,
) -> Result<bool, MessageJudgmentOutboxError> {
    let connection = db.guard().await?;
    let affected = connection
        .execute(
            "UPDATE message_judgment_outbox \
             SET attempt_count = attempt_count + 1, last_error = ?, available_at_ms = ? \
             WHERE id = ? AND NOT done",
            crate::db_params![error, next_attempt_at_ms, id.as_str()],
        )
        .await?;
    Ok(affected > 0)
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
              model_version, probability, cost_usd, decided_at_ms, created_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (stanza_id, judgment_name, model_version) DO NOTHING"
        }
        DatabaseDriver::Sqlite => {
            "INSERT OR IGNORE INTO message_judgments \
             (id, waddle_id, stanza_id, stanza_by, judgment_name, taxonomy_version, \
              model_version, probability, cost_usd, decided_at_ms, created_at_ms) \
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
                record.cost_usd,
                record.decided_at_ms,
                record.created_at_ms,
            ],
        )
        .await?;
    Ok(())
}

/// Insert every judgment in a batch and mark the outbox row done, in one
/// transaction: either every judgment is recorded and the row is marked
/// done, or none of it is and the row stays due for a clean retry from
/// scratch. A single call now answers several judgments at once (e.g.
/// `is_question` plus several `safety:*` categories); without this, a
/// failure partway through inserting them could leave some recorded and
/// others silently missing while the row is still marked done.
///
/// On a `(stanza_id, judgment_name, model_version)` conflict (a backfill
/// call re-judging a row that already has this exact judgment recorded),
/// every column except `cost_usd` is left at its first-written value —
/// but `cost_usd` is *added* to, not discarded: the batch this row's cost
/// is attributed to (see `drain.rs`'s cost-attribution comment) really did
/// cost money even when its result collides with an existing row, and
/// dropping that cost via a plain `DO NOTHING` would silently undercount
/// `SUM(cost_usd)` for exactly the calls a partial-backfill makes. This
/// keeps the invariant true unconditionally: `SUM(cost_usd)` always equals
/// the total cost of every Jev call ever made, whether or not its
/// judgments turned out to be new.
///
/// Guarded on `NOT done`, checked *before* any judgment is inserted, for
/// the same reason as [`dead_letter`]/[`record_failure`]: this outbox
/// tolerates more than one drain worker running concurrently, so a
/// different worker can have already finalized this row (e.g.
/// dead-lettered it) between this call's judge invocation and this write.
/// Without the guard, this call would still insert its (now-stale)
/// judgments and leave a row that shows a terminal failure sitting
/// alongside judgments recorded after that failure — an inconsistent
/// audit trail. Returns `true` if this call's write actually applied,
/// `false` if it was a no-op because the row was already done.
pub async fn insert_judgment_batch_and_mark_done(
    db: &Database,
    records: &[JudgmentRecord],
    outbox_id: &MessageJudgmentOutboxId,
) -> Result<bool, MessageJudgmentOutboxError> {
    // Both drivers support the same upsert grammar here, so one query
    // string covers both: on conflict, only `cost_usd` is touched (see the
    // doc comment above), and referencing it unqualified in the `SET`
    // clause (rather than `message_judgments.cost_usd`) is valid in both
    // Postgres and SQLite.
    let insert_sql = "INSERT INTO message_judgments \
         (id, waddle_id, stanza_id, stanza_by, judgment_name, taxonomy_version, \
          model_version, probability, cost_usd, decided_at_ms, created_at_ms) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (stanza_id, judgment_name, model_version) \
         DO UPDATE SET cost_usd = cost_usd + excluded.cost_usd";
    let mut tx = db.begin().await?;
    let claimed = tx
        .execute(
            "UPDATE message_judgment_outbox SET done = TRUE WHERE id = ? AND NOT done",
            crate::db_params![outbox_id.as_str()],
        )
        .await?;
    if claimed == 0 {
        // Lost the race: some other worker already finalized this row
        // (e.g. dead-lettered it) before this write. Roll back without
        // inserting anything -- these judgments are stale relative to
        // whatever already resolved the row.
        tx.rollback().await?;
        return Ok(false);
    }
    for record in records {
        let id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            insert_sql,
            crate::db_params![
                id,
                record.waddle_id.as_str(),
                record.stanza_id.id.as_str(),
                record.stanza_id.by.to_string(),
                record.judgment_name.as_str(),
                record.taxonomy_version.as_str(),
                record.model_version.as_str(),
                record.probability,
                record.cost_usd,
                record.decided_at_ms,
                record.created_at_ms,
            ],
        )
        .await?;
    }
    tx.commit().await?;
    Ok(true)
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
            judgment_name: JudgmentKind::IsQuestion,
            taxonomy_version: "v1".to_string(),
            model_version: model_version.to_string(),
            probability: 0.9,
            cost_usd: 0.00002,
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

    #[tokio::test]
    async fn dead_letter_is_a_no_op_when_row_already_done() {
        // Simulates losing a race against a concurrent drain worker that
        // already completed this row between this call's judge invocation
        // and this write.
        let db = test_db().await;
        super::super::schema::initialize(&db)
            .await
            .expect("initialize");
        enqueue_pending(
            &db,
            PendingJudgmentInput {
                waddle_id: waddle_id(),
                stanza_id: stanza("stanza-raced"),
                body: "body".to_string(),
                now_ms: 1_000,
            },
        )
        .await
        .expect("enqueue");
        let due = fetch_due_batch(&db, 10, 1_000).await.expect("fetch");
        let id = due[0].id.clone();
        mark_done(&db, &id)
            .await
            .expect("simulate concurrent winner");

        let applied = dead_letter(&db, &id, "stale failure")
            .await
            .expect("dead_letter must not error on an already-done row");
        assert!(
            !applied,
            "dead_letter must be a no-op once the row is already done"
        );

        let connection = db.guard().await.expect("guard");
        let mut rows = connection
            .query(
                "SELECT attempt_count, last_error FROM message_judgment_outbox WHERE id = ?",
                crate::db_params![id.as_str()],
            )
            .await
            .expect("query");
        let row = rows.next().await.expect("row").expect("row present");
        assert_eq!(row.get::<i64>(0).expect("attempt_count"), 0);
        assert_eq!(row.get::<Option<String>>(1).expect("last_error"), None);
    }

    #[tokio::test]
    async fn record_failure_is_a_no_op_when_row_already_done() {
        let db = test_db().await;
        super::super::schema::initialize(&db)
            .await
            .expect("initialize");
        enqueue_pending(
            &db,
            PendingJudgmentInput {
                waddle_id: waddle_id(),
                stanza_id: stanza("stanza-raced-retry"),
                body: "body".to_string(),
                now_ms: 1_000,
            },
        )
        .await
        .expect("enqueue");
        let due = fetch_due_batch(&db, 10, 1_000).await.expect("fetch");
        let id = due[0].id.clone();
        mark_done(&db, &id)
            .await
            .expect("simulate concurrent winner");

        let applied = record_failure(&db, &id, "stale failure", 50_000)
            .await
            .expect("record_failure must not error on an already-done row");
        assert!(
            !applied,
            "record_failure must be a no-op once the row is already done"
        );

        let connection = db.guard().await.expect("guard");
        let mut rows = connection
            .query(
                "SELECT attempt_count, last_error FROM message_judgment_outbox WHERE id = ?",
                crate::db_params![id.as_str()],
            )
            .await
            .expect("query");
        let row = rows.next().await.expect("row").expect("row present");
        assert_eq!(row.get::<i64>(0).expect("attempt_count"), 0);
        assert_eq!(row.get::<Option<String>>(1).expect("last_error"), None);
    }

    #[tokio::test]
    async fn insert_judgment_batch_and_mark_done_is_a_no_op_when_row_already_done() {
        // Simulates losing a race against a concurrent drain worker (e.g. one
        // that dead-lettered this exact row) between this call's judge
        // invocation and this write.
        let db = test_db().await;
        super::super::schema::initialize(&db)
            .await
            .expect("initialize");
        enqueue_pending(
            &db,
            PendingJudgmentInput {
                waddle_id: waddle_id(),
                stanza_id: stanza("stanza-raced-batch"),
                body: "body".to_string(),
                now_ms: 1_000,
            },
        )
        .await
        .expect("enqueue");
        let due = fetch_due_batch(&db, 10, 1_000).await.expect("fetch");
        let id = due[0].id.clone();
        dead_letter(&db, &id, "a concurrent worker already gave up")
            .await
            .expect("simulate concurrent winner");

        let applied = insert_judgment_batch_and_mark_done(
            &db,
            &[judgment("stanza-raced-batch", "model-a")],
            &id,
        )
        .await
        .expect("insert_judgment_batch_and_mark_done must not error on an already-done row");
        assert!(
            !applied,
            "insert_judgment_batch_and_mark_done must be a no-op once the row is already done"
        );

        let connection = db.guard().await.expect("guard");
        let mut rows = connection
            .query(
                "SELECT COUNT(*) FROM message_judgments WHERE stanza_id = ?",
                crate::db_params!["stanza-raced-batch"],
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
        assert_eq!(
            count, 0,
            "no judgment must be inserted when the row was already done"
        );
    }

    #[tokio::test]
    async fn insert_judgment_batch_and_mark_done_accumulates_cost_on_conflict() {
        // Simulates a backfill: a row already has an `is_question` judgment
        // recorded (e.g. from before a new safety category was added), so a
        // later batch answering it again collides on
        // (stanza_id, judgment_name, model_version). That later call still
        // cost real money -- its cost must be added to the existing row's
        // cost_usd, not silently discarded.
        let db = test_db().await;
        super::super::schema::initialize(&db)
            .await
            .expect("initialize");
        enqueue_pending(
            &db,
            PendingJudgmentInput {
                waddle_id: waddle_id(),
                stanza_id: stanza("stanza-cost-accum"),
                body: "body".to_string(),
                now_ms: 1_000,
            },
        )
        .await
        .expect("enqueue");
        let due = fetch_due_batch(&db, 10, 1_000).await.expect("fetch");
        let first_id = due[0].id.clone();

        let mut first_record = judgment("stanza-cost-accum", "model-a");
        first_record.cost_usd = 0.00002;
        let first_applied = insert_judgment_batch_and_mark_done(&db, &[first_record], &first_id)
            .await
            .expect("first insert must not error");
        assert!(first_applied, "first insert must apply");

        // A second outbox row for the same stanza+judgment+model (as a
        // backfill re-judge would produce), carrying a different cost.
        enqueue_pending(
            &db,
            PendingJudgmentInput {
                waddle_id: waddle_id(),
                stanza_id: stanza("stanza-cost-accum"),
                body: "body".to_string(),
                now_ms: 2_000,
            },
        )
        .await
        .expect("enqueue second");
        let due_second = fetch_due_batch(&db, 10, 2_000)
            .await
            .expect("fetch")
            .into_iter()
            .find(|row| row.id != first_id)
            .expect("second row");
        let mut second_record = judgment("stanza-cost-accum", "model-a");
        second_record.probability = 0.5; // Different value: must not overwrite the first.
        second_record.cost_usd = 0.00003;
        let applied = insert_judgment_batch_and_mark_done(&db, &[second_record], &due_second.id)
            .await
            .expect("second insert must not error on a conflicting judgment identity");
        assert!(
            applied,
            "the outbox row itself is claimed and marked done even though its judgment collided"
        );

        let connection = db.guard().await.expect("guard");
        let mut rows = connection
            .query(
                "SELECT COUNT(*), probability, cost_usd FROM message_judgments \
                 WHERE stanza_id = ? AND judgment_name = ? AND model_version = ?",
                crate::db_params!["stanza-cost-accum", IS_QUESTION_JUDGMENT_NAME, "model-a"],
            )
            .await
            .expect("query");
        let row = rows.next().await.expect("row").expect("row present");
        assert_eq!(
            row.get::<i64>(0).expect("count"),
            1,
            "the conflicting judgment must not create a second row"
        );
        assert_eq!(
            row.get::<f64>(1).expect("probability"),
            0.9,
            "the first-written probability must survive the conflict, not the second call's"
        );
        assert_eq!(
            row.get::<f64>(2).expect("cost_usd"),
            0.00002 + 0.00003,
            "the second call's cost must be added, not discarded, even though its judgment collided"
        );
    }

    // #1831 Phase 2: `enqueue_pending_in_tx` is the real production seam
    // (called from `ingress::durable::apply_durable` via
    // `ingress_uow::MessageJudgmentOutboxRepository`). These two tests
    // prove it round-trips identically to `enqueue_pending` when
    // committed, and — the whole point of moving this onto a caller-owned
    // transaction — that it rolls back cleanly when that transaction
    // never commits, exactly like every other ingress repository write.

    #[tokio::test]
    async fn enqueue_pending_in_tx_is_visible_once_committed() {
        let db = test_db().await;
        super::super::schema::initialize(&db)
            .await
            .expect("initialize");

        let mut tx = db.begin().await.expect("begin");
        enqueue_pending_in_tx(
            &mut tx,
            PendingJudgmentInput {
                waddle_id: waddle_id(),
                stanza_id: stanza("stanza-tx-commit"),
                body: "is this committed?".to_string(),
                now_ms: 1_000,
            },
        )
        .await
        .expect("enqueue in tx");
        tx.commit().await.expect("commit");

        let due = fetch_due_batch(&db, 10, 1_000).await.expect("fetch");
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].stanza_id.as_str(), "stanza-tx-commit");
        assert_eq!(due[0].body_snapshot, "is this committed?");
    }

    #[tokio::test]
    async fn enqueue_pending_in_tx_is_absent_when_the_transaction_rolls_back() {
        let db = test_db().await;
        super::super::schema::initialize(&db)
            .await
            .expect("initialize");

        let mut tx = db.begin().await.expect("begin");
        enqueue_pending_in_tx(
            &mut tx,
            PendingJudgmentInput {
                waddle_id: waddle_id(),
                stanza_id: stanza("stanza-tx-rollback"),
                body: "never committed".to_string(),
                now_ms: 1_000,
            },
        )
        .await
        .expect("enqueue in tx");
        // No `tx.commit()`: dropping the transaction rolls it back, the
        // same behaviour `ingress_uow::IngressUowTransaction` documents
        // and relies on for every other repository write in the same
        // ingress transaction as an archive write.
        drop(tx);

        let due = fetch_due_batch(&db, 10, 1_000).await.expect("fetch");
        assert!(
            due.is_empty(),
            "a row enqueued on a rolled-back transaction must not persist"
        );
    }
}
