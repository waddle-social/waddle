use std::collections::HashMap;
use std::fmt;

use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::config::MAX_RESPONSE_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JudgmentKind {
    IsQuestion,
    SafetyHateSpeech,
    SafetyExplicit,
    SafetyHarassment,
    SafetyViolence,
    SafetySelfHarm,
    SafetySpam,
    SafetyScam,
}

impl JudgmentKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IsQuestion => "is_question",
            Self::SafetyHateSpeech => "safety:hate_speech",
            Self::SafetyExplicit => "safety:explicit",
            Self::SafetyHarassment => "safety:harassment",
            Self::SafetyViolence => "safety:violence",
            Self::SafetySelfHarm => "safety:self_harm",
            Self::SafetySpam => "safety:spam",
            Self::SafetyScam => "safety:scam",
        }
    }
}

struct QuestionSpec {
    kind: JudgmentKind,
    taxonomy_version: &'static str,
    instructions: &'static str,
    criteria_true: &'static str,
    criteria_false: &'static str,
}

const QUESTIONS: &[QuestionSpec] = &[
    QuestionSpec {
        kind: JudgmentKind::IsQuestion,
        taxonomy_version: "is-question-v1",
        instructions: "Is this chat message phrased as a question that expects an answer or response from someone else in the conversation?",
        criteria_true: "The message asks something and expects a reply from someone else.",
        criteria_false: "The message is a statement, reaction, or does not expect a reply.",
    },
    QuestionSpec {
        kind: JudgmentKind::SafetyHateSpeech,
        taxonomy_version: "safety-hate-speech-v1",
        instructions: "Does this chat message contain hate speech: content that attacks, demeans, or incites hatred or violence against people based on a protected characteristic such as race, ethnicity, religion, gender, sexual orientation, or disability?",
        criteria_true: "The message attacks, demeans, or incites hatred or violence against people based on a protected characteristic.",
        criteria_false: "The message does not attack, demean, or incite hatred or violence based on a protected characteristic.",
    },
    QuestionSpec {
        kind: JudgmentKind::SafetyExplicit,
        taxonomy_version: "safety-explicit-v1",
        instructions: "Does this chat message contain sexually explicit content: graphic sexual descriptions or explicit sexual solicitation, as distinct from casual, non-graphic references?",
        criteria_true: "The message contains graphic sexual content or explicit sexual solicitation.",
        criteria_false: "The message does not contain graphic sexual content or explicit sexual solicitation.",
    },
    QuestionSpec {
        kind: JudgmentKind::SafetyHarassment,
        taxonomy_version: "safety-harassment-v1",
        instructions: "Does this chat message harass, bully, insult, or demean a specific person in the conversation, as distinct from criticizing a public figure's actions or ideas in general?",
        criteria_true: "The message directly targets a specific individual with insults, bullying, or demeaning language.",
        criteria_false: "The message does not directly target a specific individual this way.",
    },
    QuestionSpec {
        kind: JudgmentKind::SafetyViolence,
        taxonomy_version: "safety-violence-v1",
        instructions: "Does this chat message threaten violence, or describe or glorify graphic violence against a person, animal, or group?",
        criteria_true: "The message threatens violence, or describes or glorifies graphic violence.",
        criteria_false: "The message does not threaten, describe, or glorify graphic violence.",
    },
    QuestionSpec {
        kind: JudgmentKind::SafetySelfHarm,
        taxonomy_version: "safety-self-harm-v1",
        instructions: "Does this chat message express intent toward self-harm or suicide, or encourage self-harm or suicide in someone else?",
        criteria_true: "The message expresses intent toward self-harm or suicide, or encourages it in someone else.",
        criteria_false: "The message does not express or encourage self-harm or suicide.",
    },
    QuestionSpec {
        kind: JudgmentKind::SafetySpam,
        taxonomy_version: "safety-spam-v1",
        instructions: "Is this chat message spam: unsolicited bulk, promotional, or advertising content that is not part of a genuine conversation, such as product plugs, affiliate links, repeated mass postings, or automated advertising?",
        criteria_true: "The message is unsolicited promotional, advertising, or bulk content rather than genuine conversation.",
        criteria_false: "The message is genuine conversation, not unsolicited promotion or bulk posting.",
    },
    QuestionSpec {
        kind: JudgmentKind::SafetyScam,
        taxonomy_version: "safety-scam-v1",
        instructions: "Is this chat message a scam: an attempt to deceive readers into sending money or cryptocurrency, revealing credentials or personal information, or visiting a malicious link, including phishing, fake giveaways, impersonation, and fraudulent investment offers?",
        criteria_true: "The message tries to deceive readers for money, credentials, personal information, or a malicious link.",
        criteria_false: "The message does not try to deceive readers for money, credentials, personal information, or a malicious link.",
    },
];

#[derive(Clone, Debug, PartialEq)]
pub struct NamedJudgment {
    pub kind: JudgmentKind,
    pub probability: f64,
    pub taxonomy_version: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct JudgmentBatch {
    pub judgments: Vec<NamedJudgment>,
    pub model_version: String,
    /// Cost of the entire provider call. The persistence boundary must
    /// attribute it to one stored judgment, rather than to every answer.
    pub cost_usd: f64,
}

impl JudgmentBatch {
    /// The provider reports dollars, while the actor ledger stores whole
    /// microdollars once for the entire Decisions call.
    pub fn cost_micro_usd(&self) -> Result<u64, Failure> {
        let micro_usd = self.cost_usd * 1_000_000.0;
        if !micro_usd.is_finite() || micro_usd >= u64::MAX as f64 {
            return Err(Failure::InvalidResponse(InvalidResponse::InvalidCost));
        }
        Ok(micro_usd.round() as u64)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidResponse {
    DocumentShape,
    MissingAnswer(JudgmentKind),
    ProbabilityOutOfRange(JudgmentKind),
    InvalidCost,
}

/// Failure metadata is intentionally independent of provider text and the
/// submitted message body. Neither `Debug` nor `Display` can expose them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Transport,
    HttpStatus(u16),
    ResponseTooLarge,
    InvalidResponse(InvalidResponse),
}

impl Failure {
    pub const fn category(self) -> &'static str {
        match self {
            Self::Transport => "transport",
            Self::HttpStatus(_) => "http_status",
            Self::ResponseTooLarge => "response_too_large",
            Self::InvalidResponse(_) => "invalid_response",
        }
    }

    pub const fn http_status(self) -> Option<u16> {
        match self {
            Self::HttpStatus(status) => Some(status),
            _ => None,
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HttpStatus(status) => write!(f, "Jev returned HTTP {status}"),
            _ => write!(f, "Jev call failed: {}", self.category()),
        }
    }
}

impl std::error::Error for Failure {}

/// Build one Decisions request covering every supported classification.
pub fn request_body(model: &str, state: &str) -> String {
    let mut questions = Map::with_capacity(QUESTIONS.len());
    for spec in QUESTIONS {
        questions.insert(
            spec.kind.as_str().to_string(),
            json!({
                "type": "noul",
                "instructions": spec.instructions,
                "criteria": {
                    "true": spec.criteria_true,
                    "false": spec.criteria_false,
                }
            }),
        );
    }
    json!({
        "model": model,
        "questions": Value::Object(questions),
        "state": state,
    })
    .to_string()
}

#[derive(Deserialize)]
struct DecisionsResponse {
    answers: HashMap<String, NoulAnswer>,
    model: String,
    usage: DecisionsUsage,
}

#[derive(Deserialize)]
struct NoulAnswer {
    noul: f64,
}

#[derive(Deserialize)]
struct DecisionsUsage {
    cost: f64,
}

/// Parse the whole provider response or reject the whole batch. The body is
/// never included in an error, even if the provider echoes submitted text.
pub fn parse_http_response(
    status: u16,
    body: &str,
    fallback_model: &str,
) -> Result<JudgmentBatch, Failure> {
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(Failure::ResponseTooLarge);
    }
    if !(200..300).contains(&status) {
        return Err(Failure::HttpStatus(status));
    }
    let response: DecisionsResponse = serde_json::from_str(body)
        .map_err(|_| Failure::InvalidResponse(InvalidResponse::DocumentShape))?;
    let mut judgments = Vec::with_capacity(QUESTIONS.len());
    for spec in QUESTIONS {
        let answer = response
            .answers
            .get(spec.kind.as_str())
            .ok_or(Failure::InvalidResponse(InvalidResponse::MissingAnswer(
                spec.kind,
            )))?;
        if !(0.0..=1.0).contains(&answer.noul) {
            return Err(Failure::InvalidResponse(
                InvalidResponse::ProbabilityOutOfRange(spec.kind),
            ));
        }
        judgments.push(NamedJudgment {
            kind: spec.kind,
            probability: answer.noul,
            taxonomy_version: spec.taxonomy_version,
        });
    }
    if !response.usage.cost.is_finite() || response.usage.cost < 0.0 {
        return Err(Failure::InvalidResponse(InvalidResponse::InvalidCost));
    }
    Ok(JudgmentBatch {
        judgments,
        model_version: if response.model.is_empty() {
            fallback_model.to_string()
        } else {
            response.model
        },
        cost_usd: response.usage.cost,
    })
}
