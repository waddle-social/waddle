//! HTTP client for Jev, a decision-model service from a vendor called
//! TypeSafe AI (publicly launched 2026-09-15), used to answer the
//! `is_question` judgment defined by [`super::judge::MessageJudge`].
//!
//! # The wire format below is an unverified best guess
//!
//! At the time this module was written there was no official Jev/TypeSafe
//! API documentation and no test credentials available. Everything about
//! the request/response JSON shape came from third-party sources (an
//! OpenRouter community doc page, TypeSafe's own launch blog post, and a
//! community-written gist) that disagree with each other on details such as
//! the exact endpoint path. **Do not treat [`build_request_body`] or
//! [`parse_response`] as ground truth.** They are deliberately kept as small,
//! separately testable pure functions so that when this is checked against a
//! real Jev account, only this file (and its tests) needs to change — no
//! caller outside this module knows or cares about the wire shape, because
//! everything is mediated through the [`super::judge::MessageJudge`] trait.
//!
//! Candidate endpoints seen in third-party sources (none verified):
//! `https://openrouter.ai/api/alpha/decisions`,
//! `https://openrouter.ai/api/v1/systemone`,
//! `https://api.typesafe.ai/v1/systemone`. This is exactly why
//! [`JevClientConfig::endpoint`] is a required, non-defaulted config value
//! rather than a hardcoded constant: a wrong guess here is a config change,
//! not a code change.
//!
//! This client MUST NOT be enabled against production traffic until someone
//! with real Jev/TypeSafe API access has verified the request/response shape
//! against the actual service.

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::{redirect, Client, Url};
use serde_json::Value;

use super::judge::{IsQuestionJudgment, JudgeError, MessageJudge};

/// Default model identifier sent to Jev when the caller does not override
/// it. Unverified — Jev may use a different alias scheme entirely.
const DEFAULT_MODEL: &str = "jev-latest";

/// This is meant to be a fast, synchronous-feeling judgment call (that is
/// the whole premise of the feature), so the timeout is short.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(4);

/// Response bodies are expected to be a small typed-answer JSON document,
/// not generated text, so this cap is generous relative to that but still
/// bounds a misbehaving or compromised endpoint.
const DEFAULT_MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// Best-guess prompt sent for the yes/no ("Noul", per one source) primitive
/// backing `is_question`.
const IS_QUESTION_PROMPT: &str =
    "Is this message asking a question that expects an answer from someone else in the conversation?";

/// Configuration for a [`JevClient`].
///
/// `endpoint` and `api_key` have no meaningful default — they must come from
/// real deployment configuration/secrets, never a hardcoded guess. `Default`
/// exists only so callers can start from a template and override fields; a
/// `JevClient` built from `JevClientConfig::default()` unmodified always
/// fails validation in [`JevClient::new`], because an empty `api_key` and
/// empty `endpoint` mean "unconfigured", not "use some fallback".
#[derive(Clone)]
pub struct JevClientConfig {
    /// Base HTTPS URL for the Jev decision endpoint. Required; must come
    /// from real config. See the module docs for why the exact path is not
    /// hardcoded.
    pub endpoint: String,
    /// Model identifier passed in every request body.
    pub model: String,
    /// Jev API key. Never logged, never included in `Display`/`Debug`
    /// output, never included in an error message. Empty means
    /// "unconfigured".
    pub api_key: String,
    /// Per-request timeout (connect + total). A few seconds — this call is
    /// meant to be fast.
    pub request_timeout: Duration,
}

impl Default for JevClientConfig {
    fn default() -> Self {
        Self {
            // Empty means "unconfigured": there is no safe default endpoint
            // to guess at (see module docs on candidate URLs disagreeing).
            endpoint: String::new(),
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
    async fn is_question(&self, body: &str) -> Result<IsQuestionJudgment, JudgeError> {
        let request_body = build_request_body(&self.model, body);
        let response = self.transport.send(request_body).await?;
        if !(200..300).contains(&response.status) {
            return Err(JudgeError::Transport(transport_error_message(
                response.status,
                &response.body,
            )));
        }
        let document = serde_json::from_slice::<Value>(&response.body).map_err(|error| {
            JudgeError::InvalidResponse(format!("jev response was not valid JSON: {error}"))
        })?;
        parse_response(&document, &self.model)
    }
}

/// Non-secret-leaking message for a non-2xx Jev response: status code plus a
/// short, control-character-stripped snippet of the body. The body is never
/// the API key (that only ever appears in the outbound `Authorization`
/// header this client sends, never in what Jev sends back), but is still
/// capped and sanitized as defense in depth against a misbehaving endpoint.
fn transport_error_message(status: u16, body: &[u8]) -> String {
    const MAX_SNIPPET_BYTES: usize = 256;
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

/// Pure, separately testable request-body builder — analogous to
/// `provider_request_json_from_parts` in `ai-chatbot/src/provider.rs`. Kept
/// free of any I/O so its exact shape can be exercised with plain
/// assertions and adjusted without touching the transport.
///
/// Best-guess shape (unverified, see module docs): a single named question
/// using the yes/no primitive, alongside the free-text `state` the question
/// is asked about.
fn build_request_body(model: &str, body: &str) -> Value {
    serde_json::json!({
        "model": model,
        "state": body,
        "questions": {
            "is_question": {
                "type": "yes_no",
                "prompt": IS_QUESTION_PROMPT,
            }
        }
    })
}

/// Plausible JSON pointers (tried in order) for each field of the
/// `is_question` answer. Multiple shapes are tried because three
/// third-party sources disagreed on naming, and none were an official spec.
/// This is the one place raw JSON navigation is acceptable per this
/// repository's typed-payloads rule — the *result* is always the typed
/// [`IsQuestionJudgment`], and failure is always the typed [`JudgeError`].
const PROBABILITY_POINTERS: &[&str] = &[
    "/answers/is_question/probability",
    "/is_question/probability",
    "/answers/is_question/yes",
    "/questions/is_question/probability",
    "/probability",
];

const CONFIDENCE_POINTERS: &[&str] = &[
    "/answers/is_question/confidence",
    "/is_question/confidence",
    "/answers/is_question/certainty",
    "/questions/is_question/confidence",
    "/confidence",
];

const TAXONOMY_VERSION_POINTERS: &[&str] = &[
    "/answers/is_question/taxonomy_version",
    "/is_question/taxonomy_version",
    "/taxonomy_version",
    "/taxonomyVersion",
];

const MODEL_VERSION_POINTERS: &[&str] = &[
    "/answers/is_question/model_version",
    "/is_question/model_version",
    "/model_version",
    "/modelVersion",
    "/model",
];

fn first_f64_at(document: &Value, pointers: &[&str]) -> Option<f64> {
    pointers
        .iter()
        .find_map(|pointer| document.pointer(pointer).and_then(Value::as_f64))
}

fn first_str_at(document: &Value, pointers: &[&str]) -> Option<String> {
    pointers
        .iter()
        .find_map(|pointer| document.pointer(pointer).and_then(Value::as_str))
        .map(str::to_string)
}

/// Pure, separately testable response parser — analogous to
/// `parse_provider_answer_from_document` in `ai-chatbot/src/provider.rs`.
/// Probes several plausible response shapes (see the `*_POINTERS`
/// constants) rather than assuming any single one is correct, and never
/// panics on unexpected shapes: everything not found or out of range
/// becomes a typed [`JudgeError::InvalidResponse`].
///
/// `fallback_model_version` is the configured model identifier, used when
/// the response itself does not report one back. `taxonomy_version` falls
/// back to the literal string `"unknown"` rather than failing the whole
/// judgment, since its absence does not by itself mean the probability is
/// untrustworthy — but it is recorded as literally `"unknown"`, never
/// silently as a real-looking version string, so schema drift stays
/// visible in stored rows exactly as the field's own doc comment intends.
fn parse_response(
    document: &Value,
    fallback_model_version: &str,
) -> Result<IsQuestionJudgment, JudgeError> {
    let probability = first_f64_at(document, PROBABILITY_POINTERS).ok_or_else(|| {
        JudgeError::InvalidResponse(
            "jev response did not contain a recognizable is_question probability".to_string(),
        )
    })?;
    if !(0.0..=1.0).contains(&probability) {
        return Err(JudgeError::InvalidResponse(format!(
            "jev is_question probability {probability} was outside 0.0..=1.0"
        )));
    }
    let confidence = first_f64_at(document, CONFIDENCE_POINTERS).ok_or_else(|| {
        JudgeError::InvalidResponse(
            "jev response did not contain a recognizable is_question confidence".to_string(),
        )
    })?;
    if !(0.0..=1.0).contains(&confidence) {
        return Err(JudgeError::InvalidResponse(format!(
            "jev is_question confidence {confidence} was outside 0.0..=1.0"
        )));
    }
    let taxonomy_version =
        first_str_at(document, TAXONOMY_VERSION_POINTERS).unwrap_or_else(|| "unknown".to_string());
    let model_version = first_str_at(document, MODEL_VERSION_POINTERS)
        .unwrap_or_else(|| fallback_model_version.to_string());

    Ok(IsQuestionJudgment {
        probability,
        confidence,
        taxonomy_version,
        model_version,
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

    #[test]
    fn build_request_body_matches_expected_shape() {
        let body = build_request_body("jev-latest", "are we there yet?");
        assert_eq!(
            body,
            serde_json::json!({
                "model": "jev-latest",
                "state": "are we there yet?",
                "questions": {
                    "is_question": {
                        "type": "yes_no",
                        "prompt": IS_QUESTION_PROMPT,
                    }
                }
            })
        );
    }

    #[tokio::test]
    async fn is_question_parses_successful_primary_shape_response() {
        let client = client_with(|_request| {
            Ok(ok_response(serde_json::json!({
                "answers": {
                    "is_question": {
                        "probability": 0.92,
                        "confidence": 0.81,
                        "taxonomy_version": "tax-2026-09-01",
                        "model_version": "jev-2026-09-15",
                    }
                }
            })))
        });

        let judgment = client
            .is_question("are we there yet?")
            .await
            .expect("well-formed response should parse");

        assert_eq!(
            judgment,
            IsQuestionJudgment {
                probability: 0.92,
                confidence: 0.81,
                taxonomy_version: "tax-2026-09-01".to_string(),
                model_version: "jev-2026-09-15".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn is_question_parses_successful_flat_fallback_shape_response() {
        let client = client_with(|_request| {
            Ok(ok_response(serde_json::json!({
                "is_question": {
                    "probability": 0.1,
                    "confidence": 0.5,
                }
            })))
        });

        let judgment = client
            .is_question("hello there")
            .await
            .expect("flat fallback shape should still parse");

        assert_eq!(judgment.probability, 0.1);
        assert_eq!(judgment.confidence, 0.5);
        // Neither version field was present, so both fall back rather than
        // erroring, per parse_response's documented fallback behavior.
        assert_eq!(judgment.taxonomy_version, "unknown");
        assert_eq!(judgment.model_version, "jev-test-model");
    }

    #[tokio::test]
    async fn is_question_rejects_malformed_json_shape() {
        let client = client_with(|_request| {
            Ok(ok_response(serde_json::json!({
                "unexpected": "shape entirely"
            })))
        });

        let error = client
            .is_question("does this even parse")
            .await
            .expect_err("shape with no recognizable probability must be rejected");

        assert!(matches!(error, JudgeError::InvalidResponse(_)));
    }

    #[tokio::test]
    async fn is_question_rejects_probability_out_of_range() {
        let client = client_with(|_request| {
            Ok(ok_response(serde_json::json!({
                "is_question": { "probability": 1.5, "confidence": 0.5 }
            })))
        });

        let error = client
            .is_question("body")
            .await
            .expect_err("out-of-range probability must be rejected");

        assert!(matches!(error, JudgeError::InvalidResponse(_)));
    }

    #[tokio::test]
    async fn is_question_rejects_non_json_body() {
        let client = client_with(|_request| {
            Ok(JevHttpResponse {
                status: 200,
                body: b"not json at all".to_vec(),
            })
        });

        let error = client
            .is_question("body")
            .await
            .expect_err("non-JSON body must be rejected, not panic");

        assert!(matches!(error, JudgeError::InvalidResponse(_)));
    }

    #[tokio::test]
    async fn is_question_maps_non_2xx_status_to_transport_error() {
        let client = client_with(|_request| {
            Ok(JevHttpResponse {
                status: 503,
                body: b"{\"error\":\"upstream overloaded\"}".to_vec(),
            })
        });

        let error = client
            .is_question("body")
            .await
            .expect_err("non-2xx status must be a transport error");

        match error {
            JudgeError::Transport(message) => {
                assert!(message.contains("503"));
                assert!(message.contains("upstream overloaded"));
            }
            other => panic!("expected Transport error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn is_question_maps_transport_failure_to_transport_error() {
        let client = client_with(|_request| {
            Err(JudgeError::Transport(
                "jev request failed: operation timed out".to_string(),
            ))
        });

        let error = client
            .is_question("body")
            .await
            .expect_err("connection/timeout failures must surface as Transport");

        assert!(matches!(error, JudgeError::Transport(_)));
    }

    #[test]
    fn config_debug_output_redacts_api_key() {
        let config = JevClientConfig {
            endpoint: "https://api.typesafe.ai/v1/systemone".to_string(),
            model: "jev-latest".to_string(),
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
            endpoint: "http://api.typesafe.ai/v1/systemone".to_string(),
            model: "jev-latest".to_string(),
            api_key: "some-key".to_string(),
            request_timeout: Duration::from_secs(3),
        };
        let error = JevClient::new(config).expect_err("non-https endpoint must be rejected");
        assert!(matches!(error, JevClientConfigError::InvalidEndpoint));
    }

    #[test]
    fn new_rejects_missing_api_key() {
        let config = JevClientConfig {
            endpoint: "https://api.typesafe.ai/v1/systemone".to_string(),
            model: "jev-latest".to_string(),
            api_key: String::new(),
            request_timeout: Duration::from_secs(3),
        };
        let error = JevClient::new(config).expect_err("empty api key must be rejected");
        assert!(matches!(error, JevClientConfigError::MissingApiKey));
    }

    #[test]
    fn default_config_is_unconfigured() {
        let config = JevClientConfig::default();
        assert!(config.endpoint.is_empty());
        assert!(config.api_key.is_empty());
        assert_eq!(config.model, DEFAULT_MODEL);
        let error = JevClient::new(config).expect_err("default config must not silently be usable");
        assert!(matches!(
            error,
            JevClientConfigError::MissingApiKey | JevClientConfigError::InvalidEndpoint
        ));
    }
}
