//! Async drain worker for the `extension_job_outbox` queue.
//!
//! Mirrors `message_judgment_outbox::drain`'s shape (claim a batch, process
//! rows with bounded concurrency, reschedule/dead-letter failures with
//! exponential backoff) but is otherwise generic: nothing here knows what a
//! "judgment" is — [`DurableJobRunner`] is the only seam between this
//! module's queue/retry machinery and an actual extension invocation, so
//! the retry/backoff logic is fully unit-testable against a fake runner
//! (see this module's tests) without wasmtime or a real guest component.
//!
//! Safe to run on more than one node at once for the same reason
//! `message_judgment_outbox::drain` was: each poll tick claims its batch
//! exclusively via [`store::claim_due_batch`] rather than a plain `SELECT`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use waddle_extensions::JudgmentResult;

use crate::db::Database;

use super::store::{self, ClaimedJob};
use super::wire::SafetyScoresWireSink;

/// Retry/backoff constants, copied verbatim from
/// `message_judgment_outbox::drain` (generic exponential-backoff math,
/// nothing job-kind-specific).
pub const MAX_ATTEMPTS: i64 = 20;
pub const BASE_RETRY_DELAY_MS: i64 = 5_000;
pub const MAX_RETRY_DELAY_MS: i64 = 600_000;

const DEFAULT_BATCH_LIMIT: i64 = 100;

/// Bound on job invocations processed concurrently within one
/// [`drain_once`] batch, for the same reason
/// `message_judgment_outbox::drain::MAX_CONCURRENT_JUDGE_CALLS` existed:
/// rows are otherwise independent, so a slow (or, per this PR's guardrail
/// 1, wedged-forever) guest invocation must not serialize the rest of the
/// batch behind it.
const MAX_CONCURRENT_JOB_CALLS: usize = 8;

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
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DrainOutcome {
    pub fetched: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub dead_lettered: usize,
}

/// One durable job invocation's outcome, as reported by whatever actually
/// ran the guest (the real implementation: `ExtensionManager` +
/// `WasmExtensionActor`; tests: a fake).
pub enum DurableJobRunOutcome {
    Success(JudgmentResult),
    Failure {
        message: String,
        retryable: bool,
    },
    /// No extension currently loaded and granted for this job's kind (it
    /// may have been unloaded, or had its grant revoked, since this row
    /// was enqueued). Treated exactly like a retryable failure — the row
    /// backs off and is retried in case the extension returns, and is
    /// eventually dead-lettered like any other persistently-failing row
    /// rather than being force-routed to a different extension.
    NoHandler,
}

/// Abstracts "invoke whatever extension should process this job" so the
/// queue/retry/dead-letter logic in this module is testable without
/// wasmtime or a real guest component. The real implementation lives in
/// `server::extension_host_adapter` (or a small adapter over
/// `ExtensionManager::durable_job_handler` +
/// `WasmExtensionActor::handle_event_for_waddle_with_requester`).
#[async_trait]
pub trait DurableJobRunner: Send + Sync {
    async fn run(&self, job: &ClaimedJob) -> DurableJobRunOutcome;
}

/// Claim one due batch (exclusively, fairly across extensions — see
/// `store::claim_due_batch`) and attempt each job. Never panics and never
/// propagates a job failure as a hard error.
pub async fn drain_once(
    db: &Database,
    runner: &dyn DurableJobRunner,
    sink: &dyn SafetyScoresWireSink,
    now_ms: i64,
    batch_limit: i64,
) -> DrainOutcome {
    let mut outcome = DrainOutcome::default();
    let batch = match store::claim_due_batch(db, batch_limit, now_ms).await {
        Ok(batch) => batch,
        Err(error) => {
            tracing::warn!(%error, "extension_job_outbox: claim_due_batch failed");
            return outcome;
        }
    };
    outcome.fetched = batch.len();
    let deltas: Vec<RowOutcome> = stream::iter(batch)
        .map(|row| process_row(db, runner, sink, row, now_ms))
        .buffer_unordered(MAX_CONCURRENT_JOB_CALLS)
        .collect()
        .await;
    for delta in deltas {
        match delta {
            RowOutcome::Succeeded => outcome.succeeded += 1,
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

enum RowOutcome {
    Succeeded,
    Failed {
        dead_lettered: bool,
    },
    /// The run resolved, but the finalize write lost the lease race (a
    /// concurrent worker already reclaimed or finalized this row) or hit a
    /// database error (already logged at the call site).
    StoreErrorIgnored,
}

async fn process_row(
    db: &Database,
    runner: &dyn DurableJobRunner,
    sink: &dyn SafetyScoresWireSink,
    row: ClaimedJob,
    now_ms: i64,
) -> RowOutcome {
    match runner.run(&row).await {
        DurableJobRunOutcome::Success(result) => finalize_success(db, sink, &row, result).await,
        DurableJobRunOutcome::Failure { message, retryable } => {
            finalize_failure(db, &row, &message, retryable, now_ms).await
        }
        DurableJobRunOutcome::NoHandler => {
            finalize_failure(
                db,
                &row,
                "no loaded/granted extension currently handles this job kind",
                true,
                now_ms,
            )
            .await
        }
    }
}

/// Guardrail 2 (lease-checked finalize) applied in the order that avoids a
/// double broadcast: the row is marked done *before* the wire effect is
/// emitted, and only if that lease-checked write actually applies — a
/// worker that lost its lease to a reclaimer must not also broadcast a
/// result the reclaiming worker might independently (and, once it
/// completes, exclusively) also produce.
async fn finalize_success(
    db: &Database,
    sink: &dyn SafetyScoresWireSink,
    row: &ClaimedJob,
    result: JudgmentResult,
) -> RowOutcome {
    match store::mark_done(db, &row.id, &row.lease_token).await {
        Ok(true) => {
            if let Err(error) = sink
                .emit_safety_scores(row.room.clone(), row.target_stanza_id.clone(), result)
                .await
            {
                // Delivery is best-effort once the job itself is durably
                // recorded as succeeded: retrying the whole job (a
                // potentially paid, rate-limited vendor call) just because
                // a live-occupant broadcast could not be delivered would
                // defeat the point of marking it done. Log and move on.
                tracing::warn!(
                    %error,
                    job_id = row.id.as_str(),
                    "extension_job_outbox: safety-scores wire emission failed after the job \
                     itself was recorded done"
                );
            }
            RowOutcome::Succeeded
        }
        Ok(false) => RowOutcome::StoreErrorIgnored,
        Err(error) => {
            tracing::warn!(%error, "extension_job_outbox: mark_done failed");
            RowOutcome::StoreErrorIgnored
        }
    }
}

async fn finalize_failure(
    db: &Database,
    row: &ClaimedJob,
    message: &str,
    retryable: bool,
    now_ms: i64,
) -> RowOutcome {
    if !retryable || row.attempt_count >= MAX_ATTEMPTS {
        return match store::dead_letter(db, &row.id, &row.lease_token, message).await {
            Ok(true) => RowOutcome::Failed {
                dead_lettered: true,
            },
            Ok(false) => RowOutcome::StoreErrorIgnored,
            Err(error) => {
                tracing::warn!(%error, "extension_job_outbox: dead_letter failed");
                RowOutcome::Failed {
                    dead_lettered: false,
                }
            }
        };
    }
    let delay = retry_delay_ms(row.attempt_count);
    match store::record_failure(
        db,
        &row.id,
        &row.lease_token,
        message,
        now_ms.saturating_add(delay),
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => return RowOutcome::StoreErrorIgnored,
        Err(error) => {
            tracing::warn!(%error, "extension_job_outbox: record_failure failed");
        }
    }
    RowOutcome::Failed {
        dead_lettered: false,
    }
}

/// Background poll loop: calls [`drain_once`] on an interval, forever.
/// Callers `tokio::spawn` this; it never returns.
pub async fn run_drain_loop(
    db: Database,
    runner: Arc<dyn DurableJobRunner>,
    sink: Arc<dyn SafetyScoresWireSink>,
    poll_interval: Duration,
) {
    let mut interval = tokio::time::interval(poll_interval);
    loop {
        interval.tick().await;
        let now_ms = crate::time::now_ms();
        drain_once(
            &db,
            runner.as_ref(),
            sink.as_ref(),
            now_ms,
            DEFAULT_BATCH_LIMIT,
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_job_outbox::store::{enqueue_pending, fetch_due_batch, PendingJobInput};
    use crate::extension_job_outbox::wire::NullSafetyScoresWireSink;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use waddle_extensions::{JobKind, JudgmentScore, PluginId, RoomJid, WaddleId};
    use waddle_xmpp_core::xep0359::StanzaId;

    fn extension() -> PluginId {
        PluginId::new("community-safety-judge").expect("plugin id")
    }

    fn kind() -> JobKind {
        JobKind::new("message-judge").expect("job kind")
    }

    fn waddle() -> WaddleId {
        WaddleId::new("default").expect("waddle id")
    }

    fn stanza(id: &str) -> StanzaId {
        StanzaId::new(
            id.to_string(),
            "room@conference.example.test".parse().expect("room jid"),
        )
    }

    async fn test_db() -> Database {
        let db = Database::in_memory(&format!(
            "extension-job-outbox-drain-{}",
            uuid::Uuid::new_v4()
        ))
        .await
        .expect("in-memory database");
        super::super::schema::initialize(&db)
            .await
            .expect("initialize");
        db
    }

    fn judgment_result() -> JudgmentResult {
        JudgmentResult {
            model_version: waddle_extensions::JudgmentModelVersion::new("test-model-1")
                .expect("model version"),
            scores: vec![JudgmentScore {
                category: waddle_extensions::JudgmentCategory::new("is_question")
                    .expect("category"),
                probability: waddle_extensions::JudgmentProbability::new(0.75).expect("prob"),
                taxonomy_version: waddle_extensions::JudgmentTaxonomyVersion::new("v1")
                    .expect("taxonomy version"),
            }],
        }
    }

    type FakeRunnerResponder = dyn Fn(&ClaimedJob) -> DurableJobRunOutcome + Send + Sync;

    struct FakeRunner {
        responder: Box<FakeRunnerResponder>,
        calls: Mutex<Vec<i64>>,
    }

    impl FakeRunner {
        fn new(
            responder: impl Fn(&ClaimedJob) -> DurableJobRunOutcome + Send + Sync + 'static,
        ) -> Self {
            Self {
                responder: Box::new(responder),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.lock().expect("calls").len()
        }
    }

    #[async_trait]
    impl DurableJobRunner for FakeRunner {
        async fn run(&self, job: &ClaimedJob) -> DurableJobRunOutcome {
            self.calls.lock().expect("calls").push(job.attempt_count);
            (self.responder)(job)
        }
    }

    #[tokio::test]
    async fn drain_once_success_path_marks_done_and_emits_wire_effect() {
        let db = test_db().await;
        enqueue_pending(
            &db,
            PendingJobInput {
                extension_id: extension(),
                job_kind: kind(),
                waddle_id: waddle(),
                room: RoomJid::new("room@conference.example.test").ok(),
                target_stanza_id: stanza("stanza-ok"),
                body: "is this a question?".to_string(),
                now_ms: 1_000,
            },
        )
        .await
        .expect("enqueue");

        let emitted = Arc::new(AtomicUsize::new(0));
        let sink = super::super::wire::tests::CountingSink::new(Arc::clone(&emitted));
        let runner = FakeRunner::new(|_row| DurableJobRunOutcome::Success(judgment_result()));
        let outcome = drain_once(&db, &runner, &sink, 1_000, 10).await;

        assert_eq!(outcome.fetched, 1);
        assert_eq!(outcome.succeeded, 1);
        assert_eq!(outcome.failed, 0);
        assert_eq!(emitted.load(Ordering::SeqCst), 1);
        assert!(fetch_due_batch(&db, 10, 1_000)
            .await
            .expect("fetch")
            .is_empty());
    }

    #[tokio::test]
    async fn drain_once_failure_path_reschedules_with_backoff_without_touching_attempt_count() {
        let db = test_db().await;
        enqueue_pending(
            &db,
            PendingJobInput {
                extension_id: extension(),
                job_kind: kind(),
                waddle_id: waddle(),
                room: None,
                target_stanza_id: stanza("stanza-fail"),
                body: "body".to_string(),
                now_ms: 1_000,
            },
        )
        .await
        .expect("enqueue");

        let sink = NullSafetyScoresWireSink;
        let runner = FakeRunner::new(|_row| DurableJobRunOutcome::Failure {
            message: "transport error".to_string(),
            retryable: true,
        });
        let outcome = drain_once(&db, &runner, &sink, 1_000, 10).await;

        assert_eq!(outcome.fetched, 1);
        assert_eq!(outcome.succeeded, 0);
        assert_eq!(outcome.failed, 1);
        assert_eq!(outcome.dead_lettered, 0);

        // The claim itself already bumped attempt_count to 1 (guardrail 1);
        // finalize_failure's record_failure must not bump it again.
        let expected_available_at = 1_000 + retry_delay_ms(1);
        let due_later = fetch_due_batch(&db, 10, expected_available_at)
            .await
            .expect("fetch");
        assert_eq!(due_later.len(), 1);
        assert_eq!(due_later[0].attempt_count, 1);
    }

    #[tokio::test]
    async fn drain_once_dead_letters_a_non_retryable_failure_immediately() {
        let db = test_db().await;
        enqueue_pending(
            &db,
            PendingJobInput {
                extension_id: extension(),
                job_kind: kind(),
                waddle_id: waddle(),
                room: None,
                target_stanza_id: stanza("stanza-permanent"),
                body: "body".to_string(),
                now_ms: 1_000,
            },
        )
        .await
        .expect("enqueue");

        let sink = NullSafetyScoresWireSink;
        let runner = FakeRunner::new(|_row| DurableJobRunOutcome::Failure {
            message: "malformed job".to_string(),
            retryable: false,
        });
        let outcome = drain_once(&db, &runner, &sink, 1_000, 10).await;

        assert_eq!(outcome.dead_lettered, 1);
        assert!(fetch_due_batch(&db, 10, i64::MAX)
            .await
            .expect("fetch")
            .is_empty());
    }

    #[tokio::test]
    async fn drain_once_dead_letters_after_max_attempts_even_though_the_guest_never_returns() {
        // Simulates guardrail 1: a "wedged" job whose guest invocation never
        // returns is modeled here as a runner that always reports a
        // retryable failure (standing in for the row being reclaimed after
        // its lease goes stale with no successful invocation ever having
        // happened) — attempt_count still marches to MAX_ATTEMPTS purely
        // from claim/reclaim, and the row is dead-lettered on its own.
        let db = test_db().await;
        enqueue_pending(
            &db,
            PendingJobInput {
                extension_id: extension(),
                job_kind: kind(),
                waddle_id: waddle(),
                room: None,
                target_stanza_id: stanza("stanza-wedged"),
                body: "body".to_string(),
                now_ms: 0,
            },
        )
        .await
        .expect("enqueue");

        let sink = NullSafetyScoresWireSink;
        let runner = FakeRunner::new(|_row| DurableJobRunOutcome::Failure {
            message: "still failing".to_string(),
            retryable: true,
        });
        let mut now_ms = 0_i64;
        for attempt in 1..=MAX_ATTEMPTS {
            let outcome = drain_once(&db, &runner, &sink, now_ms, 10).await;
            assert_eq!(outcome.fetched, 1, "attempt {attempt}: row still due");
            if attempt < MAX_ATTEMPTS {
                assert_eq!(outcome.dead_lettered, 0);
                now_ms += retry_delay_ms(attempt);
            } else {
                assert_eq!(outcome.dead_lettered, 1, "final attempt must dead-letter");
            }
        }
        assert_eq!(runner.call_count(), MAX_ATTEMPTS as usize);
        assert!(fetch_due_batch(&db, 10, i64::MAX)
            .await
            .expect("fetch")
            .is_empty());
    }

    #[tokio::test]
    async fn drain_once_treats_no_handler_as_retryable() {
        let db = test_db().await;
        enqueue_pending(
            &db,
            PendingJobInput {
                extension_id: extension(),
                job_kind: kind(),
                waddle_id: waddle(),
                room: None,
                target_stanza_id: stanza("stanza-no-handler"),
                body: "body".to_string(),
                now_ms: 1_000,
            },
        )
        .await
        .expect("enqueue");

        let sink = NullSafetyScoresWireSink;
        let runner = FakeRunner::new(|_row| DurableJobRunOutcome::NoHandler);
        let outcome = drain_once(&db, &runner, &sink, 1_000, 10).await;
        assert_eq!(outcome.failed, 1);
        assert_eq!(outcome.dead_lettered, 0);
    }

    #[tokio::test]
    async fn fair_claiming_gives_every_extension_its_due_row_before_any_extension_gets_a_second() {
        let db = test_db().await;
        let busy = PluginId::new("busy-extension").expect("plugin");
        let quiet = PluginId::new("quiet-extension").expect("plugin");
        for index in 0..5 {
            enqueue_pending(
                &db,
                PendingJobInput {
                    extension_id: busy.clone(),
                    job_kind: kind(),
                    waddle_id: waddle(),
                    room: None,
                    target_stanza_id: stanza(&format!("busy-{index}")),
                    body: "body".to_string(),
                    now_ms: 1_000,
                },
            )
            .await
            .expect("enqueue busy");
        }
        enqueue_pending(
            &db,
            PendingJobInput {
                extension_id: quiet.clone(),
                job_kind: kind(),
                waddle_id: waddle(),
                room: None,
                target_stanza_id: stanza("quiet-1"),
                body: "body".to_string(),
                now_ms: 1_000,
            },
        )
        .await
        .expect("enqueue quiet");

        // A batch limit smaller than the busy extension's own backlog: if
        // claiming were a plain global `ORDER BY available_at_ms`, the busy
        // extension's 5 older-enqueued... (all same available_at_ms here, so
        // ordered by id) rows could fill the entire batch, starving the
        // quiet extension. Fair per-extension claiming must include the
        // quiet extension's one due row in the very first batch.
        let claimed = store::claim_due_batch(&db, 3, 1_000).await.expect("claim");
        assert_eq!(claimed.len(), 3);
        assert!(
            claimed.iter().any(|row| row.extension_id == quiet),
            "the quiet extension's only due row must not be starved by the busy extension's backlog"
        );
    }
}
