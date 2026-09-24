//! Contract between the judgment outbox drain worker and whatever decision
//! model actually answers `is_question`. Kept separate from both sides so the
//! outbox/drain plumbing and the Jev HTTP client can be built and tested
//! independently of each other.

use async_trait::async_trait;

/// Result of asking whether a message body is a question.
///
/// `taxonomy_version` and `model_version` are recorded on every judgment so a
/// later change to either shows up as schema drift in the stored rows, not as
/// unexplained model drift when judgments are compared over time.
#[derive(Debug, Clone, PartialEq)]
pub struct IsQuestionJudgment {
    /// Probability, in `0.0..=1.0`, that the body is a question.
    pub probability: f64,
    /// Model-reported confidence, in `0.0..=1.0`, in that probability.
    pub confidence: f64,
    pub taxonomy_version: String,
    pub model_version: String,
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
