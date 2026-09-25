//! Contract between the judgment outbox drain worker and whatever decision
//! model actually answers `is_question`. Kept separate from both sides so the
//! outbox/drain plumbing and the Jev HTTP client can be built and tested
//! independently of each other.

use async_trait::async_trait;

/// Result of asking whether a message body is a question.
///
/// `taxonomy_version` and `model_version` are recorded on every judgment so a
/// later change to either shows up as schema drift in the stored rows, not as
/// unexplained model drift when judgments are compared over time. There is
/// no `confidence` field: `is_question` is a Jev "Noul" (yes/no) judgment,
/// and Jev's Decisions API reports only a probability for Noul answers, no
/// separate confidence value (unlike its "Choice"/"Score" primitives, which
/// do) — see `jev_client.rs`'s module docs.
#[derive(Debug, Clone, PartialEq)]
pub struct IsQuestionJudgment {
    /// Probability, in `0.0..=1.0`, that the body is a question.
    pub probability: f64,
    pub taxonomy_version: String,
    pub model_version: String,
    /// USD cost of the request that produced this judgment, from the
    /// provider's own `usage.cost` field — this Phase's stated purpose is
    /// measuring cost-per-thousand-messages, so this is recorded per
    /// judgment rather than only sampled or logged.
    pub cost_usd: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum JudgeError {
    #[error("judge transport failure: {0}")]
    Transport(String),
    #[error("judge returned an invalid response: {0}")]
    InvalidResponse(String),
}

/// A decision-model client capable of answering the `is_question` judgment.
///
/// Implemented by the Jev HTTP client; the outbox drain worker depends only
/// on this trait, never on Jev-specific request/response types.
#[async_trait]
pub trait MessageJudge: Send + Sync {
    async fn is_question(&self, body: &str) -> Result<IsQuestionJudgment, JudgeError>;
}
