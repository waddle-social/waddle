//! Contract between the judgment outbox drain worker and whatever decision
//! model actually answers the outbox's fixed set of questions about a
//! message body. Kept separate from both sides so the outbox/drain plumbing
//! and the Jev HTTP client can be built and tested independently of each
//! other.

use async_trait::async_trait;

/// One named judgment's result, from whichever call in [`JudgmentBatch`]
/// produced it.
///
/// `taxonomy_version` is recorded per judgment, not per batch, even though
/// every judgment in a batch comes from one underlying call: each
/// judgment's `instructions`/`criteria` wording can be revised
/// independently later (e.g. tightening `hate_speech`'s definition without
/// touching `is_question`'s), and a per-judgment version is what lets that
/// show up as schema drift on exactly the row it affects, not every row
/// answered in the same call.
#[derive(Debug, Clone, PartialEq)]
pub struct NamedJudgment {
    /// The `message_judgments.judgment_name` this result is stored under
    /// (e.g. `"is_question"`, `"safety:hate_speech"`).
    pub judgment_name: String,
    /// Probability, in `0.0..=1.0`, that the named condition holds. Every
    /// judgment this outbox asks is a Jev "Noul" (yes/no) question, whose
    /// answer is a bare probability — no separate confidence value (unlike
    /// Jev's "Choice"/"Score" primitives) — see `jev_client.rs`'s module
    /// docs.
    pub probability: f64,
    pub taxonomy_version: String,
}

/// Result of one call to [`MessageJudge::judge`]: every question the outbox
/// currently asks about a single message body, answered together.
///
/// `model_version` and `cost_usd` are batch-level, not per-judgment: they
/// describe the one underlying provider call that produced every judgment
/// in `judgments`, so recording them per-judgment would misrepresent a
/// single call's cost as if it were spent once per question. Callers
/// persisting a batch must attribute `cost_usd` to exactly one stored row
/// (by convention, the first judgment), never to all of them, or a later
/// `SUM(cost_usd)` double- (or N-times-) counts every batched call.
#[derive(Debug, Clone, PartialEq)]
pub struct JudgmentBatch {
    /// Always non-empty: a batch answering zero questions is not a
    /// meaningful result and [`MessageJudge::judge`] implementations must
    /// return [`JudgeError::InvalidResponse`] instead of an empty batch.
    pub judgments: Vec<NamedJudgment>,
    pub model_version: String,
    /// USD cost of the single request that produced every judgment in this
    /// batch, from the provider's own `usage.cost` field — this Phase's
    /// stated purpose is measuring cost-per-thousand-messages, so this is
    /// recorded per batch rather than only sampled or logged.
    pub cost_usd: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum JudgeError {
    #[error("judge transport failure: {0}")]
    Transport(String),
    #[error("judge returned an invalid response: {0}")]
    InvalidResponse(String),
}

/// A decision-model client capable of answering the outbox's fixed set of
/// judgments about one message body in a single call.
///
/// Implemented by the Jev HTTP client; the outbox drain worker depends only
/// on this trait, never on Jev-specific request/response types.
#[async_trait]
pub trait MessageJudge: Send + Sync {
    async fn judge(&self, body: &str) -> Result<JudgmentBatch, JudgeError>;
}
