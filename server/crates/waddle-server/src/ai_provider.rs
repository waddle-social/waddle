use serde::{Deserialize, Serialize};
use thiserror::Error;

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
    generate_ai_response_with_config(&reqwest::Client::new(), &config, prompt).await
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
    let without_command = if trimmed.len() >= 3 && trimmed[..3].eq_ignore_ascii_case("/ai") {
        &trimmed[3..]
    } else {
        trimmed
    };
    without_command
        .replace("@waddle", "")
        .replace("@Waddle", "")
        .trim()
        .to_string()
}

pub fn is_ai_prompt_body(body: &str) -> bool {
    let trimmed = body.trim_start();
    let slash = trimmed.len() >= 3
        && trimmed[..3].eq_ignore_ascii_case("/ai")
        && trimmed
            .as_bytes()
            .get(3)
            .is_none_or(|ch| matches!(ch, b' ' | b'\t' | b'\r' | b'\n'));
    slash || body.to_ascii_lowercase().contains("@waddle")
}

#[cfg(test)]
mod tests {
    use super::{
        generate_ai_response_with_config, AiProviderConfig, AiProviderError, AiProviderKind,
    };
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    async fn empty_provider_response_is_unavailable_error() {
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
}
