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

use futures::stream::{self, StreamExt};

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

/// Bound on judge calls processed concurrently within one [`drain_once`]
/// batch. Rows are otherwise fully independent, so without this, one slow
/// judge round-trip would serialize the rest of the batch behind it —
/// worst case, `batch_limit` sequential calls at the judge's own timeout
/// each. Bounded (rather than unbounded) so a large batch doesn't open an
/// unbounded number of concurrent outbound requests or DB connections at
/// once.
const MAX_CONCURRENT_JUDGE_CALLS: usize = 8;

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
    // Rows are fully independent (each is its own DB row, its own judge
    // call), so they're processed with bounded concurrency rather than one
    // at a time — otherwise one slow judge call would serialize the rest of
    // the batch behind it. `db`/`judge` are shared immutable references;
    // each row's own outcome is folded into `outcome` afterward rather than
    // mutated concurrently.
    let deltas: Vec<RowOutcome> = stream::iter(batch)
        .map(|row| process_row(db, judge, row, now_ms))
        .buffer_unordered(MAX_CONCURRENT_JUDGE_CALLS)
        .collect()
        .await;
    for delta in deltas {
        match delta {
            RowOutcome::Judged => outcome.judged += 1,
            RowOutcome::Failed { dead_lettered } => {
                outcome.failed += 1;
                if dead_lettered {
                    outcome.dead_lettered += 1;
                }
            }
            RowOutcome::StoreErrorIgnored => {}
        }
    }
    outcome
}

/// Per-row result of [`process_row`], folded into a shared [`DrainOutcome`]
/// by [`drain_once`] once every concurrently-processed row has finished —
/// never mutated concurrently from multiple in-flight rows.
enum RowOutcome {
    Judged,
    Failed {
        dead_lettered: bool,
    },
    /// A judge call resolved, but the *store* write that should have
    /// followed it failed (already logged at the call site). Not counted
    /// as judged, failed, or dead-lettered: the row's true state is
    /// whatever is durably persisted, and none of those counters would be
    /// accurate here.
    StoreErrorIgnored,
}

async fn process_row(
    db: &Database,
    judge: &dyn MessageJudge,
    row: PendingJudgmentRow,
    now_ms: i64,
) -> RowOutcome {
    // A prior pass over this exact row can have already judged it
    // successfully (insert_judgment committed) and then failed on the
    // *next* step (e.g. a transient mark_done error), leaving the row due
    // again. Re-invoking the judge in that case risks a second, unrelated
    // failure overwriting last_error on a row that is, in truth, already
    // judged -- a completed judgment misreported as failed. Skip straight
    // to marking it done instead.
    match store::judgment_exists(db, &row.stanza_id, store::IS_QUESTION_JUDGMENT_NAME).await {
        Ok(true) => {
            return match store::mark_done(db, &row.id).await {
                Ok(()) => RowOutcome::Judged,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "message_judgment_outbox: mark_done for already-judged row failed"
                    );
                    RowOutcome::StoreErrorIgnored
                }
            };
        }
        Ok(false) => {}
        Err(error) => {
            tracing::warn!(%error, "message_judgment_outbox: judgment_exists check failed");
            // Fall through and attempt the judge call as normal; failing
            // the existence check is not a reason to skip processing.
        }
    }
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
                return RowOutcome::StoreErrorIgnored;
            }
            if let Err(error) = store::mark_done(db, &row.id).await {
                tracing::warn!(%error, "message_judgment_outbox: mark_done after success failed");
                return RowOutcome::StoreErrorIgnored;
            }
            RowOutcome::Judged
        }
        Err(judge_error) => {
            let next_attempt = row.attempt_count.saturating_add(1);
            if next_attempt >= MAX_ATTEMPTS {
                // Give up: mark done (not deleted) so this permanently-
                // failing row stops being retried forever and never blocks
                // other rows or grows the queue unboundedly. Persists the
                // terminal error too (`dead_letter`, unlike `mark_done`),
                // so the audit trail doesn't silently drop the one failure
                // that actually ended the row's retries.
                return match store::dead_letter(db, &row.id, &judge_error.to_string()).await {
                    Ok(true) => RowOutcome::Failed {
                        dead_lettered: true,
                    },
                    Ok(false) => {
                        // Lost the race: a concurrent drain worker already
                        // completed this row between this call's judge
                        // invocation and this write. This failure is stale
                        // and must not overwrite the completed judgment's
                        // audit trail.
                        RowOutcome::StoreErrorIgnored
                    }
                    Err(error) => {
                        tracing::warn!(%error, "message_judgment_outbox: dead_letter failed");
                        RowOutcome::Failed {
                            dead_lettered: false,
                        }
                    }
                };
            }
            let delay = retry_delay_ms(next_attempt);
            match store::record_failure(
                db,
                &row.id,
                &judge_error.to_string(),
                now_ms.saturating_add(delay),
            )
            .await
            {
                Ok(true) => {}
                Ok(false) => {
                    // Same race as above, one retry earlier: don't record
                    // a stale failure over an already-completed row.
                    return RowOutcome::StoreErrorIgnored;
                }
                Err(error) => {
                    tracing::warn!(%error, "message_judgment_outbox: record_failure failed");
                }
            }
            RowOutcome::Failed {
                dead_lettered: false,
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

    /// Canned [`MessageJudge`] that re-invokes a responder closure (given
    /// the row's body) on every call. A closure keyed on the body — rather
    /// than a canned `Vec` of responses — sidesteps `JudgeError` not
    /// implementing `Clone` (the fixed contract in `judge.rs` is not ours
    /// to change) while still letting a single judge instance answer
    /// differently for different rows in the same batch.
    type FakeJudgeResponder = dyn Fn(&str) -> Result<IsQuestionJudgment, JudgeError> + Send + Sync;

    struct FakeJudge {
        responder: Box<FakeJudgeResponder>,
        calls: Mutex<usize>,
    }

    impl FakeJudge {
        fn new(
            responder: impl Fn(&str) -> Result<IsQuestionJudgment, JudgeError> + Send + Sync + 'static,
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
        async fn is_question(&self, body: &str) -> Result<IsQuestionJudgment, JudgeError> {
            *self.calls.lock().expect("calls mutex") += 1;
            (self.responder)(body)
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

        let judge = FakeJudge::new(|_body| ok_judgment());
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

        let judge = FakeJudge::new(|_body| err_judgment());
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

    /// Fails only for one specific message body, succeeds for every other —
    /// so a single drain pass can exercise a permanently-failing row and a
    /// healthy sibling row at the same time, distinguished by body text
    /// rather than by artificially staggering when each becomes due.
    struct BodyKeyedFakeJudge {
        fail_body: &'static str,
    }

    #[async_trait]
    impl MessageJudge for BodyKeyedFakeJudge {
        async fn is_question(&self, body: &str) -> Result<IsQuestionJudgment, JudgeError> {
            if body == self.fail_body {
                err_judgment()
            } else {
                ok_judgment()
            }
        }
    }

    #[tokio::test]
    async fn drain_once_dead_letters_after_max_attempts_without_blocking_the_queue() {
        let db = test_db().await;
        enqueue_pending(
            &db,
            PendingJudgmentInput {
                waddle_id: waddle_id(),
                stanza_id: stanza("stanza-dead-letter"),
                body: "body-fails".to_string(),
                now_ms: 0,
            },
        )
        .await
        .expect("enqueue");
        // A second, healthy row must keep draining even while the first
        // row is permanently failing — both are due from the same instant,
        // so the very first pass fetches both together.
        enqueue_pending(
            &db,
            PendingJudgmentInput {
                waddle_id: waddle_id(),
                stanza_id: stanza("stanza-healthy"),
                body: "body-healthy".to_string(),
                now_ms: 0,
            },
        )
        .await
        .expect("enqueue");

        let judge = BodyKeyedFakeJudge {
            fail_body: "body-fails",
        };
        let mut now_ms = 0_i64;
        for attempt in 1..=MAX_ATTEMPTS {
            let outcome = drain_once(&db, &judge, now_ms, 10).await;
            if attempt == 1 {
                // Both rows are due together on the first pass: the
                // healthy one is judged and marked done immediately, the
                // other starts its failure/backoff trajectory.
                assert_eq!(outcome.fetched, 2, "attempt 1: both rows are due");
                assert_eq!(outcome.judged, 1, "attempt 1: the healthy row succeeds");
            } else {
                assert_eq!(
                    outcome.fetched, 1,
                    "attempt {attempt}: only the still-failing row remains due"
                );
            }
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

        // But it still exists, marked done, not deleted -- and its terminal
        // failure reason is persisted (`dead_letter`, not a bare
        // `mark_done`), not silently dropped in favor of an earlier
        // attempt's error.
        let connection = db.guard().await.expect("guard");
        let mut rows = connection
            .query(
                "SELECT done, last_error FROM message_judgment_outbox WHERE stanza_id = ?",
                crate::db_params!["stanza-dead-letter"],
            )
            .await
            .expect("query");
        let row = rows.next().await.expect("row").expect("row present");
        assert!(row.get::<bool>(0).expect("done"));
        assert_eq!(
            row.get::<Option<String>>(1).expect("last_error"),
            Some("judge transport failure: connection reset".to_string())
        );

        // The healthy row was judged successfully on the very first pass,
        // was never dead-lettered, and never blocked (or was blocked by)
        // the other row's ongoing failures.
        let mut rows = connection
            .query(
                "SELECT probability FROM message_judgments WHERE stanza_id = ?",
                crate::db_params!["stanza-healthy"],
            )
            .await
            .expect("query");
        let row = rows.next().await.expect("row").expect("row present");
        assert_eq!(row.get::<f64>(0).expect("probability"), 0.75);
    }

    #[tokio::test]
    async fn drain_once_does_not_re_judge_a_row_already_durably_judged() {
        // Simulates the aftermath of a prior pass whose judge call and
        // insert_judgment both succeeded, but which failed afterward (e.g.
        // a transient mark_done error) -- leaving the outbox row not done,
        // so it's still due, even though `message_judgments` already has a
        // correct, durable result for it.
        let db = test_db().await;
        enqueue_pending(
            &db,
            PendingJudgmentInput {
                waddle_id: waddle_id(),
                stanza_id: stanza("stanza-already-judged"),
                body: "body".to_string(),
                now_ms: 0,
            },
        )
        .await
        .expect("enqueue");
        store::insert_judgment(
            &db,
            JudgmentRecord {
                waddle_id: waddle_id(),
                stanza_id: stanza("stanza-already-judged"),
                judgment_name: store::IS_QUESTION_JUDGMENT_NAME.to_string(),
                taxonomy_version: "v1".to_string(),
                model_version: "jev-1".to_string(),
                probability: 0.75,
                confidence: 0.6,
                decided_at_ms: 0,
                created_at_ms: 0,
            },
        )
        .await
        .expect("pre-seed judgment");

        // A judge that always fails: if `drain_once` called it, this row
        // would come out failed/dead-lettered and `last_error` would be
        // overwritten -- exactly the misreport this guards against.
        let judge = FakeJudge::new(|_body| err_judgment());
        let outcome = drain_once(&db, &judge, 0, 10).await;

        assert_eq!(outcome.judged, 1);
        assert_eq!(outcome.failed, 0);
        assert_eq!(
            judge.call_count(),
            0,
            "the judge must not be called for a row that's already durably judged"
        );

        let connection = db.guard().await.expect("guard");
        let mut rows = connection
            .query(
                "SELECT done, last_error FROM message_judgment_outbox WHERE stanza_id = ?",
                crate::db_params!["stanza-already-judged"],
            )
            .await
            .expect("query");
        let row = rows.next().await.expect("row").expect("row present");
        assert!(row.get::<bool>(0).expect("done"));
        assert_eq!(row.get::<Option<String>>(1).expect("last_error"), None);
    }
}
