//! Durable, async, annotation-only outbox for the `is_question`
//! community-enrichment judgment (issue #1831 Phase 1).
//!
//! This mirrors the outbox pattern already used by `room_effect_outbox` and
//! `notification_outbox` (a durable queue table + an async drain worker +
//! retry/backoff), fully decoupled from the synchronous ingress/delivery
//! path — but deliberately simpler: this is a single background drain
//! worker, not a clustered actor with lease contention, so there is no
//! lease token, ownership claim, or supervisor here.
//!
//! - [`judge`]: the fixed contract the drain worker calls against (a
//!   separate agent implements the Jev HTTP client against this trait).
//! - [`schema`]: dual Postgres/SQLite DDL for the two tables.
//! - [`store`]: typed CRUD on top of those tables.
//! - [`drain`]: the async worker that fetches due rows, calls the judge,
//!   and records results.
//!
//! Nothing here is wire-visible: results are stored for later analysis
//! only, and enqueueing is a fire-and-forget, best-effort side effect of
//! archiving a message — it never blocks or delays delivery.

mod drain;
mod jev_client;
mod judge;
mod schema;
mod store;

pub use drain::{
    drain_once, retry_delay_ms, run_drain_loop, DrainOutcome, BASE_RETRY_DELAY_MS, MAX_ATTEMPTS,
    MAX_RETRY_DELAY_MS,
};
pub use jev_client::{JevClient, JevClientConfig, JevClientConfigError};
pub use judge::{JudgeError, JudgmentBatch, MessageJudge, NamedJudgment};
pub use store::{
    dead_letter, enqueue_pending, fetch_due_batch, insert_judgment,
    insert_judgment_batch_and_mark_done, mark_done, record_failure, JudgmentRecord,
    MessageJudgmentOutboxId, PendingJudgmentInput, PendingJudgmentRow, IS_QUESTION_JUDGMENT_NAME,
    MAX_BODY_SNAPSHOT_CHARS, SAFETY_EXPLICIT_JUDGMENT_NAME, SAFETY_HARASSMENT_JUDGMENT_NAME,
    SAFETY_HATE_SPEECH_JUDGMENT_NAME, SAFETY_SELF_HARM_JUDGMENT_NAME,
    SAFETY_VIOLENCE_JUDGMENT_NAME,
};

use crate::db::{Database, DatabaseError};

/// Bootstrap both tables. Idempotent — safe to call on every process start.
///
/// Not yet called from server startup (`crates/waddle-server/src/server`):
/// that wiring, plus spawning [`run_drain_loop`] and constructing the real
/// Jev [`MessageJudge`] implementation, lands in a follow-up PR once the
/// Jev HTTP client is ready (see `judge.rs`'s doc comment).
pub async fn initialize(db: &Database) -> Result<(), MessageJudgmentOutboxError> {
    schema::initialize(db).await
}

#[derive(Debug, thiserror::Error)]
pub enum MessageJudgmentOutboxError {
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("invalid stored stanza-id assigning JID: {0}")]
    InvalidStanzaByJid(String),
}
