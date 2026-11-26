use crate::bindings::waddle::extension::types;
use crate::constants::{
    DEFAULT_CONTEXT_LIMIT, DEFAULT_DEPLOYMENT, DEFAULT_PROVIDER, MAX_CONTEXT_LIMIT,
};

pub(crate) trait ProviderExecutor {
    fn execute(&self, request: ProviderRequest) -> Result<ProviderAnswer, ProviderExecutionError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CredentialPreference {
    InvokingUser,
    Tenant(NonEmptyString),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderConfig {
    pub(crate) provider: NonEmptyString,
    pub(crate) deployment: NonEmptyString,
    pub(crate) model: NonEmptyString,
    pub(crate) credential: CredentialPreference,
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
        if document.get("api_key").is_some() || document.get("endpoint").is_some() {
            return Err(ProviderConfigError::LegacyPlaintextConfigForbidden);
        }
        let provider = json_config_string(&document, "provider")
            .unwrap_or_else(|| DEFAULT_PROVIDER.to_string());
        let deployment = json_config_string(&document, "deployment")
            .unwrap_or_else(|| DEFAULT_DEPLOYMENT.to_string());
        let model =
            json_config_string(&document, "model").ok_or(ProviderConfigError::MissingModel)?;
        let credential = match json_config_string(&document, "credential_name") {
            Some(name) => {
                let name =
                    NonEmptyString::try_new(name).ok_or(ProviderConfigError::InvalidCredential)?;
                CredentialPreference::Tenant(name)
            }
            None => CredentialPreference::InvokingUser,
        };
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
            provider: NonEmptyString::try_new(provider)
                .ok_or(ProviderConfigError::MissingProvider)?,
            deployment: NonEmptyString::try_new(deployment)
                .ok_or(ProviderConfigError::MissingDeployment)?,
            model: NonEmptyString::try_new(model).ok_or(ProviderConfigError::MissingModel)?,
            credential,
            system_prompt: json_config_string(&document, "system_prompt")
                .and_then(NonEmptyString::try_new),
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
    LegacyPlaintextConfigForbidden,
    MissingProvider,
    MissingDeployment,
    MissingModel,
    InvalidCredential,
    InvalidContextLimit,
}

impl std::fmt::Display for ProviderConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => f.write_str("expected JSON config with broker provider and model"),
            Self::InvalidJson => f.write_str("provider config must be a JSON object"),
            Self::LegacyPlaintextConfigForbidden => {
                f.write_str("legacy plaintext api_key/endpoint configuration is forbidden")
            }
            Self::MissingProvider => f.write_str("missing provider"),
            Self::MissingDeployment => f.write_str("missing deployment"),
            Self::MissingModel => f.write_str("missing model"),
            Self::InvalidCredential => f.write_str("credential_name must be non-empty"),
            Self::InvalidContextLimit => f.write_str("context_limit must be an unsigned integer"),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderRequest {
    pub(crate) provider: NonEmptyString,
    pub(crate) deployment: NonEmptyString,
    pub(crate) model: NonEmptyString,
    pub(crate) credential: CredentialPreference,
    pub(crate) context_limit: u32,
    pub(crate) messages: Vec<ProviderMessage>,
    pub(crate) tools: Vec<HostToolRequest>,
    pub(crate) tool_target: Option<ResponseTarget>,
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
    Channels,
    Spaces,
}

impl HostTool {
    pub(crate) fn from_provider_name(name: &str) -> Option<Self> {
        match name {
            "query_mam" => Some(Self::QueryMam),
            "list_room_members" => Some(Self::Members),
            "list_channels" => Some(Self::Channels),
            "list_spaces" => Some(Self::Spaces),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ExecutionContext {
    pub(crate) prompt: CleanPrompt,
    pub(crate) response_target: Option<ResponseTarget>,
}

impl ExecutionContext {
    pub(crate) fn command(prompt: CleanPrompt, response_target: Option<ResponseTarget>) -> Self {
        Self {
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
    Denied(String),
    #[cfg(not(test))]
    Canceled,
    #[cfg(not(test))]
    RateLimited,
    BrokerFailure(String),
    InvalidResponse(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CleanPrompt(pub(crate) NonEmptyString);

impl CleanPrompt {
    pub(crate) fn try_new(value: String) -> Option<Self> {
        NonEmptyString::try_new(value).map(Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NonEmptyString(String);

impl NonEmptyString {
    pub(crate) fn try_new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then(|| Self(value.trim().to_string()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}
