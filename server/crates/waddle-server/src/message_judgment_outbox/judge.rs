//! Contract between the judgment outbox drain worker and whatever decision
//! model actually answers the outbox's fixed set of questions about a
//! message body. Kept separate from both sides so the outbox/drain plumbing
//! and the Jev HTTP client can be built and tested independently of each
//! other.

use async_trait::async_trait;

use super::store::{
    IS_QUESTION_JUDGMENT_NAME, SAFETY_EXPLICIT_JUDGMENT_NAME, SAFETY_HARASSMENT_JUDGMENT_NAME,
    SAFETY_HATE_SPEECH_JUDGMENT_NAME, SAFETY_SELF_HARM_JUDGMENT_NAME,
    SAFETY_VIOLENCE_JUDGMENT_NAME,
};

/// The fixed, closed set of judgment categories this outbox asks about a
/// message body. A typed identifier (rather than a bare `String`) for the
/// same reason `xep::*` namespace identifiers are dedicated constants: this
/// is structured data with a known set of values, not free-form text, so a
/// typo or an unrecognized name is a compile error here instead of a
/// silent runtime mismatch. Converts to `&'static str` only at the two
/// genuine I/O boundaries that need one: the Jev Decisions API's JSON
/// question/answer keys, and the `message_judgments.judgment_name` SQL
/// column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JudgmentKind {
    IsQuestion,
    SafetyHateSpeech,
    SafetyExplicit,
    SafetyHarassment,
    SafetyViolence,
    SafetySelfHarm,
}

impl JudgmentKind {
    /// The `message_judgments.judgment_name` value this kind is stored
    /// under, and the key it's asked/answered under in Jev's Decisions API
    /// `questions`/`answers` objects.
    pub fn as_str(self) -> &'static str {
        match self {
            JudgmentKind::IsQuestion => IS_QUESTION_JUDGMENT_NAME,
            JudgmentKind::SafetyHateSpeech => SAFETY_HATE_SPEECH_JUDGMENT_NAME,
            JudgmentKind::SafetyExplicit => SAFETY_EXPLICIT_JUDGMENT_NAME,
            JudgmentKind::SafetyHarassment => SAFETY_HARASSMENT_JUDGMENT_NAME,
            JudgmentKind::SafetyViolence => SAFETY_VIOLENCE_JUDGMENT_NAME,
            JudgmentKind::SafetySelfHarm => SAFETY_SELF_HARM_JUDGMENT_NAME,
        }
    }
}

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
    /// The judgment category this result is for.
    pub judgment_name: JudgmentKind,
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
