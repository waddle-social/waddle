//! Durable, annotation-only outbox for the `is_question`/safety
//! community-enrichment judgments (issue #1831).
//!
//! This mirrors the outbox pattern already used by `room_effect_outbox` and
//! `notification_outbox` (a durable queue table + an async drain worker +
//! retry/backoff), decoupled from the synchronous ingress/delivery path in
//! that judging a message never blocks or delays delivery — but
//! deliberately simpler than those two: this is a single background drain
//! worker, not a clustered actor with lease contention, so there is no
//! lease token, ownership claim, or supervisor here.
//!
//! - [`judge`]: the fixed contract the drain worker calls against.
//! - [`jev_client`]: the real Jev/OpenRouter HTTP [`judge::MessageJudge`].
//! - [`schema`]: dual Postgres/SQLite DDL for the two tables.
//! - [`store`]: typed CRUD on top of those tables.
//! - [`drain`]: the async worker that fetches due rows, calls the judge,
//!   and records results.
//!
//! Nothing here is wire-visible: results are stored for later analysis
//! only. **Enqueueing is not fire-and-forget**: `ingress::durable`'s
//! `apply_durable` calls [`store::enqueue_pending_in_tx`] (via
//! `ingress_uow::MessageJudgmentOutboxRepository`) inside the exact same
//! database transaction that writes the archive row it accompanies, so the
//! two commit or roll back together (#1831 Phase 2) — see that function's
//! docs for why this is the correct seam (not the `*_immediate.rs` "Phase
//! C" executors, which also run during the two-phase planning pass and
//! would enqueue for plans later rejected and never actually committed).

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
pub use judge::{JudgeError, JudgmentBatch, JudgmentKind, MessageJudge, NamedJudgment};
pub use store::{
    dead_letter, enqueue_pending, enqueue_pending_in_tx, fetch_due_batch, insert_judgment,
    insert_judgment_batch_and_mark_done, mark_done, record_failure, JudgmentRecord,
    MessageJudgmentOutboxId, PendingJudgmentInput, PendingJudgmentRow, IS_QUESTION_JUDGMENT_NAME,
    MAX_BODY_SNAPSHOT_CHARS, SAFETY_EXPLICIT_JUDGMENT_NAME, SAFETY_HARASSMENT_JUDGMENT_NAME,
    SAFETY_HATE_SPEECH_JUDGMENT_NAME, SAFETY_SELF_HARM_JUDGMENT_NAME,
    SAFETY_VIOLENCE_JUDGMENT_NAME,
};

use crate::db::{Database, DatabaseError};

/// Bootstrap both tables. Idempotent — safe to call on every process start.
///
/// Called from server startup (`server::http::spawn_message_judgment_outbox`)
/// whenever `ServerConfig::message_judgment_outbox.enabled` is true, right
/// before [`run_drain_loop`] is spawned against a real Jev
/// [`MessageJudge`] (#1831 Phase 2) — see that function's doc comment.
/// Left uncalled when the flag is false, matching this feature's
/// measurement-only, default-off contract: no table, no rows, no worker.
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
