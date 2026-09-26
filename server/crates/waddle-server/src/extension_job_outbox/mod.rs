//! Generic, host-owned durable after-commit job queue for `waddle-extension`
//! guests (issue #1831 Phase B). Replaces `message_judgment_outbox`, which
//! was Jev-shaped (a `JudgmentKind` enum, a `cost_usd` column, a
//! judgment-name-keyed idempotency index) and lived in core server code
//! that had no business knowing about one vendor's product policy.
//!
//! This module owns only the durable queue/lease/retry/dead-letter
//! machinery — claiming, backoff, and finalization are entirely
//! extension-agnostic (`extension_id` + `job_kind` route a row to whichever
//! loaded/granted actor should process it; the host never interprets a
//! job's `body_snapshot` itself). The judgment *policy* — what questions to
//! ask, which vendor to ask them of, how to score the answers — lives
//! entirely in the guest `waddle-extension` (see `server/extensions/`),
//! reached through the ordinary `framework.handle-event` export via a
//! `DurableJob` event (see `waddle_extensions::types::events::DurableJob`).
//!
//! - [`schema`]: dual Postgres/SQLite DDL for `extension_job_outbox`.
//! - [`store`]: typed CRUD, including the three launch-blocking guardrails
//!   (attempt-on-claim, lease-checked finalize, fair per-extension
//!   claiming) — see that module's docs for the full reasoning.
//! - [`drain`]: the async worker that claims due rows, invokes the
//!   extension manager, and either finalizes success (recording the result
//!   and emitting the XEP-0422 wire effect via [`wire`]) or
//!   reschedules/dead-letters failure.
//! - [`wire`]: turns a successful job's typed judgment result into the
//!   actual XEP-0422 `<apply-to>` `urn:waddle:safety-scores:1` broadcast —
//!   the "wire delivery" piece #1842/#1853 explicitly deferred, and what
//!   the already-merged Apple/Android/web client parsers
//!   (`waddle_xmpp_client::xep::safety_scores`) are sitting inert waiting
//!   to consume.
//!
//! **Enqueueing is grant-derived, not config-flag-gated.** The real
//! production enqueue site (`ingress::durable::apply_durable`, via
//! `ingress_uow::ExtensionJobOutboxRepository`) only enqueues a row when
//! `ExtensionManager::durable_job_grant_holder` currently names a
//! loaded/granted extension for the job kind in question — never a static
//! `ServerConfig` boolean. A boolean gate would let an operator revoke an
//! extension's grant without also flipping a separate flag, leaving
//! message bodies accumulate in the queue with nothing ever consuming
//! them; a live-manager-state gate cannot go out of sync with the grant it
//! reads, because it *is* the grant.

mod drain;
mod runner;
mod store;
mod wire;

pub mod schema;

pub use drain::{
    drain_once, retry_delay_ms, run_drain_loop, DrainOutcome, DurableJobRunOutcome,
    DurableJobRunner, BASE_RETRY_DELAY_MS, MAX_ATTEMPTS, MAX_RETRY_DELAY_MS,
};
pub use runner::ExtensionManagerDurableJobRunner;
pub use store::{
    claim_due_batch, dead_letter, enqueue_pending, enqueue_pending_in_tx, mark_done,
    record_failure, ClaimedJob, ExtensionJobOutboxId, ExtensionJobOutboxLeaseToken,
    PendingJobInput, CLAIM_TIMEOUT_MS, MAX_BODY_SNAPSHOT_CHARS,
};
pub use wire::{SafetyScoresWireSink, WebSocketStateSafetyScoresWireSink, WireSinkError};

/// Job kind for the community-safety/is-question judgment (the
/// `community-safety-judge` extension's one declared job kind). A typed
/// constant, not an ad-hoc string literal at each call site — the same
/// convention this codebase's `xep::*` namespace identifiers follow.
pub const MESSAGE_JUDGE_JOB_KIND: &str = "message-judge";

use crate::db::{Database, DatabaseError};

/// Bootstrap the `extension_job_outbox` table. Idempotent — safe to call on
/// every process start. Unlike `message_judgment_outbox::initialize`, this
/// is called unconditionally at startup (see
/// `server::http::spawn_extension_job_outbox`): the table always exists,
/// and whether any row is ever enqueued into it is decided per-job-kind at
/// enqueue time by the grant-derived gate, not by a single global feature
/// flag deciding whether the table itself exists.
pub async fn initialize(db: &Database) -> Result<(), ExtensionJobOutboxError> {
    schema::initialize(db).await
}

#[derive(Debug, thiserror::Error)]
pub enum ExtensionJobOutboxError {
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("claimed extension_job_outbox row is missing its lease token")]
    MissingLeaseToken,
    #[error("invalid stored plugin id: {0}")]
    InvalidPluginId(String),
    #[error("invalid stored job kind: {0}")]
    InvalidJobKind(String),
    #[error("invalid stored waddle id: {0}")]
    InvalidWaddleId(String),
    #[error("invalid stored room jid: {0}")]
    InvalidRoomJid(String),
    #[error("invalid stored target stanza-id assigning JID: {0}")]
    InvalidStanzaByJid(String),
}
