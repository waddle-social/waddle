use std::fmt;

use serde::Deserialize;

pub const DEFAULT_PROVIDER: &str = "openrouter";
pub const DEFAULT_DEPLOYMENT: &str = "zdr";
pub const DEFAULT_MODEL: &str = "typesafe/jev-1.13";
pub const DEFAULT_CREDENTIAL_NAME: &str = "tenant-default";
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JevConfig {
    pub provider: String,
    pub deployment: String,
    pub model: String,
    pub credential_name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    InvalidJson,
    LegacySecretForbidden,
    MissingModel,
    InvalidField,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson => f.write_str("Jev config must be a JSON object"),
            Self::LegacySecretForbidden => {
                f.write_str("legacy plaintext api_key/endpoint config is forbidden")
            }
            Self::MissingModel => f.write_str("Jev model must be non-empty"),
            Self::InvalidField => f.write_str("Jev provider/deployment/credential_name invalid"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[derive(Deserialize)]
struct RawConfig {
    provider: Option<String>,
    deployment: Option<String>,
    model: Option<String>,
    credential_name: Option<String>,
    endpoint: Option<String>,
    api_key: Option<serde_json::Value>,
}

impl JevConfig {
    pub fn parse(input: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig = serde_json::from_str(input).map_err(|_| ConfigError::InvalidJson)?;
        if raw.api_key.is_some() || raw.endpoint.is_some() {
            return Err(ConfigError::LegacySecretForbidden);
        }
        let provider = raw
            .provider
            .as_deref()
            .unwrap_or(DEFAULT_PROVIDER)
            .trim()
            .to_string();
        let deployment = raw
            .deployment
            .as_deref()
            .unwrap_or(DEFAULT_DEPLOYMENT)
            .trim()
            .to_string();
        let credential_name = raw
            .credential_name
            .as_deref()
            .unwrap_or(DEFAULT_CREDENTIAL_NAME)
            .trim()
            .to_string();
        if provider.is_empty() || deployment.is_empty() || credential_name.is_empty() {
            return Err(ConfigError::InvalidField);
        }
        let model = raw.model.as_deref().unwrap_or(DEFAULT_MODEL).trim();
        if model.is_empty() {
            return Err(ConfigError::MissingModel);
        }
        Ok(Self {
            provider,
            deployment,
            model: model.to_string(),
            credential_name,
        })
    }
}
