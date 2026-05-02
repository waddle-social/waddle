use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

const AI_HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const AI_HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const AI_COMMAND: &str = "/ai";
const WADDLE_MENTION: &str = "@waddle";
const OPENAI_BASE_URL: &str = "https://api.openai.com";
const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api";
const OPENROUTER_DEFAULT_MODEL: &str = "openrouter/free";
const OPENROUTER_DEFAULT_TITLE: &str = "Waddle";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiProviderKind {
    OpenAi,
    OpenRouter,
}

#[derive(Debug, Clone)]
pub struct AiProviderConfig {
    pub kind: AiProviderKind,
    pub api_key: String,
    pub model: String,
    pub base_url: String,
    pub http_referer: Option<String>,
    pub app_title: Option<String>,
}

#[derive(Debug, Error)]
pub enum AiProviderError {
    #[error(
        "AI provider unavailable: set WADDLE_AI_PROVIDER=openai with OPENAI_API_KEY and WADDLE_AI_MODEL, or WADDLE_AI_PROVIDER=openrouter with OPENROUTER_API_KEY"
    )]
    Unavailable,
    #[error("unsupported AI provider {0:?}")]
    UnsupportedProvider(String),
    #[error("AI provider request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("AI provider HTTP client initialization failed: {0}")]
    HttpClient(String),
    #[error("AI provider returned no answer")]
    EmptyResponse,
}

impl AiProviderConfig {
    pub fn from_env() -> Result<Self, AiProviderError> {
        let provider =
            std::env::var("WADDLE_AI_PROVIDER").map_err(|_| AiProviderError::Unavailable)?;
        let kind = match provider.as_str() {
            "openai" => AiProviderKind::OpenAi,
            "openrouter" => AiProviderKind::OpenRouter,
            other => return Err(AiProviderError::UnsupportedProvider(other.to_string())),
        };
        let api_key = api_key_from_env(&kind)?;
        let model = model_from_env(&kind)?;
        let base_url = base_url_from_env(&kind);
        Ok(Self {
            kind,
            api_key,
            model,
            base_url,
            http_referer: optional_env("WADDLE_OPENROUTER_REFERER"),
            app_title: optional_env("WADDLE_OPENROUTER_TITLE"),
        })
    }
}

fn api_key_from_env(kind: &AiProviderKind) -> Result<String, AiProviderError> {
    let key = match kind {
        AiProviderKind::OpenAi => optional_env("OPENAI_API_KEY"),
        AiProviderKind::OpenRouter => optional_env("OPENROUTER_API_KEY"),
    }
    .ok_or(AiProviderError::Unavailable)?;
    Ok(key)
}

fn model_from_env(kind: &AiProviderKind) -> Result<String, AiProviderError> {
    match kind {
        AiProviderKind::OpenAi => {
            optional_env("WADDLE_AI_MODEL").ok_or(AiProviderError::Unavailable)
        }
        AiProviderKind::OpenRouter => {
            Ok(optional_env("WADDLE_AI_MODEL")
                .unwrap_or_else(|| OPENROUTER_DEFAULT_MODEL.to_string()))
        }
    }
}

fn base_url_from_env(kind: &AiProviderKind) -> String {
    match kind {
        AiProviderKind::OpenAi => {
            optional_env("WADDLE_OPENAI_BASE_URL").unwrap_or_else(|| OPENAI_BASE_URL.to_string())
        }
        AiProviderKind::OpenRouter => optional_env("WADDLE_OPENROUTER_BASE_URL")
            .unwrap_or_else(|| OPENROUTER_BASE_URL.to_string()),
    }
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[derive(Debug, Serialize)]
struct OpenAiChatRequest<'a> {
    model: &'a str,
    messages: Vec<OpenAiMessage<'a>>,
}

#[derive(Debug, Serialize)]
struct OpenAiMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiResponseMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAiResponseMessage {
    content: String,
}

/// A single archived room message used to provide conversation history to the AI.
#[derive(Debug, Clone)]
pub struct HistoricalMessage {
    pub sender_nick: String,
    pub body: String,
}

pub async fn generate_ai_response(
    prompt: &str,
    history: &[HistoricalMessage],
) -> Result<String, AiProviderError> {
    let config = AiProviderConfig::from_env()?;
    generate_ai_response_with_config(ai_http_client()?, &config, prompt, history).await
}

pub fn is_ai_provider_configured() -> bool {
    AiProviderConfig::from_env().is_ok()
}

pub async fn generate_ai_response_with_config(
    client: &reqwest::Client,
    config: &AiProviderConfig,
    prompt: &str,
    history: &[HistoricalMessage],
) -> Result<String, AiProviderError> {
    match config.kind {
        AiProviderKind::OpenAi | AiProviderKind::OpenRouter => {
            generate_openai_compatible_response(client, config, prompt, history).await
        }
    }
}

fn ai_http_client() -> Result<&'static reqwest::Client, AiProviderError> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(AI_HTTP_CONNECT_TIMEOUT)
                .timeout(AI_HTTP_REQUEST_TIMEOUT)
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|error| AiProviderError::HttpClient(error.clone()))
}

async fn generate_openai_compatible_response(
    client: &reqwest::Client,
    config: &AiProviderConfig,
    prompt: &str,
    history: &[HistoricalMessage],
) -> Result<String, AiProviderError> {
    let url = format!(
        "{}/v1/chat/completions",
        config.base_url.trim_end_matches('/')
    );
    let history_bodies: Vec<String> = history
        .iter()
        .map(|msg| format!("{}: {}", msg.sender_nick, msg.body))
        .collect();
    let history_messages: Vec<OpenAiMessage<'_>> = history_bodies
        .iter()
        .map(|body| OpenAiMessage {
            role: "user",
            content: body.as_str(),
        })
        .collect();
    let mut messages = Vec::with_capacity(1 + history_messages.len() + 1);
    messages.push(OpenAiMessage {
        role: "system",
        content: "You are Waddle's XMPP-native room assistant. Answer concisely.",
    });
    messages.extend(history_messages);
    messages.push(OpenAiMessage {
        role: "user",
        content: prompt,
    });
    let request = OpenAiChatRequest {
        model: config.model.as_str(),
        messages,
    };

    let mut builder = client
        .post(url)
        .bearer_auth(config.api_key.as_str())
        .json(&request);
    if config.kind == AiProviderKind::OpenRouter {
        builder = builder.header(
            "X-OpenRouter-Title",
            config
                .app_title
                .as_deref()
                .unwrap_or(OPENROUTER_DEFAULT_TITLE),
        );
        if let Some(http_referer) = config.http_referer.as_deref() {
            builder = builder.header("HTTP-Referer", http_referer);
        }
    }

    let response = builder
        .send()
        .await?
        .error_for_status()?
        .json::<OpenAiChatResponse>()
        .await?;

    response
        .choices
        .into_iter()
        .find_map(|choice| {
            let content = choice.message.content.trim().to_string();
            (!content.is_empty()).then_some(content)
        })
        .ok_or(AiProviderError::EmptyResponse)
}

pub fn clean_ai_prompt(body: &str) -> String {
    let trimmed = body.trim();
    let without_command = strip_ai_command(trimmed).unwrap_or(trimmed);
    remove_waddle_mentions(without_command).trim().to_string()
}

pub fn is_ai_prompt_body(body: &str) -> bool {
    let trimmed = body.trim_start();
    strip_ai_command(trimmed).is_some() || contains_waddle_mention(body)
}

fn strip_ai_command(trimmed: &str) -> Option<&str> {
    (has_ai_command_prefix(trimmed)
        && is_command_boundary(trimmed.as_bytes().get(AI_COMMAND.len())))
    .then(|| trimmed.get(AI_COMMAND.len()..).unwrap_or(""))
}

fn has_ai_command_prefix(trimmed: &str) -> bool {
    trimmed
        .get(..AI_COMMAND.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(AI_COMMAND))
}

fn remove_waddle_mentions(value: &str) -> String {
    let mut cleaned = String::with_capacity(value.len());
    let mut cursor = 0;
    while cursor < value.len() {
        let remaining = &value[cursor..];
        if remaining
            .get(..WADDLE_MENTION.len())
            .is_some_and(|mention| mention.eq_ignore_ascii_case(WADDLE_MENTION))
            && is_word_boundary(remaining.as_bytes().get(WADDLE_MENTION.len()))
        {
            cursor += WADDLE_MENTION.len();
        } else {
            let Some(ch) = remaining.chars().next() else {
                break;
            };
            cleaned.push(ch);
            cursor += ch.len_utf8();
        }
    }
    cleaned
}

fn contains_waddle_mention(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower
        .match_indices(WADDLE_MENTION)
        .any(|(start, mention)| is_word_boundary(lower.as_bytes().get(start + mention.len())))
}

fn is_word_boundary(next: Option<&u8>) -> bool {
    !matches!(next, Some(b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_'))
}

fn is_command_boundary(next: Option<&u8>) -> bool {
    matches!(next, None | Some(b' ' | b'\t' | b'\r' | b'\n'))
}

#[cfg(test)]
mod tests {
    use super::{
        clean_ai_prompt, generate_ai_response_with_config, is_ai_prompt_body, AiProviderConfig,
        AiProviderError, AiProviderKind, HistoricalMessage,
    };
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    mod shared_ai_prompt_cases {
        include!("../../../test-fixtures/ai_prompt_cases.rs");
    }

    #[tokio::test]
    async fn mocked_openai_response_becomes_answer() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{ "message": { "content": "Rawkode is David Flanagan." } }]
            })))
            .mount(&server)
            .await;

        let answer = generate_ai_response_with_config(
            &reqwest::Client::new(),
            &AiProviderConfig {
                kind: AiProviderKind::OpenAi,
                api_key: "test-key".to_string(),
                model: "gpt-test".to_string(),
                base_url: server.uri(),
                http_referer: None,
                app_title: None,
            },
            "/ai Who is Rawkode?",
            &[],
        )
        .await
        .expect("mocked response");

        assert_eq!(answer, "Rawkode is David Flanagan.");
    }

    #[tokio::test]
    async fn mocked_openrouter_response_uses_openai_compatible_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer router-key"))
            .and(header("x-openrouter-title", "Waddle"))
            .and(header("http-referer", "https://waddle.chat"))
            .and(body_string_contains("openrouter/free"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{ "message": { "content": "Ottawa" } }]
            })))
            .mount(&server)
            .await;

        let answer = generate_ai_response_with_config(
            &reqwest::Client::new(),
            &AiProviderConfig {
                kind: AiProviderKind::OpenRouter,
                api_key: "router-key".to_string(),
                model: "openrouter/free".to_string(),
                base_url: server.uri(),
                http_referer: Some("https://waddle.chat".to_string()),
                app_title: None,
            },
            "/ai what is the capital of Canada?",
            &[],
        )
        .await
        .expect("mocked response");

        assert_eq!(answer, "Ottawa");
    }

    #[tokio::test]
    async fn prompt_is_sent_to_provider() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_string_contains("/ai Who is Rawkode?"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{ "message": { "content": "ok" } }]
            })))
            .mount(&server)
            .await;

        generate_ai_response_with_config(
            &reqwest::Client::new(),
            &AiProviderConfig {
                kind: AiProviderKind::OpenAi,
                api_key: "test-key".to_string(),
                model: "gpt-test".to_string(),
                base_url: server.uri(),
                http_referer: None,
                app_title: None,
            },
            "/ai Who is Rawkode?",
            &[],
        )
        .await
        .expect("provider response");
    }

    #[tokio::test]
    async fn empty_provider_response_is_empty_response_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": []
            })))
            .mount(&server)
            .await;

        let err = generate_ai_response_with_config(
            &reqwest::Client::new(),
            &AiProviderConfig {
                kind: AiProviderKind::OpenAi,
                api_key: "test-key".to_string(),
                model: "gpt-test".to_string(),
                base_url: server.uri(),
                http_referer: None,
                app_title: None,
            },
            "/ai no fake answers",
            &[],
        )
        .await
        .expect_err("empty response");

        assert!(matches!(err, AiProviderError::EmptyResponse));
    }

    #[test]
    fn ai_prompt_helpers_do_not_panic_on_multibyte_prefixes() {
        assert!(!is_ai_prompt_body("☃ /ai later"));
        assert_eq!(clean_ai_prompt("☃ /ai later"), "☃ /ai later");
    }

    #[test]
    fn clean_ai_prompt_strips_command_and_mentions_case_insensitively() {
        assert_eq!(
            clean_ai_prompt("  /AI @WADDLE explain this  "),
            "explain this"
        );
        assert_eq!(clean_ai_prompt("@wAdDlE continue"), "continue");
        assert_eq!(
            clean_ai_prompt("@waddle_bot continue"),
            "@waddle_bot continue"
        );
        assert_eq!(
            clean_ai_prompt("@waddleBot continue"),
            "@waddleBot continue"
        );
        assert_eq!(clean_ai_prompt("/airship @WADDLE"), "/airship");
        assert!(is_ai_prompt_body("@WADDLE continue"));
        assert!(!is_ai_prompt_body("@waddle_bot continue"));
        assert!(!is_ai_prompt_body("@waddleBot continue"));
    }

    #[test]
    fn shared_ai_prompt_cases_match_host_parser() {
        for &(body, is_prompt, cleaned) in shared_ai_prompt_cases::AI_PROMPT_CASES {
            assert_eq!(is_ai_prompt_body(body), is_prompt, "{body}");
            assert_eq!(clean_ai_prompt(body), cleaned, "{body}");
        }
    }

    #[tokio::test]
    async fn room_history_is_included_in_request_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_string_contains("alice: Hello everyone"))
            .and(body_string_contains("bob: How are you"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{ "message": { "content": "I see the history." } }]
            })))
            .mount(&server)
            .await;

        let history = vec![
            HistoricalMessage {
                sender_nick: "alice".to_string(),
                body: "Hello everyone".to_string(),
            },
            HistoricalMessage {
                sender_nick: "bob".to_string(),
                body: "How are you".to_string(),
            },
        ];

        let answer = generate_ai_response_with_config(
            &reqwest::Client::new(),
            &AiProviderConfig {
                kind: AiProviderKind::OpenAi,
                api_key: "test-key".to_string(),
                model: "gpt-test".to_string(),
                base_url: server.uri(),
                http_referer: None,
                app_title: None,
            },
            "/ai summarize",
            &history,
        )
        .await
        .expect("history response");

        assert_eq!(answer, "I see the history.");
    }

    #[tokio::test]
    async fn empty_history_produces_valid_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{ "message": { "content": "ok" } }]
            })))
            .mount(&server)
            .await;

        generate_ai_response_with_config(
            &reqwest::Client::new(),
            &AiProviderConfig {
                kind: AiProviderKind::OpenAi,
                api_key: "test-key".to_string(),
                model: "gpt-test".to_string(),
                base_url: server.uri(),
                http_referer: None,
                app_title: None,
            },
            "/ai hello",
            &[],
        )
        .await
        .expect("empty history request succeeds");
    }
}
