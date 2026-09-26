//! Pure request/response logic for Jev's Decisions API, ported from the
//! host-side `message_judgment_outbox::jev_client` (issue #1831 Phase B —
//! moved out of core server code into this guest extension, since Jev's
//! specific vendor/prompt/taxonomy choices are Waddle product policy, not
//! host infrastructure). See that module's original doc comment (in git
//! history) for the full wire-format citation; unchanged here.
//!
//! Deliberately free of any I/O — [`build_request_body`]/[`parse_response`]
//! are the same pure, separately-testable functions the host-side client
//! had; only the transport changed (from `reqwest` to this guest's
//! `runtime::http-request` WIT import, wired in `lib.rs`).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// The wire token a judgment category is stored/sent under — matching
/// `message_judgments.judgment_name` (server) and
/// `waddle_xmpp_client::xep::safety_scores::SafetyCategory::as_wire`
/// (client) exactly, so a score this extension emits is one the already
/// -merged client parsers recognize.
pub(crate) mod category {
    pub(crate) const IS_QUESTION: &str = "is_question";
    pub(crate) const SAFETY_HATE_SPEECH: &str = "safety:hate_speech";
    pub(crate) const SAFETY_EXPLICIT: &str = "safety:explicit";
    pub(crate) const SAFETY_HARASSMENT: &str = "safety:harassment";
    pub(crate) const SAFETY_VIOLENCE: &str = "safety:violence";
    pub(crate) const SAFETY_SELF_HARM: &str = "safety:self_harm";
}

struct JudgmentQuestionSpec {
    judgment_name: &'static str,
    taxonomy_version: &'static str,
    instructions: &'static str,
    criteria_true: &'static str,
    criteria_false: &'static str,
}

/// Every judgment this extension asks about a message body, asked together
/// in one Decisions API call (cheaper than one call per judgment — `state`'s
/// input tokens are billed once per call). Adding a new judgment kind is
/// adding one entry here.
const JUDGMENT_QUESTIONS: &[JudgmentQuestionSpec] = &[
    JudgmentQuestionSpec {
        judgment_name: category::IS_QUESTION,
        taxonomy_version: "is-question-v1",
        instructions: "Is this chat message phrased as a question that expects an answer or response from someone else in the conversation?",
        criteria_true: "The message asks something and expects a reply from someone else.",
        criteria_false: "The message is a statement, reaction, or does not expect a reply.",
    },
    JudgmentQuestionSpec {
        judgment_name: category::SAFETY_HATE_SPEECH,
        taxonomy_version: "safety-hate-speech-v1",
        instructions: "Does this chat message contain hate speech: content that attacks, demeans, or incites hatred or violence against people based on a protected characteristic such as race, ethnicity, religion, gender, sexual orientation, or disability?",
        criteria_true: "The message attacks, demeans, or incites hatred or violence against people based on a protected characteristic.",
        criteria_false: "The message does not attack, demean, or incite hatred or violence based on a protected characteristic.",
    },
    JudgmentQuestionSpec {
        judgment_name: category::SAFETY_EXPLICIT,
        taxonomy_version: "safety-explicit-v1",
        instructions: "Does this chat message contain sexually explicit content: graphic sexual descriptions or explicit sexual solicitation, as distinct from casual, non-graphic references?",
        criteria_true: "The message contains graphic sexual content or explicit sexual solicitation.",
        criteria_false: "The message does not contain graphic sexual content or explicit sexual solicitation.",
    },
    JudgmentQuestionSpec {
        judgment_name: category::SAFETY_HARASSMENT,
        taxonomy_version: "safety-harassment-v1",
        instructions: "Does this chat message harass, bully, insult, or demean a specific person in the conversation, as distinct from criticizing a public figure's actions or ideas in general?",
        criteria_true: "The message directly targets a specific individual with insults, bullying, or demeaning language.",
        criteria_false: "The message does not directly target a specific individual this way.",
    },
    JudgmentQuestionSpec {
        judgment_name: category::SAFETY_VIOLENCE,
        taxonomy_version: "safety-violence-v1",
        instructions: "Does this chat message threaten violence, or describe or glorify graphic violence against a person, animal, or group?",
        criteria_true: "The message threatens violence, or describes or glorifies graphic violence.",
        criteria_false: "The message does not threaten, describe, or glorify graphic violence.",
    },
    JudgmentQuestionSpec {
        judgment_name: category::SAFETY_SELF_HARM,
        taxonomy_version: "safety-self-harm-v1",
        instructions: "Does this chat message express intent toward self-harm or suicide, or encourage self-harm or suicide in someone else?",
        criteria_true: "The message expresses intent toward self-harm or suicide, or encourages it in someone else.",
        criteria_false: "The message does not express or encourage self-harm or suicide.",
    },
];

#[derive(Serialize)]
struct NoulQuestion<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    instructions: &'a str,
    criteria: NoulCriteria<'a>,
}

#[derive(Serialize)]
struct NoulCriteria<'a> {
    #[serde(rename = "true")]
    when_true: &'a str,
    #[serde(rename = "false")]
    when_false: &'a str,
}

/// One named judgment result: a category, its probability, and the
/// taxonomy version its question was asked under.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NamedJudgment {
    pub(crate) judgment_name: &'static str,
    pub(crate) probability: f64,
    pub(crate) taxonomy_version: &'static str,
}

/// Result of one Decisions API call: every judgment asked, plus the actual
/// model snapshot that answered.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct JudgmentBatch {
    pub(crate) judgments: Vec<NamedJudgment>,
    pub(crate) model_version: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum JudgeError {
    Transport(String),
    InvalidResponse(String),
}

impl std::fmt::Display for JudgeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(message) => write!(formatter, "judge transport failure: {message}"),
            Self::InvalidResponse(message) => {
                write!(formatter, "judge returned an invalid response: {message}")
            }
        }
    }
}

/// Pure, separately testable request-body builder. Asks every question in
/// [`JUDGMENT_QUESTIONS`] against the same `state` in one request.
pub(crate) fn build_request_body(model: &str, body: &str) -> Value {
    let mut questions = serde_json::Map::with_capacity(JUDGMENT_QUESTIONS.len());
    for spec in JUDGMENT_QUESTIONS {
        let question = NoulQuestion {
            kind: "noul",
            instructions: spec.instructions,
            criteria: NoulCriteria {
                when_true: spec.criteria_true,
                when_false: spec.criteria_false,
            },
        };
        questions.insert(
            spec.judgment_name.to_string(),
            serde_json::to_value(question).expect("NoulQuestion always serializes"),
        );
    }
    serde_json::json!({
        "model": model,
        "questions": Value::Object(questions),
        "state": body,
    })
}

#[derive(Deserialize)]
struct DecisionsResponse {
    answers: HashMap<String, NoulAnswer>,
    model: String,
}

#[derive(Deserialize)]
struct NoulAnswer {
    noul: f64,
}

/// Pure, separately testable response parser. Never panics on an
/// out-of-range or missing value: either becomes a typed
/// [`JudgeError::InvalidResponse`] for the *whole* batch, never a partially
/// -built result.
pub(crate) fn parse_response(
    response_body: &[u8],
    fallback_model_version: &str,
) -> Result<JudgmentBatch, JudgeError> {
    let response = serde_json::from_slice::<DecisionsResponse>(response_body).map_err(|error| {
        JudgeError::InvalidResponse(format!(
            "jev response did not match the documented Decisions API shape: {error}"
        ))
    })?;
    let mut judgments = Vec::with_capacity(JUDGMENT_QUESTIONS.len());
    for spec in JUDGMENT_QUESTIONS {
        let answer = response.answers.get(spec.judgment_name).ok_or_else(|| {
            JudgeError::InvalidResponse(format!(
                "jev response did not include an answer for \"{}\"",
                spec.judgment_name
            ))
        })?;
        let probability = answer.noul;
        if !(0.0..=1.0).contains(&probability) {
            return Err(JudgeError::InvalidResponse(format!(
                "jev \"{}\" probability {probability} was outside 0.0..=1.0",
                spec.judgment_name
            )));
        }
        judgments.push(NamedJudgment {
            judgment_name: spec.judgment_name,
            probability,
            taxonomy_version: spec.taxonomy_version,
        });
    }
    let model_version = if response.model.is_empty() {
        fallback_model_version.to_string()
    } else {
        response.model
    };
    Ok(JudgmentBatch {
        judgments,
        model_version,
    })
}

/// Extracts `error.message` per the Decisions API's documented error shape
/// (`{"error": {"code": <status>, "message": "..."}}`). `None` if the body
/// doesn't match — callers fall back to a raw snippet rather than failing.
fn documented_error_message(body: &[u8]) -> Option<String> {
    let document = serde_json::from_slice::<Value>(body).ok()?;
    document
        .pointer("/error/message")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Non-secret-leaking message for a non-2xx Jev response.
pub(crate) fn transport_error_message(status: u16, body: &[u8]) -> String {
    const MAX_SNIPPET_BYTES: usize = 256;
    if let Some(message) = documented_error_message(body) {
        return format!("jev returned HTTP {status}: {message}");
    }
    let snippet: String = String::from_utf8_lossy(body)
        .chars()
        .filter(|character| !character.is_control() || character.is_whitespace())
        .take(MAX_SNIPPET_BYTES)
        .collect::<String>()
        .trim()
        .to_string();
    if snippet.is_empty() {
        format!("jev returned HTTP {status}")
    } else {
        format!("jev returned HTTP {status}: {snippet}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_answers(overrides: &[(&str, f64)]) -> Value {
        let mut answers = serde_json::Map::new();
        for spec in JUDGMENT_QUESTIONS {
            let probability = overrides
                .iter()
                .find(|(name, _)| *name == spec.judgment_name)
                .map(|(_, probability)| *probability)
                .unwrap_or(0.1);
            answers.insert(
                spec.judgment_name.to_string(),
                serde_json::json!({ "noul": probability, "type": "noul" }),
            );
        }
        Value::Object(answers)
    }

    fn full_response(overrides: &[(&str, f64)], model: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "answers": full_answers(overrides),
            "model": model,
            "usage": { "cost": 0.00002, "input_tokens": 476, "output_tokens": 70 }
        }))
        .expect("test JSON always serializes")
    }

    fn judgment(batch: &JudgmentBatch, judgment_name: &str) -> f64 {
        batch
            .judgments
            .iter()
            .find(|j| j.judgment_name == judgment_name)
            .unwrap_or_else(|| panic!("no judgment named {judgment_name} in batch"))
            .probability
    }

    #[test]
    fn build_request_body_asks_every_judgment_in_one_request() {
        let body = build_request_body("typesafe/jev-1.13", "are we there yet?");
        let questions = body
            .get("questions")
            .and_then(Value::as_object)
            .expect("questions object");
        assert_eq!(body.get("model").expect("model field"), "typesafe/jev-1.13");
        assert_eq!(body.get("state").expect("state field"), "are we there yet?");
        assert_eq!(questions.len(), JUDGMENT_QUESTIONS.len());
        for spec in JUDGMENT_QUESTIONS {
            let question = questions
                .get(spec.judgment_name)
                .unwrap_or_else(|| panic!("missing question for {}", spec.judgment_name));
            assert_eq!(question.get("type").expect("type field"), "noul");
        }
    }

    #[test]
    fn parse_response_parses_every_judgment_from_one_documented_response() {
        let body = full_response(
            &[
                (category::IS_QUESTION, 0.92),
                (category::SAFETY_HATE_SPEECH, 0.03),
            ],
            "typesafe/jev-1.13-20260917",
        );
        let batch = parse_response(&body, "fallback").expect("well-formed response should parse");
        assert_eq!(batch.judgments.len(), JUDGMENT_QUESTIONS.len());
        assert_eq!(judgment(&batch, category::IS_QUESTION), 0.92);
        assert_eq!(batch.model_version, "typesafe/jev-1.13-20260917");
    }

    #[test]
    fn parse_response_falls_back_to_configured_model_when_response_omits_it() {
        let body = full_response(&[], "");
        let batch = parse_response(&body, "fallback-model")
            .expect("empty model field should still parse, using the fallback");
        assert_eq!(batch.model_version, "fallback-model");
    }

    #[test]
    fn parse_response_rejects_response_missing_one_judgment() {
        let body = serde_json::to_vec(&serde_json::json!({
            "answers": { category::IS_QUESTION: { "noul": 0.5, "type": "noul" } },
            "model": "typesafe/jev-1.13-20260917",
            "usage": { "cost": 0.0 }
        }))
        .expect("serialize");
        let error = parse_response(&body, "fallback")
            .expect_err("a response missing an expected judgment must be rejected entirely");
        assert!(matches!(error, JudgeError::InvalidResponse(_)));
    }

    #[test]
    fn parse_response_rejects_probability_out_of_range() {
        let body = full_response(&[(category::SAFETY_VIOLENCE, 1.5)], "typesafe/jev-1.13");
        let error = parse_response(&body, "fallback")
            .expect_err("out-of-range probability must be rejected");
        assert!(matches!(error, JudgeError::InvalidResponse(_)));
    }

    #[test]
    fn parse_response_rejects_non_json_body() {
        let error = parse_response(b"not json at all", "fallback")
            .expect_err("non-JSON body must be rejected, not panic");
        assert!(matches!(error, JudgeError::InvalidResponse(_)));
    }

    #[test]
    fn transport_error_message_uses_documented_error_shape() {
        let body = br#"{"error":{"code":503,"message":"Service temporarily unavailable"}}"#;
        let message = transport_error_message(503, body);
        assert!(message.contains("503"));
        assert!(message.contains("Service temporarily unavailable"));
    }

    #[test]
    fn transport_error_message_falls_back_to_snippet() {
        let message = transport_error_message(500, b"plain text failure");
        assert!(message.contains("500"));
        assert!(message.contains("plain text failure"));
    }
}
