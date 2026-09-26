//! Extension config: `{"api_key": "...", "model": "typesafe/jev-1.13"}`.
//! `api_key` is expected to arrive via the host's `config_secret_files`
//! mechanism (`waddle_extensions::config::ExtensionModuleConfig::config_secret_files`),
//! which reads a mounted secret file and injects its contents as a string
//! value under the given config key — this extension never reads a secret
//! from anywhere else. `model` is optional and defaults to
//! [`crate::constants::DEFAULT_JEV_MODEL`].

use serde::Deserialize;

use crate::constants::DEFAULT_JEV_MODEL;

#[derive(Debug, Deserialize)]
struct RawConfig {
    api_key: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderConfig {
    pub(crate) api_key: String,
    pub(crate) model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProviderConfigError {
    InvalidJson(String),
    MissingApiKey,
}

impl std::fmt::Display for ProviderConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidJson(error) => {
                write!(
                    formatter,
                    "community-safety-judge config is not valid JSON: {error}"
                )
            }
            Self::MissingApiKey => {
                write!(
                    formatter,
                    "community-safety-judge config is missing a non-empty api_key"
                )
            }
        }
    }
}

impl ProviderConfig {
    pub(crate) fn parse(raw: &str) -> Result<Self, ProviderConfigError> {
        let parsed: RawConfig = serde_json::from_str(raw)
            .map_err(|error| ProviderConfigError::InvalidJson(error.to_string()))?;
        let api_key = parsed
            .api_key
            .filter(|key| !key.trim().is_empty())
            .ok_or(ProviderConfigError::MissingApiKey)?;
        Ok(Self {
            api_key,
            model: parsed
                .model
                .filter(|model| !model.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_JEV_MODEL.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_api_key_and_default_model() {
        let config = ProviderConfig::parse(r#"{"api_key": "secret-key"}"#).expect("parse");
        assert_eq!(config.api_key, "secret-key");
        assert_eq!(config.model, DEFAULT_JEV_MODEL);
    }

    #[test]
    fn parses_model_override() {
        let config =
            ProviderConfig::parse(r#"{"api_key": "secret-key", "model": "typesafe/jev-2"}"#)
                .expect("parse");
        assert_eq!(config.model, "typesafe/jev-2");
    }

    #[test]
    fn rejects_missing_api_key() {
        let error = ProviderConfig::parse(r#"{}"#).expect_err("missing api_key must be rejected");
        assert_eq!(error, ProviderConfigError::MissingApiKey);
    }

    #[test]
    fn rejects_empty_api_key() {
        let error = ProviderConfig::parse(r#"{"api_key": "   "}"#)
            .expect_err("blank api_key must be rejected");
        assert_eq!(error, ProviderConfigError::MissingApiKey);
    }

    #[test]
    fn rejects_invalid_json() {
        let error = ProviderConfig::parse("not json").expect_err("invalid JSON must be rejected");
        assert!(matches!(error, ProviderConfigError::InvalidJson(_)));
    }
}
