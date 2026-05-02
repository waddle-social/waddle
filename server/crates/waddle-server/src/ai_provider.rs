use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
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

/// Maximum number of tool-call rounds before giving up.
const MAX_TOOL_ROUNDS: usize = 5;

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

/// A room message returned by an AI tool call.
#[derive(Debug, Clone)]
pub struct HistoricalMessage {
    pub sender_nick: String,
    pub body: String,
}

/// A tool call request the AI can make to access XMPP-native data.
#[derive(Debug)]
pub enum AiToolRequest {
    /// Search the channel message archive by keyword (XEP-0313 + XEP-0431).
    SearchChannelHistory { query: String, max: Option<u32> },
    /// Fetch the most recent messages from the channel (XEP-0313).
    GetRecentMessages { max: Option<u32> },
    /// A tool call registered by a WASM extension (routed via ExtensionManager).
    Extension {
        name: String,
        arguments: serde_json::Value,
    },
}

/// Result from executing an AI tool call.
#[derive(Debug)]
pub enum AiToolResult {
    Messages(Vec<HistoricalMessage>),
    /// Opaque string content from a WASM extension tool.
    Extension(String),
    Error(String),
}

/// Executes AI tool calls against XMPP-native data sources.
///
/// Implementations connect the AI to live XMPP data (MAM archives, roster,
/// etc.). The server uses [`RoomToolExecutor`] in interpret.rs; tests inject
/// mock executors.
#[async_trait]
pub trait AiToolExecutor: Send + Sync {
    async fn execute(&self, request: AiToolRequest) -> AiToolResult;
}

/// No-op executor when no XMPP data sources are available (e.g. no MAM storage).
pub struct NoopToolExecutor;

#[async_trait]
impl AiToolExecutor for NoopToolExecutor {
    async fn execute(&self, _request: AiToolRequest) -> AiToolResult {
        AiToolResult::Error("Channel history unavailable".to_string())
    }
}

// --- OpenAI-compatible wire types ---

#[derive(Debug, Serialize, Clone)]
struct OpenAiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAiToolCall>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OpenAiToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: OpenAiToolCallFunction,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OpenAiToolCallFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize)]
struct OpenAiChatRequest<'a> {
    model: &'a str,
    messages: &'a [OpenAiMessage],
    tools: &'a [serde_json::Value],
}

#[derive(Debug, Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiResponseMessage {
    content: Option<String>,
    tool_calls: Option<Vec<OpenAiToolCall>>,
}

#[derive(Debug, Deserialize)]
struct SearchChannelHistoryArgs {
    query: String,
    max: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct GetRecentMessagesArgs {
    max: Option<u32>,
}

// --- Tool definitions ---

fn tool_definitions() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "search_channel_history",
                "description": "Search the XMPP channel message archive for messages matching a query (XEP-0313/XEP-0431 full-text search). Use this to find what someone said, look up past discussions, or answer questions about earlier conversations.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Search terms to find in message history"
                        },
                        "max": {
                            "type": "integer",
                            "description": "Maximum number of results to return (default: 10, max: 20)"
                        }
                    },
                    "required": ["query"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "get_recent_messages",
                "description": "Fetch the most recent messages from the XMPP channel (XEP-0313 MAM). Use this to get context about the current conversation.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "max": {
                            "type": "integer",
                            "description": "Number of recent messages to fetch (default: 20, max: 50)"
                        }
                    }
                }
            }
        }),
    ]
}

// --- Public API ---

pub async fn generate_ai_response(
    prompt: &str,
    executor: &dyn AiToolExecutor,
) -> Result<String, AiProviderError> {
    let config = AiProviderConfig::from_env()?;
    generate_ai_response_with_config(ai_http_client()?, &config, prompt, executor).await
}

pub fn is_ai_provider_configured() -> bool {
    AiProviderConfig::from_env().is_ok()
}

pub async fn generate_ai_response_with_config(
    client: &reqwest::Client,
    config: &AiProviderConfig,
    prompt: &str,
    executor: &dyn AiToolExecutor,
) -> Result<String, AiProviderError> {
    match config.kind {
        AiProviderKind::OpenAi | AiProviderKind::OpenRouter => {
            generate_openai_compatible_response(client, config, prompt, executor).await
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
    executor: &dyn AiToolExecutor,
) -> Result<String, AiProviderError> {
    let url = format!(
        "{}/v1/chat/completions",
        config.base_url.trim_end_matches('/')
    );
    let tools = tool_definitions();

    let mut messages = vec![system_message(), user_message(prompt)];

    for _ in 0..MAX_TOOL_ROUNDS {
        let request = OpenAiChatRequest {
            model: config.model.as_str(),
            messages: &messages,
            tools: &tools,
        };

        let mut builder = client
            .post(&url)
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

        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or(AiProviderError::EmptyResponse)?;

        match choice.finish_reason.as_deref() {
            Some("tool_calls") => {
                let tool_calls = choice.message.tool_calls.unwrap_or_default();
                if tool_calls.is_empty() {
                    return Err(AiProviderError::EmptyResponse);
                }
                messages.push(OpenAiMessage {
                    role: "assistant".to_string(),
                    content: choice.message.content,
                    tool_call_id: None,
                    tool_calls: Some(tool_calls.clone()),
                });
                for tool_call in tool_calls {
                    let result = execute_tool_call(executor, &tool_call).await;
                    messages.push(OpenAiMessage {
                        role: "tool".to_string(),
                        content: Some(result),
                        tool_call_id: Some(tool_call.id),
                        tool_calls: None,
                    });
                }
            }
            _ => {
                let content = choice
                    .message
                    .content
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                return if content.is_empty() {
                    Err(AiProviderError::EmptyResponse)
                } else {
                    Ok(content)
                };
            }
        }
    }

    Err(AiProviderError::EmptyResponse)
}

async fn execute_tool_call(executor: &dyn AiToolExecutor, tool_call: &OpenAiToolCall) -> String {
    let request = match parse_tool_request(tool_call) {
        Ok(request) => request,
        Err(reason) => return format!("Error: invalid tool call — {reason}"),
    };
    match executor.execute(request).await {
        AiToolResult::Messages(messages) => format_messages_for_ai(&messages),
        AiToolResult::Extension(content) => content,
        AiToolResult::Error(error) => format!("Error: {error}"),
    }
}

fn parse_tool_request(tool_call: &OpenAiToolCall) -> Result<AiToolRequest, String> {
    match tool_call.function.name.as_str() {
        "search_channel_history" => {
            let args: SearchChannelHistoryArgs =
                serde_json::from_str(&tool_call.function.arguments)
                    .map_err(|e| format!("invalid search_channel_history args: {e}"))?;
            Ok(AiToolRequest::SearchChannelHistory {
                query: args.query,
                max: args.max,
            })
        }
        "get_recent_messages" => {
            let args: GetRecentMessagesArgs = serde_json::from_str(&tool_call.function.arguments)
                .unwrap_or(GetRecentMessagesArgs { max: None });
            Ok(AiToolRequest::GetRecentMessages { max: args.max })
        }
        _ => {
            let arguments = serde_json::from_str(&tool_call.function.arguments)
                .unwrap_or(serde_json::Value::Object(Default::default()));
            Ok(AiToolRequest::Extension {
                name: tool_call.function.name.clone(),
                arguments,
            })
        }
    }
}

fn format_messages_for_ai(messages: &[HistoricalMessage]) -> String {
    if messages.is_empty() {
        return "No messages found.".to_string();
    }
    messages
        .iter()
        .map(|m| format!("{}: {}", m.sender_nick, m.body))
        .collect::<Vec<_>>()
        .join("\n")
}

fn system_message() -> OpenAiMessage {
    OpenAiMessage {
        role: "system".to_string(),
        content: Some(
            "You are Waddle's XMPP-native room assistant. \
             You have access to the channel's message history via tools — \
             use search_channel_history to find specific past messages, \
             and get_recent_messages for recent context. \
             Answer concisely."
                .to_string(),
        ),
        tool_call_id: None,
        tool_calls: None,
    }
}

fn user_message(content: &str) -> OpenAiMessage {
    OpenAiMessage {
        role: "user".to_string(),
        content: Some(content.to_string()),
        tool_call_id: None,
        tool_calls: None,
    }
}

// --- Prompt parsing helpers (shared with the ai-chatbot WASM extension) ---

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
        AiProviderError, AiProviderKind, AiToolExecutor, AiToolRequest, AiToolResult,
        HistoricalMessage, NoopToolExecutor,
    };
    use async_trait::async_trait;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    mod shared_ai_prompt_cases {
        include!("../../../test-fixtures/ai_prompt_cases.rs");
    }

    fn test_config(base_url: &str) -> AiProviderConfig {
        AiProviderConfig {
            kind: AiProviderKind::OpenAi,
            api_key: "test-key".to_string(),
            model: "gpt-test".to_string(),
            base_url: base_url.to_string(),
            http_referer: None,
            app_title: None,
        }
    }

    fn openrouter_config(base_url: &str) -> AiProviderConfig {
        AiProviderConfig {
            kind: AiProviderKind::OpenRouter,
            api_key: "router-key".to_string(),
            model: "openrouter/free".to_string(),
            base_url: base_url.to_string(),
            http_referer: Some("https://waddle.chat".to_string()),
            app_title: None,
        }
    }

    #[tokio::test]
    async fn mocked_openai_response_becomes_answer() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{ "message": { "content": "Rawkode is David Flanagan." }, "finish_reason": "stop" }]
            })))
            .mount(&server)
            .await;

        let answer = generate_ai_response_with_config(
            &reqwest::Client::new(),
            &test_config(&server.uri()),
            "/ai Who is Rawkode?",
            &NoopToolExecutor,
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
                "choices": [{ "message": { "content": "Ottawa" }, "finish_reason": "stop" }]
            })))
            .mount(&server)
            .await;

        let answer = generate_ai_response_with_config(
            &reqwest::Client::new(),
            &openrouter_config(&server.uri()),
            "/ai what is the capital of Canada?",
            &NoopToolExecutor,
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
                "choices": [{ "message": { "content": "ok" }, "finish_reason": "stop" }]
            })))
            .mount(&server)
            .await;

        generate_ai_response_with_config(
            &reqwest::Client::new(),
            &test_config(&server.uri()),
            "/ai Who is Rawkode?",
            &NoopToolExecutor,
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
            &test_config(&server.uri()),
            "/ai no fake answers",
            &NoopToolExecutor,
        )
        .await
        .expect_err("empty response");

        assert!(matches!(err, AiProviderError::EmptyResponse));
    }

    #[tokio::test]
    async fn tools_are_included_in_every_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_string_contains("search_channel_history"))
            .and(body_string_contains("get_recent_messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{ "message": { "content": "ok" }, "finish_reason": "stop" }]
            })))
            .mount(&server)
            .await;

        generate_ai_response_with_config(
            &reqwest::Client::new(),
            &test_config(&server.uri()),
            "/ai hello",
            &NoopToolExecutor,
        )
        .await
        .expect("request includes tool definitions");
    }

    struct FixedToolExecutor {
        messages: Vec<HistoricalMessage>,
    }

    #[async_trait]
    impl AiToolExecutor for FixedToolExecutor {
        async fn execute(&self, _request: AiToolRequest) -> AiToolResult {
            AiToolResult::Messages(self.messages.clone())
        }
    }

    #[tokio::test]
    async fn tool_call_is_executed_and_result_sent_back() {
        let server = MockServer::start().await;

        // First request: model asks to search channel history
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_string_contains("search_channel_history"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": null,
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": "search_channel_history",
                                "arguments": "{\"query\": \"release notes\"}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        // Second request: model has tool results and returns final answer
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_string_contains("alice: Hello everyone"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{ "message": { "content": "I found the relevant messages." }, "finish_reason": "stop" }]
            })))
            .mount(&server)
            .await;

        let executor = FixedToolExecutor {
            messages: vec![HistoricalMessage {
                sender_nick: "alice".to_string(),
                body: "Hello everyone".to_string(),
            }],
        };

        let answer = generate_ai_response_with_config(
            &reqwest::Client::new(),
            &test_config(&server.uri()),
            "/ai search for release notes",
            &executor,
        )
        .await
        .expect("tool-augmented response");

        assert_eq!(answer, "I found the relevant messages.");
    }

    #[tokio::test]
    async fn get_recent_messages_tool_call_is_handled() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": null,
                        "tool_calls": [{
                            "id": "call_2",
                            "type": "function",
                            "function": {
                                "name": "get_recent_messages",
                                "arguments": "{\"max\": 5}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_string_contains("bob: standup update"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{ "message": { "content": "Here is the recent context." }, "finish_reason": "stop" }]
            })))
            .mount(&server)
            .await;

        let executor = FixedToolExecutor {
            messages: vec![HistoricalMessage {
                sender_nick: "bob".to_string(),
                body: "standup update".to_string(),
            }],
        };

        let answer = generate_ai_response_with_config(
            &reqwest::Client::new(),
            &test_config(&server.uri()),
            "/ai what did bob say?",
            &executor,
        )
        .await
        .expect("get_recent_messages tool response");

        assert_eq!(answer, "Here is the recent context.");
    }

    #[tokio::test]
    async fn unhandled_extension_tool_call_returns_error_result_to_model() {
        // An extension tool with no registered executor should return an error
        // to the model so it can recover gracefully rather than stalling.
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": null,
                        "tool_calls": [{
                            "id": "call_unknown",
                            "type": "function",
                            "function": {
                                "name": "some_extension_tool",
                                "arguments": "{}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        // The error result is sent back; model produces a final answer.
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_string_contains("Error:"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{ "message": { "content": "I couldn't use that tool." }, "finish_reason": "stop" }]
            })))
            .mount(&server)
            .await;

        let answer = generate_ai_response_with_config(
            &reqwest::Client::new(),
            &test_config(&server.uri()),
            "/ai test",
            &NoopToolExecutor,
        )
        .await
        .expect("unhandled extension tool handled gracefully");

        assert_eq!(answer, "I couldn't use that tool.");
    }

    #[tokio::test]
    async fn noop_executor_returns_unavailable_error_to_model() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": null,
                        "tool_calls": [{
                            "id": "call_noop",
                            "type": "function",
                            "function": {
                                "name": "get_recent_messages",
                                "arguments": "{}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_string_contains("Channel history unavailable"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{ "message": { "content": "I don't have access to history right now." }, "finish_reason": "stop" }]
            })))
            .mount(&server)
            .await;

        let answer = generate_ai_response_with_config(
            &reqwest::Client::new(),
            &test_config(&server.uri()),
            "/ai summarize",
            &NoopToolExecutor,
        )
        .await
        .expect("noop executor error propagated to model");

        assert_eq!(answer, "I don't have access to history right now.");
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
