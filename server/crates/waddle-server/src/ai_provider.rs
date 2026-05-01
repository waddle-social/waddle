use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

const AI_HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const AI_HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const AI_COMMAND: &str = "/ai";
const WADDLE_MENTION: &str = "@waddle";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiProviderKind {
    OpenAi,
}

#[derive(Debug, Clone)]
pub struct AiProviderConfig {
    pub kind: AiProviderKind,
    pub api_key: String,
    pub model: String,
    pub base_url: String,
}

#[derive(Debug, Error)]
pub enum AiProviderError {
    #[error("AI provider unavailable: set WADDLE_AI_PROVIDER=openai, OPENAI_API_KEY, and WADDLE_AI_MODEL")]
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
            other => return Err(AiProviderError::UnsupportedProvider(other.to_string())),
        };
        let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| AiProviderError::Unavailable)?;
        let model = std::env::var("WADDLE_AI_MODEL").map_err(|_| AiProviderError::Unavailable)?;
        if api_key.trim().is_empty() || model.trim().is_empty() {
            return Err(AiProviderError::Unavailable);
        }
        Ok(Self {
            kind,
            api_key,
            model,
            base_url: std::env::var("WADDLE_OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com".to_string()),
        })
    }
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

pub async fn generate_ai_response(prompt: &str) -> Result<String, AiProviderError> {
    let config = AiProviderConfig::from_env()?;
    generate_ai_response_with_config(ai_http_client()?, &config, prompt).await
}

pub fn is_ai_provider_configured() -> bool {
    AiProviderConfig::from_env().is_ok()
}

pub async fn generate_ai_response_with_config(
    client: &reqwest::Client,
    config: &AiProviderConfig,
    prompt: &str,
) -> Result<String, AiProviderError> {
    match config.kind {
        AiProviderKind::OpenAi => generate_openai_response(client, config, prompt).await,
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

async fn generate_openai_response(
    client: &reqwest::Client,
    config: &AiProviderConfig,
    prompt: &str,
) -> Result<String, AiProviderError> {
    let url = format!(
        "{}/v1/chat/completions",
        config.base_url.trim_end_matches('/')
    );
    let request = OpenAiChatRequest {
        model: config.model.as_str(),
        messages: vec![
            OpenAiMessage {
                role: "system",
                content: "You are Waddle's XMPP-native room assistant. Answer concisely.",
            },
            OpenAiMessage {
                role: "user",
                content: prompt,
            },
        ],
    };

    let response = client
        .post(url)
        .bearer_auth(config.api_key.as_str())
        .json(&request)
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
        AiProviderError, AiProviderKind,
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
            },
            "/ai Who is Rawkode?",
        )
        .await
        .expect("mocked response");

        assert_eq!(answer, "Rawkode is David Flanagan.");
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
            },
            "/ai Who is Rawkode?",
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
            },
            "/ai no fake answers",
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
}
