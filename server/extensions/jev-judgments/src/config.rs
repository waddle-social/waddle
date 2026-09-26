use std::fmt;

use serde::Deserialize;
use url::Url;

pub const DEFAULT_ENDPOINT: &str = "https://openrouter.ai/api/alpha/decisions";
pub const DEFAULT_MODEL: &str = "typesafe/jev-1.13";
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Clone)]
pub struct JevConfig {
    pub endpoint: Url,
    pub model: String,
    pub api_key: Secret,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    InvalidJson,
    InvalidEndpoint,
    MissingModel,
    MissingApiKey,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson => f.write_str("Jev config must be a JSON object"),
            Self::InvalidEndpoint => f.write_str("Jev endpoint must be an HTTPS URL"),
            Self::MissingModel => f.write_str("Jev model must be non-empty"),
            Self::MissingApiKey => f.write_str("Jev API key is not configured"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[derive(Deserialize)]
struct RawConfig {
    endpoint: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
}

impl JevConfig {
    pub fn parse(input: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig = serde_json::from_str(input).map_err(|_| ConfigError::InvalidJson)?;
        let endpoint = raw.endpoint.as_deref().unwrap_or(DEFAULT_ENDPOINT);
        let endpoint = Url::parse(endpoint).map_err(|_| ConfigError::InvalidEndpoint)?;
        if endpoint.scheme() != "https"
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
        {
            return Err(ConfigError::InvalidEndpoint);
        }
        let model = raw.model.as_deref().unwrap_or(DEFAULT_MODEL).trim();
        if model.is_empty() {
            return Err(ConfigError::MissingModel);
        }
        let api_key = raw
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or(ConfigError::MissingApiKey)?;
        Ok(Self {
            endpoint,
            model: model.to_string(),
            api_key: Secret(api_key.to_string()),
        })
    }
}
