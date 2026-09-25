//! HTTP client for Jev, TypeSafe AI's "System One" structured decision
//! model, used to answer every judgment defined by [`JUDGMENT_QUESTIONS`]
//! in a single call, per [`super::judge::MessageJudge`]. Jev is available
//! through this codebase's existing OpenRouter account (the same
//! `OPENROUTER_API_KEY`/secret already deployed for the `ai-chatbot`
//! extension's provider calls — see
//! `server/extensions/ai-chatbot/src/provider.rs` — so wiring this in is
//! not a new vendor integration).
//!
//! # Wire format: per OpenRouter's official Decisions API docs
//!
//! Jev is *not* a chat-completions model: it has its own typed "Decisions"
//! API, confirmed from OpenRouter's own documentation
//! (<https://openrouter.ai/docs/guides/community/jev> and
//! <https://openrouter.ai/docs/api/api-reference/alphadecisions/submit-a-decisions-request>,
//! fetched 2026-09-25):
//!
//! - `POST https://openrouter.ai/api/alpha/decisions`, `Authorization: Bearer
//!   <OpenRouter API key>` — the same key, same account, as the existing
//!   OpenRouter usage in this repo.
//! - Request: `{"model": "typesafe/jev-1.13", "questions": {"<key>": {"type":
//!   "noul"|"choice"|"score", "instructions": "...", "criteria": ...}, ...},
//!   "state": <the content to evaluate>}`. The Decisions API accepts any
//!   number of named questions per request, all evaluated against the same
//!   `state` — this client asks every judgment in [`JUDGMENT_QUESTIONS`] this
//!   way, in one request, rather than one call per judgment (cheaper, since
//!   `state`'s input tokens are billed once per call, not once per
//!   judgment). Every judgment here is a "Noul" question (a yes/no
//!   condition), so `criteria` is `{"true": "...", "false": "..."}`.
//! - Response: `{"answers": {"<key>": {"noul": <0.0..=1.0>, "type":
//!   "noul"}, ...}, "model": "typesafe/jev-1.13-<date>", "usage": {"cost":
//!   <USD>, "input_tokens": N, "output_tokens": N}, ...}`. **A Noul answer
//!   has no `confidence` field** — only `Choice`/`Score` answers do — which
//!   is why [`super::judge::NamedJudgment`] does not carry one.
//! - Errors: `{"error": {"code": <status>, "message": "..."}}` for every
//!   non-2xx status the docs enumerate (400/401/402/403/404/413/429/5xx).
//!
//! This has not yet been exercised against a live Jev/OpenRouter account
//! from this codebase — it is built directly from OpenRouter's published API
//! reference, not from guessing — so [`build_request_body`] and
//! [`parse_response`] are still kept as small, separately testable pure
//! functions: if a live call ever turns up a documentation/reality mismatch,
//! only this file (and its tests) needs to change.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::{redirect, Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::judge::{JudgeError, JudgmentBatch, JudgmentKind, MessageJudge, NamedJudgment};

/// Jev's Decisions API endpoint, confirmed from OpenRouter's own docs (see
/// module docs). Not a guess: this is the one and only documented endpoint
/// for the Decisions surface.
const DEFAULT_ENDPOINT: &str = "https://openrouter.ai/api/alpha/decisions";

/// Pinned model identifier, rather than the `~typesafe/jev-latest` alias
/// OpenRouter also documents: a judgment feature that records `model_version`
/// on every row (to detect model drift, see [`IsQuestionJudgment`]'s docs)
/// should not have its underlying model silently change out from under a
/// fixed config value. Bump this deliberately when moving to a newer Jev
/// release.
const DEFAULT_MODEL: &str = "typesafe/jev-1.13";

/// This is meant to be a fast, synchronous-feeling judgment call (that is
/// the whole premise of the feature), so the timeout is short.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(4);

/// Response bodies are expected to be a small typed-answer JSON document,
/// not generated text, so this cap is generous relative to that but still
/// bounds a misbehaving or compromised endpoint.
const DEFAULT_MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// One named "Noul" (yes/no) question this client asks Jev about a message
/// body, and the fixed wording behind it.
///
/// `taxonomy_version` is Waddle's own concept, not part of Jev's response —
/// bump a question's version whenever its `instructions`/`criteria` wording
/// changes, so a stored judgment's `taxonomy_version` shows the question's
/// wording drifted even though `model_version` didn't.
struct JudgmentQuestionSpec {
    /// The judgment category this question's answer is stored under, and
    /// the key it's asked under in the Decisions API request/response
    /// (`questions`/`answers`) via [`JudgmentKind::as_str`].
    judgment_name: JudgmentKind,
    taxonomy_version: &'static str,
    instructions: &'static str,
    criteria_true: &'static str,
    criteria_false: &'static str,
}

/// Every judgment this outbox currently asks about a message body, asked
/// together in one Decisions API call (see the module docs on why one call
/// rather than one per judgment). Adding a new judgment kind is adding one
/// entry here — no other code in this file changes.
const JUDGMENT_QUESTIONS: &[JudgmentQuestionSpec] = &[
    JudgmentQuestionSpec {
        judgment_name: JudgmentKind::IsQuestion,
        taxonomy_version: "is-question-v1",
        instructions: "Is this chat message phrased as a question that expects an answer or response from someone else in the conversation?",
        criteria_true: "The message asks something and expects a reply from someone else.",
        criteria_false: "The message is a statement, reaction, or does not expect a reply.",
    },
    JudgmentQuestionSpec {
        judgment_name: JudgmentKind::SafetyHateSpeech,
        taxonomy_version: "safety-hate-speech-v1",
        instructions: "Does this chat message contain hate speech: content that attacks, demeans, or incites hatred or violence against people based on a protected characteristic such as race, ethnicity, religion, gender, sexual orientation, or disability?",
        criteria_true: "The message attacks, demeans, or incites hatred or violence against people based on a protected characteristic.",
        criteria_false: "The message does not attack, demean, or incite hatred or violence based on a protected characteristic.",
    },
    JudgmentQuestionSpec {
        judgment_name: JudgmentKind::SafetyExplicit,
        taxonomy_version: "safety-explicit-v1",
        instructions: "Does this chat message contain sexually explicit content: graphic sexual descriptions or explicit sexual solicitation, as distinct from casual, non-graphic references?",
        criteria_true: "The message contains graphic sexual content or explicit sexual solicitation.",
        criteria_false: "The message does not contain graphic sexual content or explicit sexual solicitation.",
    },
    JudgmentQuestionSpec {
        judgment_name: JudgmentKind::SafetyHarassment,
        taxonomy_version: "safety-harassment-v1",
        instructions: "Does this chat message harass, bully, insult, or demean a specific person in the conversation, as distinct from criticizing a public figure's actions or ideas in general?",
        criteria_true: "The message directly targets a specific individual with insults, bullying, or demeaning language.",
        criteria_false: "The message does not directly target a specific individual this way.",
    },
    JudgmentQuestionSpec {
        judgment_name: JudgmentKind::SafetyViolence,
        taxonomy_version: "safety-violence-v1",
        instructions: "Does this chat message threaten violence, or describe or glorify graphic violence against a person, animal, or group?",
        criteria_true: "The message threatens violence, or describes or glorifies graphic violence.",
        criteria_false: "The message does not threaten, describe, or glorify graphic violence.",
    },
    JudgmentQuestionSpec {
        judgment_name: JudgmentKind::SafetySelfHarm,
        taxonomy_version: "safety-self-harm-v1",
        instructions: "Does this chat message express intent toward self-harm or suicide, or encourage self-harm or suicide in someone else?",
        criteria_true: "The message expresses intent toward self-harm or suicide, or encourages it in someone else.",
        criteria_false: "The message does not express or encourage self-harm or suicide.",
    },
];

/// Configuration for a [`JevClient`].
///
/// `endpoint` and `model` default to Jev's documented Decisions API and a
/// pinned model version (see the constants above) — both confirmed from
/// OpenRouter's published docs, not guessed. `api_key` has no meaningful
/// default: it must come from real deployment configuration/secrets, and a
/// `JevClient` built from `JevClientConfig::default()` unmodified always
/// fails validation in [`JevClient::new`], because an empty `api_key` means
/// "unconfigured", not "use some fallback".
#[derive(Clone)]
pub struct JevClientConfig {
    /// Base HTTPS URL for the Jev decision endpoint.
    pub endpoint: String,
    /// Model identifier passed in every request body.
    pub model: String,
    /// OpenRouter API key (the same key/secret already deployed for
    /// `ai-chatbot`'s OpenRouter usage). Never logged, never included in
    /// `Display`/`Debug` output, never included in an error message. Empty
    /// means "unconfigured".
    pub api_key: String,
    /// Per-request timeout (connect + total). A few seconds — this call is
    /// meant to be fast.
    pub request_timeout: Duration,
}

impl Default for JevClientConfig {
    fn default() -> Self {
        Self {
            endpoint: DEFAULT_ENDPOINT.to_string(),
            model: DEFAULT_MODEL.to_string(),
            // Empty means "unconfigured": never a placeholder secret.
            api_key: String::new(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }
}

impl fmt::Debug for JevClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JevClientConfig")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("api_key", &"<redacted>")
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

/// Rejected [`JevClientConfig`] at construction time, before any network
/// activity — never carries the API key.
#[derive(Debug, thiserror::Error)]
pub enum JevClientConfigError {
    #[error("jev endpoint must be an https:// URL")]
    InvalidEndpoint,
    #[error("jev api key is not configured")]
    MissingApiKey,
    #[error("failed to construct the jev http client")]
    ClientBuildFailed,
}

/// Typed result of one wire call, before status/body interpretation.
/// Deliberately does not carry any reqwest types so the transport boundary
/// stays mockable with plain data in tests.
struct JevHttpResponse {
    status: u16,
    body: Vec<u8>,
}

/// Injectable transport seam: [`JevClient::is_question`] talks only to this
/// trait, so unit tests can exercise the parsing/error-mapping logic with a
/// mock implementation instead of a real network call — the same pattern
/// `ai-chatbot/src/provider.rs` uses for its injectable `execute_http`
/// closure, expressed as a trait object here because the real
/// implementation needs to own a `reqwest::Client` and configuration across
/// calls.
#[async_trait]
trait JevTransport: Send + Sync {
    async fn send(&self, request_body: Value) -> Result<JevHttpResponse, JudgeError>;
}

/// Real transport: HTTPS-only, no redirects followed, bounded timeout,
/// bounded response size. Mirrors the safety properties of the `ai-chatbot`
/// extension's `OutboundHttpRequest` capability and the plain-Rust
/// `link_preview_resolver` fetch path (same crate, same `reqwest` usage
/// pattern), adapted for a single trusted configured endpoint rather than
/// arbitrary user-supplied URLs.
struct ReqwestJevTransport {
    client: Client,
    endpoint: Url,
    api_key: String,
    max_response_bytes: usize,
}

impl ReqwestJevTransport {
    fn new(config: &JevClientConfig) -> Result<Self, JevClientConfigError> {
        if config.api_key.is_empty() {
            return Err(JevClientConfigError::MissingApiKey);
        }
        let endpoint =
            Url::parse(&config.endpoint).map_err(|_| JevClientConfigError::InvalidEndpoint)?;
        if endpoint.scheme() != "https" {
            return Err(JevClientConfigError::InvalidEndpoint);
        }
        let client = Client::builder()
            .timeout(config.request_timeout)
            .connect_timeout(config.request_timeout)
            .redirect(redirect::Policy::none())
            .https_only(true)
            .build()
            .map_err(|_| JevClientConfigError::ClientBuildFailed)?;
        Ok(Self {
            client,
            endpoint,
            api_key: config.api_key.clone(),
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        })
    }
}

#[cfg(test)]
impl ReqwestJevTransport {
    /// Test-only: builds a transport against a local (`http://`) mock
    /// server, bypassing the HTTPS-only check `new` enforces for real
    /// deployments — `Client::https_only(true)` would otherwise refuse the
    /// loopback connection outright. Real callers only ever reach
    /// `JevClient::new`, which never allows this; this exists so the
    /// *real* transport (`send`'s request building, header handling,
    /// status/size handling) gets exercised against real HTTP, not only
    /// against the [`MockTransport`] used by the parsing/error-mapping
    /// tests below.
    fn new_for_test(endpoint: &str, api_key: &str, max_response_bytes: usize) -> Self {
        let endpoint = Url::parse(endpoint).expect("test endpoint must parse");
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(5))
            .redirect(redirect::Policy::none())
            .build()
            .expect("test http client must build");
        Self {
            client,
            endpoint,
            api_key: api_key.to_string(),
            max_response_bytes,
        }
    }
}

#[async_trait]
impl JevTransport for ReqwestJevTransport {
    async fn send(&self, request_body: Value) -> Result<JevHttpResponse, JudgeError> {
        let response = self
            .client
            .post(self.endpoint.clone())
            .bearer_auth(&self.api_key)
            .json(&request_body)
            .send()
            .await
            .map_err(|error| {
                // `reqwest::Error`'s `Display` does not include header
                // values (the bearer token lives in a header, not the URL
                // or error kind), so this cannot leak the API key. Still
                // avoid `{:?}` here, which is more likely to grow secret
                // exposure over time as reqwest's internals change.
                JudgeError::Transport(format!("jev request failed: {error}"))
            })?;

        let status = response.status();
        if let Some(len) = response.content_length() {
            if len > self.max_response_bytes as u64 {
                return Err(JudgeError::Transport(format!(
                    "jev response exceeded {} byte cap",
                    self.max_response_bytes
                )));
            }
        }
        let mut response = response;
        let mut body = Vec::with_capacity(self.max_response_bytes.min(8 * 1024));
        loop {
            let chunk = response.chunk().await.map_err(|error| {
                JudgeError::Transport(format!("jev response read failed: {error}"))
            })?;
            let Some(chunk) = chunk else {
                break;
            };
            if body.len() + chunk.len() > self.max_response_bytes {
                return Err(JudgeError::Transport(format!(
                    "jev response exceeded {} byte cap",
                    self.max_response_bytes
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(JevHttpResponse {
            status: status.as_u16(),
            body,
        })
    }
}

/// HTTP client for the Jev decision-model service. Implements
/// [`MessageJudge`] so the outbox drain worker depends only on the trait,
/// never on anything in this module.
///
/// See the module docs for how unverified the wire format is.
pub struct JevClient {
    model: String,
    transport: Box<dyn JevTransport>,
}

impl fmt::Debug for JevClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Deliberately omit `transport`: it owns the API key and has no
        // `Debug` impl of its own, so there is nothing to accidentally
        // print here even if a field were added later without updating
        // this impl.
        f.debug_struct("JevClient")
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl JevClient {
    /// Builds a client from configuration, validating the endpoint is an
    /// `https://` URL and that an API key is present. Does not perform any
    /// network I/O.
    pub fn new(config: JevClientConfig) -> Result<Self, JevClientConfigError> {
        let transport = ReqwestJevTransport::new(&config)?;
        Ok(Self {
            model: config.model,
            transport: Box::new(transport),
        })
    }

    #[cfg(test)]
    fn with_transport(model: impl Into<String>, transport: impl JevTransport + 'static) -> Self {
        Self {
            model: model.into(),
            transport: Box::new(transport),
        }
    }
}

#[async_trait]
impl MessageJudge for JevClient {
    async fn judge(&self, body: &str) -> Result<JudgmentBatch, JudgeError> {
        let request_body = build_request_body(&self.model, body);
        let response = self.transport.send(request_body).await?;
        if !(200..300).contains(&response.status) {
            return Err(JudgeError::Transport(transport_error_message(
                response.status,
                &response.body,
            )));
        }
        let decoded =
            serde_json::from_slice::<DecisionsResponse>(&response.body).map_err(|error| {
                JudgeError::InvalidResponse(format!(
                    "jev response did not match the documented Decisions API shape: {error}"
                ))
            })?;
        parse_response(decoded, &self.model)
    }
}

/// Non-secret-leaking message for a non-2xx Jev response: status code plus
/// either the documented `{"error":{"message":...}}` text, or (if the body
/// doesn't match that shape) a short, control-character-stripped snippet of
/// the raw body. The body is never the API key (that only ever appears in
/// the outbound `Authorization` header this client sends, never in what Jev
/// sends back), but is still capped and sanitized as defense in depth
/// against a misbehaving endpoint.
fn transport_error_message(status: u16, body: &[u8]) -> String {
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

/// Extracts `error.message` per the Decisions API's documented error shape
/// (`{"error": {"code": <status>, "message": "..."}}`, confirmed for every
/// non-2xx status the docs enumerate). `None` if the body doesn't match —
/// callers fall back to a raw snippet rather than failing.
fn documented_error_message(body: &[u8]) -> Option<String> {
    let document = serde_json::from_slice::<Value>(body).ok()?;
    document
        .pointer("/error/message")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// One question's shape within a Decisions API request's `questions`
/// object, confirmed from OpenRouter's Decisions API reference (see module
/// docs).
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

/// Pure, separately testable request-body builder — analogous to
/// `provider_request_json_from_parts` in `ai-chatbot/src/provider.rs`. Kept
/// free of any I/O so its exact shape can be exercised with plain
/// assertions and adjusted without touching the transport. Asks every
/// question in [`JUDGMENT_QUESTIONS`] against the same `state` in one
/// request.
fn build_request_body(model: &str, body: &str) -> Value {
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
            spec.judgment_name.as_str().to_string(),
            serde_json::to_value(question).expect("NoulQuestion always serializes"),
        );
    }
    serde_json::json!({
        "model": model,
        "questions": Value::Object(questions),
        "state": body,
    })
}

/// Response shape confirmed from OpenRouter's Decisions API reference (see
/// module docs). `answers` is a dynamic map (rather than one field per
/// judgment) since [`JUDGMENT_QUESTIONS`] can grow without a matching
/// struct-field change here. Deliberately does not derive
/// `deny_unknown_fields`: fields this client doesn't need (`id`,
/// `provider`, per-answer `type`, `usage.input_tokens`/`output_tokens`) are
/// ignored rather than rejected, so an additive change on Jev's side
/// doesn't break this client.
#[derive(Deserialize)]
struct DecisionsResponse {
    answers: HashMap<String, NoulAnswer>,
    /// The actual model snapshot that answered (e.g.
    /// `"typesafe/jev-1.13-20260917"`), distinct from the requested `model`
    /// (e.g. `"typesafe/jev-1.13"`, no date suffix).
    model: String,
    usage: DecisionsUsage,
}

#[derive(Deserialize)]
struct NoulAnswer {
    /// Probability, in `0.0..=1.0`, that the Noul condition holds. No
    /// `confidence` field exists for this primitive — see module docs.
    noul: f64,
}

#[derive(Deserialize)]
struct DecisionsUsage {
    /// USD cost of this request, per the Decisions API's documented
    /// `usage.cost` field.
    cost: f64,
}

/// Pure, separately testable response parser — analogous to
/// `parse_provider_answer_from_document` in `ai-chatbot/src/provider.rs`.
/// Never panics on an out-of-range or missing value: either becomes a
/// typed [`JudgeError::InvalidResponse`] for the *whole* batch, never a
/// partially-stored result — a response missing one expected judgment is
/// exactly as untrustworthy as one missing all of them.
///
/// `fallback_model_version` is the configured model identifier, used only
/// if the response's own `model` field is empty (not expected per the
/// documented shape, but cheaper to guard than to trust blindly).
fn parse_response(
    response: DecisionsResponse,
    fallback_model_version: &str,
) -> Result<JudgmentBatch, JudgeError> {
    let mut judgments = Vec::with_capacity(JUDGMENT_QUESTIONS.len());
    for spec in JUDGMENT_QUESTIONS {
        let answer = response
            .answers
            .get(spec.judgment_name.as_str())
            .ok_or_else(|| {
                JudgeError::InvalidResponse(format!(
                    "jev response did not include an answer for \"{}\"",
                    spec.judgment_name.as_str()
                ))
            })?;
        let probability = answer.noul;
        if !(0.0..=1.0).contains(&probability) {
            return Err(JudgeError::InvalidResponse(format!(
                "jev \"{}\" probability {probability} was outside 0.0..=1.0",
                spec.judgment_name.as_str()
            )));
        }
        judgments.push(NamedJudgment {
            judgment_name: spec.judgment_name,
            probability,
            taxonomy_version: spec.taxonomy_version.to_string(),
        });
    }

    let cost_usd = response.usage.cost;
    if !cost_usd.is_finite() || cost_usd < 0.0 {
        return Err(JudgeError::InvalidResponse(format!(
            "jev usage.cost {cost_usd} was not a finite non-negative value"
        )));
    }
    let model_version = if response.model.is_empty() {
        fallback_model_version.to_string()
    } else {
        response.model
    };

    Ok(JudgmentBatch {
        judgments,
        model_version,
        cost_usd,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct MockTransport<F>
    where
        F: Fn(&Value) -> Result<JevHttpResponse, JudgeError> + Send + Sync,
    {
        respond: F,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl<F> JevTransport for MockTransport<F>
    where
        F: Fn(&Value) -> Result<JevHttpResponse, JudgeError> + Send + Sync,
    {
        async fn send(&self, request_body: Value) -> Result<JevHttpResponse, JudgeError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            (self.respond)(&request_body)
        }
    }

    fn client_with<F>(respond: F) -> JevClient
    where
        F: Fn(&Value) -> Result<JevHttpResponse, JudgeError> + Send + Sync + 'static,
    {
        JevClient::with_transport(
            "jev-test-model",
            MockTransport {
                respond,
                calls: Arc::new(AtomicUsize::new(0)),
            },
        )
    }

    fn ok_response(body: Value) -> JevHttpResponse {
        JevHttpResponse {
            status: 200,
            body: serde_json::to_vec(&body).expect("test JSON always serializes"),
        }
    }

    /// A complete, well-formed `answers` object covering every judgment in
    /// [`JUDGMENT_QUESTIONS`], defaulting every probability to `0.1` except
    /// the overrides given. Keeps individual tests from having to spell out
    /// all six judgments just to exercise one of them.
    fn full_answers(overrides: &[(JudgmentKind, f64)]) -> Value {
        let mut answers = serde_json::Map::new();
        for spec in JUDGMENT_QUESTIONS {
            let probability = overrides
                .iter()
                .find(|(name, _)| *name == spec.judgment_name)
                .map(|(_, probability)| *probability)
                .unwrap_or(0.1);
            answers.insert(
                spec.judgment_name.as_str().to_string(),
                serde_json::json!({ "noul": probability, "type": "noul" }),
            );
        }
        Value::Object(answers)
    }

    fn full_response(overrides: &[(JudgmentKind, f64)], model: &str, cost: f64) -> Value {
        serde_json::json!({
            "answers": full_answers(overrides),
            "id": "gen-dec-1789738314-X5e5eKGQdvR9rblyX250",
            "model": model,
            "provider": "TypeSafe",
            "usage": { "cost": cost, "input_tokens": 476, "output_tokens": 70 }
        })
    }

    fn judgment(batch: &JudgmentBatch, judgment_name: JudgmentKind) -> f64 {
        batch
            .judgments
            .iter()
            .find(|j| j.judgment_name == judgment_name)
            .unwrap_or_else(|| panic!("no judgment named {} in batch", judgment_name.as_str()))
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
            let question = questions.get(spec.judgment_name.as_str()).unwrap_or_else(|| {
                panic!("missing question for {}", spec.judgment_name.as_str())
            });
            assert_eq!(question.get("type").expect("type field"), "noul");
            assert_eq!(
                question.get("instructions").expect("instructions field"),
                spec.instructions
            );
            let criteria = question.get("criteria").expect("criteria");
            assert_eq!(
                criteria.get("true").expect("criteria.true field"),
                spec.criteria_true
            );
            assert_eq!(
                criteria.get("false").expect("criteria.false field"),
                spec.criteria_false
            );
        }
    }

    #[tokio::test]
    async fn judge_parses_every_judgment_from_one_documented_response() {
        let client = client_with(|_request| {
            Ok(ok_response(full_response(
                &[
                    (JudgmentKind::IsQuestion, 0.92),
                    (JudgmentKind::SafetyHateSpeech, 0.03),
                    (JudgmentKind::SafetyExplicit, 0.01),
                    (JudgmentKind::SafetyHarassment, 0.02),
                    (JudgmentKind::SafetyViolence, 0.0),
                    (JudgmentKind::SafetySelfHarm, 0.0),
                ],
                "typesafe/jev-1.13-20260917",
                0.000019992,
            )))
        });

        let batch = client
            .judge("are we there yet?")
            .await
            .expect("well-formed response should parse");

        assert_eq!(batch.judgments.len(), JUDGMENT_QUESTIONS.len());
        assert_eq!(judgment(&batch, JudgmentKind::IsQuestion), 0.92);
        assert_eq!(judgment(&batch, JudgmentKind::SafetyHateSpeech), 0.03);
        assert_eq!(judgment(&batch, JudgmentKind::SafetyExplicit), 0.01);
        assert_eq!(batch.model_version, "typesafe/jev-1.13-20260917");
        assert_eq!(batch.cost_usd, 0.000019992);
        // Every judgment carries its own taxonomy_version, independent of
        // the others, even though all six came from one call.
        let hate_speech_taxonomy = batch
            .judgments
            .iter()
            .find(|j| j.judgment_name == JudgmentKind::SafetyHateSpeech)
            .expect("hate speech judgment")
            .taxonomy_version
            .clone();
        assert_eq!(hate_speech_taxonomy, "safety-hate-speech-v1");
    }

    #[tokio::test]
    async fn judge_falls_back_to_configured_model_when_response_omits_it() {
        let client = client_with(|_request| Ok(ok_response(full_response(&[], "", 0.0))));

        let batch = client
            .judge("hello there")
            .await
            .expect("empty model field should still parse, using the fallback");

        assert_eq!(batch.model_version, "jev-test-model");
    }

    #[tokio::test]
    async fn judge_rejects_malformed_json_shape() {
        let client = client_with(|_request| {
            Ok(ok_response(serde_json::json!({
                "unexpected": "shape entirely"
            })))
        });

        let error = client
            .judge("does this even parse")
            .await
            .expect_err("a response missing the documented fields must be rejected");

        assert!(matches!(error, JudgeError::InvalidResponse(_)));
    }

    #[tokio::test]
    async fn judge_rejects_response_missing_one_judgment() {
        // Only is_question answered; every safety category is missing. The
        // whole batch must fail rather than storing a partial result.
        let client = client_with(|_request| {
            Ok(ok_response(serde_json::json!({
                "answers": { (JudgmentKind::IsQuestion.as_str()): { "noul": 0.5, "type": "noul" } },
                "model": "typesafe/jev-1.13-20260917",
                "usage": { "cost": 0.0, "input_tokens": 1, "output_tokens": 1 }
            })))
        });

        let error = client
            .judge("body")
            .await
            .expect_err("a response missing an expected judgment must be rejected entirely");

        assert!(matches!(error, JudgeError::InvalidResponse(_)));
    }

    #[tokio::test]
    async fn judge_rejects_probability_out_of_range() {
        let client = client_with(|_request| {
            Ok(ok_response(full_response(
                &[(JudgmentKind::SafetyViolence, 1.5)],
                "typesafe/jev-1.13-20260917",
                0.0,
            )))
        });

        let error = client
            .judge("body")
            .await
            .expect_err("out-of-range probability must be rejected");

        assert!(matches!(error, JudgeError::InvalidResponse(_)));
    }

    #[tokio::test]
    async fn judge_rejects_negative_cost() {
        let client = client_with(|_request| {
            Ok(ok_response(full_response(
                &[],
                "typesafe/jev-1.13-20260917",
                -0.01,
            )))
        });

        let error = client
            .judge("body")
            .await
            .expect_err("a negative usage.cost must be rejected, not silently stored");

        assert!(matches!(error, JudgeError::InvalidResponse(_)));
    }

    #[tokio::test]
    async fn judge_rejects_non_json_body() {
        let client = client_with(|_request| {
            Ok(JevHttpResponse {
                status: 200,
                body: b"not json at all".to_vec(),
            })
        });

        let error = client
            .judge("body")
            .await
            .expect_err("non-JSON body must be rejected, not panic");

        assert!(matches!(error, JudgeError::InvalidResponse(_)));
    }

    #[tokio::test]
    async fn judge_maps_non_2xx_status_using_documented_error_shape() {
        let client = client_with(|_request| {
            Ok(JevHttpResponse {
                status: 503,
                body: br#"{"error":{"code":503,"message":"Service temporarily unavailable"}}"#
                    .to_vec(),
            })
        });

        let error = client
            .judge("body")
            .await
            .expect_err("non-2xx status must be a transport error");

        match error {
            JudgeError::Transport(message) => {
                assert!(message.contains("503"));
                assert!(message.contains("Service temporarily unavailable"));
            }
            other => panic!("expected Transport error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn judge_maps_transport_failure_to_transport_error() {
        let client = client_with(|_request| {
            Err(JudgeError::Transport(
                "jev request failed: operation timed out".to_string(),
            ))
        });

        let error = client
            .judge("body")
            .await
            .expect_err("connection/timeout failures must surface as Transport");

        assert!(matches!(error, JudgeError::Transport(_)));
    }

    #[test]
    fn config_debug_output_redacts_api_key() {
        let config = JevClientConfig {
            endpoint: DEFAULT_ENDPOINT.to_string(),
            model: DEFAULT_MODEL.to_string(),
            api_key: "super-secret-value".to_string(),
            request_timeout: Duration::from_secs(3),
        };
        let debug_output = format!("{config:?}");
        assert!(!debug_output.contains("super-secret-value"));
        assert!(debug_output.contains("<redacted>"));
    }

    #[test]
    fn new_rejects_non_https_endpoint() {
        let config = JevClientConfig {
            endpoint: "http://openrouter.ai/api/alpha/decisions".to_string(),
            model: DEFAULT_MODEL.to_string(),
            api_key: "some-key".to_string(),
            request_timeout: Duration::from_secs(3),
        };
        let error = JevClient::new(config).expect_err("non-https endpoint must be rejected");
        assert!(matches!(error, JevClientConfigError::InvalidEndpoint));
    }

    #[test]
    fn new_rejects_missing_api_key() {
        let config = JevClientConfig {
            endpoint: DEFAULT_ENDPOINT.to_string(),
            model: DEFAULT_MODEL.to_string(),
            api_key: String::new(),
            request_timeout: Duration::from_secs(3),
        };
        let error = JevClient::new(config).expect_err("empty api key must be rejected");
        assert!(matches!(error, JevClientConfigError::MissingApiKey));
    }

    #[test]
    fn default_config_has_real_endpoint_and_model_but_no_api_key() {
        let config = JevClientConfig::default();
        assert_eq!(config.endpoint, DEFAULT_ENDPOINT);
        assert_eq!(config.model, DEFAULT_MODEL);
        assert!(config.api_key.is_empty());
        let error = JevClient::new(config)
            .expect_err("default config must not silently be usable without an api key");
        assert!(matches!(error, JevClientConfigError::MissingApiKey));
    }

    // The tests above exercise `build_request_body`/`parse_response` and
    // `JevClient::judge`'s error-mapping logic entirely through
    // `MockTransport`, which bypasses `ReqwestJevTransport` completely.
    // These tests instead run the real transport (request building, the
    // bearer-auth header, status handling, and the response byte cap)
    // against a real HTTP server, so a regression in `ReqwestJevTransport`
    // itself — e.g. dropping `redirect::Policy::none()`, or "simplifying"
    // the streaming byte-cap loop into an unbounded `.bytes().await` — has
    // a test to fail. Mirrors the pattern already used for the same
    // safety properties in `link_preview_resolver.rs`.
    mod real_transport {
        use super::*;
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn client_against(server: &MockServer, max_response_bytes: usize) -> JevClient {
            JevClient::with_transport(
                "jev-test-model",
                ReqwestJevTransport::new_for_test(
                    &server.uri(),
                    "test-api-key",
                    max_response_bytes,
                ),
            )
        }

        #[tokio::test]
        async fn real_transport_round_trips_a_successful_response() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(header("authorization", "Bearer test-api-key"))
                .respond_with(ResponseTemplate::new(200).set_body_json(full_response(
                    &[(JudgmentKind::SafetyHateSpeech, 0.9)],
                    "typesafe/jev-1.13-20260917",
                    0.00002,
                )))
                .mount(&server)
                .await;

            let client = client_against(&server, DEFAULT_MAX_RESPONSE_BYTES);
            let batch = client
                .judge("are we there yet?")
                .await
                .expect("well-formed response over real HTTP must parse");

            assert_eq!(judgment(&batch, JudgmentKind::SafetyHateSpeech), 0.9);
            assert_eq!(batch.model_version, "typesafe/jev-1.13-20260917");
            assert_eq!(batch.cost_usd, 0.00002);
        }

        #[tokio::test]
        async fn real_transport_maps_non_2xx_status_to_transport_error() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/"))
                .respond_with(ResponseTemplate::new(503).set_body_json(serde_json::json!({
                    "error": {
                        "code": 503,
                        "message": "Service temporarily unavailable"
                    }
                })))
                .mount(&server)
                .await;

            let client = client_against(&server, DEFAULT_MAX_RESPONSE_BYTES);
            let error = client
                .judge("body")
                .await
                .expect_err("non-2xx status over real HTTP must be a transport error");

            match error {
                JudgeError::Transport(message) => {
                    assert!(message.contains("503"));
                    assert!(message.contains("Service temporarily unavailable"));
                }
                other => panic!("expected Transport error, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn real_transport_does_not_follow_redirects() {
            let server = MockServer::start().await;
            // No mock is registered for the redirect target: if the
            // transport ever started following redirects again, this
            // request would fail with a 404 from wiremock's default
            // "no matching mock" response instead of surfacing the 302
            // itself, making a silent regression here detectable.
            Mock::given(method("POST"))
                .and(path("/"))
                .respond_with(
                    ResponseTemplate::new(302).insert_header("location", "/redirect-target"),
                )
                .mount(&server)
                .await;

            let client = client_against(&server, DEFAULT_MAX_RESPONSE_BYTES);
            let error = client
                .judge("body")
                .await
                .expect_err("a 3xx must not be silently followed and swallowed");

            match error {
                JudgeError::Transport(message) => assert!(
                    message.contains("302"),
                    "expected the 302 itself to surface, got: {message}"
                ),
                other => panic!("expected Transport error carrying the 302, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn real_transport_enforces_response_size_cap() {
            let server = MockServer::start().await;
            let oversized_body = "a".repeat(256);
            Mock::given(method("POST"))
                .and(path("/"))
                .respond_with(ResponseTemplate::new(200).set_body_string(oversized_body))
                .mount(&server)
                .await;

            // Cap smaller than the response body: must be rejected rather
            // than buffered in full, whether or not the server advertised
            // an accurate Content-Length.
            let client = client_against(&server, 16);
            let error = client
                .judge("body")
                .await
                .expect_err("oversized response over real HTTP must be rejected");

            match error {
                JudgeError::Transport(message) => {
                    assert!(message.contains("byte cap"), "got: {message}")
                }
                other => panic!("expected Transport error, got {other:?}"),
            }
        }
    }
}
