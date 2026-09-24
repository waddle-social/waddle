//! Async drain worker for the `message_judgment_outbox` queue.
//!
//! Fully decoupled from message ingress/delivery: nothing here runs on the
//! send path, nothing here is wire-visible, and a permanently-failing row
//! never blocks other rows or grows the queue unboundedly (it is
//! dead-lettered — marked done, never deleted — once `MAX_ATTEMPTS` is
//! exceeded). Unlike `room_effect_outbox`, there is no clustering lease,
//! ownership claim, or supervisor here: this is a single background poll
//! loop, safe to run on exactly one process at a time (running it on
//! several is also harmless, since `fetch_due_batch` + retries are not
//! exactly-once — duplicate judge calls are tolerated, per the idempotent
//! `message_judgments` insert).

use std::sync::Arc;
use std::time::Duration;

use crate::db::Database;

use super::judge::MessageJudge;
use super::store::{self, JudgmentRecord, PendingJudgmentRow};

/// Retry/backoff constants — generic exponential-backoff math, copied
/// verbatim from `room_effect_outbox::store` (nothing clustering-specific).
pub const MAX_ATTEMPTS: i64 = 20;
pub const BASE_RETRY_DELAY_MS: i64 = 5_000;
pub const MAX_RETRY_DELAY_MS: i64 = 600_000;

/// Default batch size for [`run_drain_loop`]'s poll ticks.
const DEFAULT_BATCH_LIMIT: i64 = 100;

pub fn retry_delay_ms(attempt: i64) -> i64 {
    let shift = if attempt <= 1 {
        0
    } else {
        (attempt - 1).min(20) as u32
    };
    BASE_RETRY_DELAY_MS
        .saturating_mul(1_i64 << shift)
        .min(MAX_RETRY_DELAY_MS)
}

/// Outcome of one [`drain_once`] pass, for tests and observability.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DrainOutcome {
    /// Rows fetched from the due set this pass.
    pub fetched: usize,
    /// Rows successfully judged and marked done.
    pub judged: usize,
    /// Rows that failed the judge call this pass (rescheduled or
    /// dead-lettered — see `dead_lettered`).
    pub failed: usize,
    /// Of `failed`, rows that exceeded `MAX_ATTEMPTS` and were dead-lettered
    /// (marked done, never retried again).
    pub dead_lettered: usize,
}

/// Fetch one due batch and attempt a judgment for each row. Never panics
/// and never propagates a judge failure as a hard error: a judge failure is
/// expected, per-row, and handled by rescheduling with backoff or
/// dead-lettering — it is never allowed to abort the batch.
pub async fn drain_once(
    db: &Database,
    judge: &dyn MessageJudge,
    now_ms: i64,
    batch_limit: i64,
) -> DrainOutcome {
    let mut outcome = DrainOutcome::default();
    let batch = match store::fetch_due_batch(db, batch_limit, now_ms).await {
        Ok(batch) => batch,
        Err(error) => {
            tracing::warn!(%error, "message_judgment_outbox: fetch_due_batch failed");
            return outcome;
        }
    };
    outcome.fetched = batch.len();
    for row in batch {
        process_row(db, judge, row, now_ms, &mut outcome).await;
    }
    outcome
}

async fn process_row(
    db: &Database,
    judge: &dyn MessageJudge,
    row: PendingJudgmentRow,
    now_ms: i64,
    outcome: &mut DrainOutcome,
) {
    match judge.is_question(&row.body_snapshot).await {
        Ok(judgment) => {
            let record = JudgmentRecord {
                waddle_id: row.waddle_id,
                stanza_id: row.stanza_id,
                judgment_name: store::IS_QUESTION_JUDGMENT_NAME.to_string(),
                taxonomy_version: judgment.taxonomy_version,
                model_version: judgment.model_version,
                probability: judgment.probability,
                confidence: judgment.confidence,
                decided_at_ms: now_ms,
                created_at_ms: now_ms,
            };
            if let Err(error) = store::insert_judgment(db, record).await {
                tracing::warn!(%error, "message_judgment_outbox: insert_judgment failed");
                return;
            }
            if let Err(error) = store::mark_done(db, &row.id).await {
                tracing::warn!(%error, "message_judgment_outbox: mark_done after success failed");
                return;
            }
            outcome.judged += 1;
        }
        Err(judge_error) => {
            outcome.failed += 1;
            let next_attempt = row.attempt_count.saturating_add(1);
            if next_attempt >= MAX_ATTEMPTS {
                // Give up silently: mark done (not deleted) so this
                // permanently-failing row stops being retried forever and
                // never blocks other rows or grows the queue unboundedly.
                if let Err(error) = store::mark_done(db, &row.id).await {
                    tracing::warn!(
                        %error,
                        "message_judgment_outbox: dead-letter mark_done failed"
                    );
                    return;
                }
                outcome.dead_lettered += 1;
                return;
            }
            let delay = retry_delay_ms(next_attempt);
            if let Err(error) = store::record_failure(
                db,
                &row.id,
                &judge_error.to_string(),
                now_ms.saturating_add(delay),
            )
            .await
            {
                tracing::warn!(%error, "message_judgment_outbox: record_failure failed");
            }
        }
    }
}

/// Background poll loop: calls [`drain_once`] on an interval, forever.
/// Callers `tokio::spawn` this; it never returns. Plain interval polling —
/// no supervisor/actor, unlike `room_effect_outbox`'s
/// `RoomEffectArmSupervisor` (a different problem: clustered lease
/// contention, which this single-worker outbox does not have).
///
/// Assumes [`super::initialize`] has already run against `db` (schema
/// bootstrap is not repeated here on every tick).
pub async fn run_drain_loop(db: Database, judge: Arc<dyn MessageJudge>, poll_interval: Duration) {
    let mut interval = tokio::time::interval(poll_interval);
    loop {
        interval.tick().await;
        let now_ms = crate::time::now_ms();
        drain_once(&db, judge.as_ref(), now_ms, DEFAULT_BATCH_LIMIT).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_judgment_outbox::judge::{IsQuestionJudgment, JudgeError};
    use crate::message_judgment_outbox::store::{
        enqueue_pending, fetch_due_batch, PendingJudgmentInput,
    };
    use async_trait::async_trait;
    use std::sync::Mutex;
    use waddle_xmpp::muc::durable::WaddleId;
    use waddle_xmpp_core::xep0359::StanzaId;

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
        let db = Database::in_memory(&format!(
            "message-judgment-outbox-drain-{}",
            uuid::Uuid::new_v4()
        ))
        .await
        .expect("in-memory database");
        super::super::schema::initialize(&db)
            .await
            .expect("initialize");
        db
    }

    /// Canned [`MessageJudge`] that re-invokes a responder closure on every
    /// call. A closure (rather than a canned `Vec` of responses) sidesteps
    /// `JudgeError` not implementing `Clone` — the fixed contract in
    /// `judge.rs` is not ours to change — while still letting each test
    /// pick fresh `Ok`/`Err` values per call.
    struct FakeJudge {
        responder: Box<dyn Fn() -> Result<IsQuestionJudgment, JudgeError> + Send + Sync>,
        calls: Mutex<usize>,
    }

    impl FakeJudge {
        fn new(
            responder: impl Fn() -> Result<IsQuestionJudgment, JudgeError> + Send + Sync + 'static,
        ) -> Self {
            Self {
                responder: Box::new(responder),
                calls: Mutex::new(0),
            }
        }

        fn call_count(&self) -> usize {
            *self.calls.lock().expect("calls mutex")
        }
    }

    #[async_trait]
    impl MessageJudge for FakeJudge {
        async fn is_question(&self, _body: &str) -> Result<IsQuestionJudgment, JudgeError> {
            *self.calls.lock().expect("calls mutex") += 1;
            (self.responder)()
        }
    }

    fn ok_judgment() -> Result<IsQuestionJudgment, JudgeError> {
        Ok(IsQuestionJudgment {
            probability: 0.75,
            confidence: 0.6,
            taxonomy_version: "v1".to_string(),
            model_version: "jev-1".to_string(),
        })
    }

    fn err_judgment() -> Result<IsQuestionJudgment, JudgeError> {
        Err(JudgeError::Transport("connection reset".to_string()))
    }

    #[tokio::test]
    async fn drain_once_success_path_records_judgment_and_marks_done() {
        let db = test_db().await;
        enqueue_pending(
            &db,
            PendingJudgmentInput {
                waddle_id: waddle_id(),
                stanza_id: stanza("stanza-ok"),
                body: "is this a question?".to_string(),
                now_ms: 1_000,
            },
        )
        .await
        .expect("enqueue");

        let judge = FakeJudge::new(ok_judgment);
        let outcome = drain_once(&db, &judge, 1_000, 10).await;

        assert_eq!(outcome.fetched, 1);
        assert_eq!(outcome.judged, 1);
        assert_eq!(outcome.failed, 0);
        assert_eq!(judge.call_count(), 1);

        // Row is gone from the due set (marked done).
        assert!(fetch_due_batch(&db, 10, 1_000)
            .await
            .expect("fetch")
            .is_empty());

        let connection = db.guard().await.expect("guard");
        let mut rows = connection
            .query(
                "SELECT probability, confidence, taxonomy_version, model_version \
                 FROM message_judgments WHERE stanza_id = ?",
                crate::db_params!["stanza-ok"],
            )
            .await
            .expect("query");
        let row = rows.next().await.expect("row").expect("row present");
        assert_eq!(row.get::<f64>(0).expect("probability"), 0.75);
        assert_eq!(row.get::<f64>(1).expect("confidence"), 0.6);
        assert_eq!(row.get::<String>(2).expect("taxonomy_version"), "v1");
        assert_eq!(row.get::<String>(3).expect("model_version"), "jev-1");
    }

    #[tokio::test]
    async fn drain_once_failure_path_reschedules_with_backoff() {
        let db = test_db().await;
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

        let judge = FakeJudge::new(err_judgment);
        let outcome = drain_once(&db, &judge, 1_000, 10).await;

        assert_eq!(outcome.fetched, 1);
        assert_eq!(outcome.judged, 0);
        assert_eq!(outcome.failed, 1);
        assert_eq!(outcome.dead_lettered, 0);

        // Not immediately due again.
        assert!(fetch_due_batch(&db, 10, 1_000)
            .await
            .expect("fetch")
            .is_empty());
        let expected_available_at = 1_000 + retry_delay_ms(1);
        let due_later = fetch_due_batch(&db, 10, expected_available_at)
            .await
            .expect("fetch");
        assert_eq!(due_later.len(), 1);
        assert_eq!(due_later[0].attempt_count, 1);
        assert_eq!(
            due_later[0].last_error.as_deref(),
            Some("judge transport failure: connection reset")
        );
    }

    #[tokio::test]
    async fn drain_once_dead_letters_after_max_attempts_without_blocking_the_queue() {
        let db = test_db().await;
        enqueue_pending(
            &db,
            PendingJudgmentInput {
                waddle_id: waddle_id(),
                stanza_id: stanza("stanza-dead-letter"),
                body: "body".to_string(),
                now_ms: 0,
            },
        )
        .await
        .expect("enqueue");
        // A second, healthy row must keep draining even while the first
        // row is permanently failing.
        enqueue_pending(
            &db,
            PendingJudgmentInput {
                waddle_id: waddle_id(),
                stanza_id: stanza("stanza-healthy"),
                body: "body".to_string(),
                now_ms: 0,
            },
        )
        .await
        .expect("enqueue");

        let judge = FakeJudge::new(err_judgment);
        let mut now_ms = 0_i64;
        for attempt in 1..=MAX_ATTEMPTS {
            let outcome = drain_once(&db, &judge, now_ms, 10).await;
            assert_eq!(
                outcome.fetched, 1,
                "attempt {attempt}: only the due row is fetched"
            );
            if attempt < MAX_ATTEMPTS {
                assert_eq!(
                    outcome.dead_lettered, 0,
                    "attempt {attempt}: not yet dead-lettered"
                );
                now_ms += retry_delay_ms(attempt);
            } else {
                assert_eq!(outcome.dead_lettered, 1, "final attempt must dead-letter");
            }
        }

        // Dead-lettered row no longer appears in the due set, ever.
        assert!(fetch_due_batch(&db, 10, i64::MAX)
            .await
            .expect("fetch")
            .is_empty());

        // But it still exists, marked done, not deleted.
        let connection = db.guard().await.expect("guard");
        let mut rows = connection
            .query(
                "SELECT done FROM message_judgment_outbox WHERE stanza_id = ?",
                crate::db_params!["stanza-dead-letter"],
            )
            .await
            .expect("query");
        let row = rows.next().await.expect("row").expect("row present");
        assert!(row.get::<bool>(0).expect("done"));

        // The healthy row was never touched by the other row's failures
        // and is still (independently) judgeable.
        let healthy_judge = FakeJudge::new(ok_judgment);
        let outcome = drain_once(&db, &healthy_judge, 0, 10).await;
        assert_eq!(outcome.judged, 1);
    }
}
