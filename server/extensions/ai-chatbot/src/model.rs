use crate::bindings::waddle::extension::types;
use crate::constants::{DEFAULT_CONTEXT_LIMIT, MAX_CONTEXT_LIMIT};

pub(crate) trait ProviderExecutor {
    fn execute(&self, request: ProviderRequest) -> Result<ProviderAnswer, ProviderExecutionError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderConfig {
    pub(crate) endpoint: NonEmptyString,
    pub(crate) model: NonEmptyString,
    pub(crate) api_key: NonEmptyString,
    pub(crate) system_prompt: Option<NonEmptyString>,
    pub(crate) context_limit: u32,
}

impl ProviderConfig {
    pub(crate) fn parse(input: &str) -> Result<Self, ProviderConfigError> {
        if input.trim().is_empty() {
            return Err(ProviderConfigError::Missing);
        }
        let document = serde_json::from_str::<serde_json::Value>(input)
            .map_err(|_| ProviderConfigError::InvalidJson)?;
        let endpoint = json_config_string(&document, "endpoint")
            .ok_or(ProviderConfigError::MissingEndpoint)?;
        let model =
            json_config_string(&document, "model").ok_or(ProviderConfigError::MissingModel)?;
        let api_key =
            json_config_string(&document, "api_key").ok_or(ProviderConfigError::MissingApiKey)?;
        let context_limit = json_config_string(&document, "context_limit")
            .map(|value| {
                value
                    .parse::<u32>()
                    .map_err(|_| ProviderConfigError::InvalidContextLimit)
            })
            .transpose()?
            .unwrap_or(DEFAULT_CONTEXT_LIMIT)
            .min(MAX_CONTEXT_LIMIT);
        Ok(Self {
            endpoint: NonEmptyString::new(endpoint).ok_or(ProviderConfigError::MissingEndpoint)?,
            model: NonEmptyString::new(model).ok_or(ProviderConfigError::MissingModel)?,
            api_key: NonEmptyString::new(api_key).ok_or(ProviderConfigError::MissingApiKey)?,
            system_prompt: json_config_string(&document, "system_prompt")
                .and_then(NonEmptyString::new),
            context_limit,
        })
    }
}

fn json_config_string(document: &serde_json::Value, key: &str) -> Option<String> {
    let value = document.get(key)?;
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProviderConfigError {
    Missing,
    InvalidJson,
    MissingEndpoint,
    MissingModel,
    MissingApiKey,
    InvalidContextLimit,
}

impl std::fmt::Display for ProviderConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => f.write_str("expected JSON config with endpoint, model, and api_key"),
            Self::InvalidJson => f.write_str("provider config must be a JSON object"),
            Self::MissingEndpoint => f.write_str("missing endpoint"),
            Self::MissingModel => f.write_str("missing model"),
            Self::MissingApiKey => f.write_str("missing api_key"),
            Self::InvalidContextLimit => f.write_str("context_limit must be an unsigned integer"),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderRequest {
    pub(crate) endpoint: NonEmptyString,
    pub(crate) model: NonEmptyString,
    pub(crate) api_key: NonEmptyString,
    pub(crate) context_limit: u32,
    pub(crate) messages: Vec<ProviderMessage>,
    pub(crate) tools: Vec<HostToolRequest>,
    pub(crate) tool_target: Option<ResponseTarget>,
    pub(crate) requester: Option<types::BareJid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderMessage {
    pub(crate) role: ProviderRole,
    pub(crate) content: NonEmptyString,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderRole {
    System,
    User,
}

impl ProviderRole {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostToolRequest {
    pub(crate) tool: HostTool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostTool {
    QueryMam,
    Members,
    Presence,
    Roster,
    Channels,
    Spaces,
}

impl HostTool {
    pub(crate) fn from_provider_name(name: &str) -> Option<Self> {
        match name {
            "query_mam" => Some(Self::QueryMam),
            "list_room_members" => Some(Self::Members),
            "get_presence" => Some(Self::Presence),
            "get_roster" => Some(Self::Roster),
            "list_channels" => Some(Self::Channels),
            "list_spaces" => Some(Self::Spaces),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ExecutionContext {
    pub(crate) requester: Option<types::BareJid>,
    pub(crate) prompt: CleanPrompt,
    pub(crate) response_target: Option<ResponseTarget>,
}

impl ExecutionContext {
    pub(crate) fn command(
        requester: types::FullJid,
        prompt: CleanPrompt,
        response_target: Option<ResponseTarget>,
    ) -> Self {
        Self {
            requester: bare_jid_from_full(&requester.value),
            prompt,
            response_target,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ResponseTarget {
    pub(crate) room: types::RoomJid,
    pub(crate) thread_id: Option<types::ThreadId>,
    pub(crate) reply_to: Option<types::ReplyTarget>,
    pub(crate) focus_thread: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderAnswer {
    pub(crate) text: NonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProviderExecutionError {
    Http(String),
    HttpStatus { status: u16, body: String },
    InvalidResponse(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CleanPrompt(pub(crate) NonEmptyString);

impl CleanPrompt {
    pub(crate) fn new(value: String) -> Option<Self> {
        NonEmptyString::new(value).map(Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NonEmptyString(String);

impl NonEmptyString {
    pub(crate) fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then(|| Self(value.trim().to_string()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

fn bare_jid_from_full(value: &str) -> Option<types::BareJid> {
    let bare = value
        .split_once('/')
        .map_or(value, |(bare, _resource)| bare);
    NonEmptyString::new(bare).map(|value| types::BareJid {
        value: value.as_str().to_string(),
    })
}
