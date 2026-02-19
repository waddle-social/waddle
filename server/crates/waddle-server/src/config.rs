//! Server configuration.

use crate::auth::providers::AuthProviderConfig;
use serde::{Deserialize, Serialize};
use std::fmt;
use tracing::info;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ServerMode {
    /// Full server mode with HTTP auth broker + XMPP.
    #[default]
    HomeServer,
    /// Standalone XMPP-focused mode.
    Standalone,
}

impl fmt::Display for ServerMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServerMode::HomeServer => write!(f, "HomeServer"),
            ServerMode::Standalone => write!(f, "Standalone"),
        }
    }
}

impl ServerMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "standalone" | "xmpp" | "xmpp-only" => ServerMode::Standalone,
            _ => ServerMode::HomeServer,
        }
    }

    pub fn auth_broker_allowed(&self) -> bool {
        matches!(self, ServerMode::HomeServer)
    }
}

#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub providers: Vec<AuthProviderConfig>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self { providers: vec![] }
    }
}

impl AuthConfig {
    pub fn from_env() -> Result<Self, String> {
        let raw = std::env::var("WADDLE_AUTH_PROVIDERS_JSON").unwrap_or_else(|_| "[]".to_string());
        let trimmed = raw.trim();

        let providers = if trimmed.starts_with('[') {
            serde_json::from_str::<Vec<AuthProviderConfig>>(trimmed)
                .map_err(|e| format!("invalid WADDLE_AUTH_PROVIDERS_JSON array: {}", e))?
        } else {
            #[derive(Deserialize)]
            struct Wrapper {
                providers: Vec<AuthProviderConfig>,
            }
            serde_json::from_str::<Wrapper>(trimmed)
                .map_err(|e| format!("invalid WADDLE_AUTH_PROVIDERS_JSON object: {}", e))?
                .providers
        };

        // Validation is strict and fails startup.
        let registry = crate::auth::ProviderRegistry::new(providers.clone())
            .map_err(|e| format!("invalid provider config: {}", e))?;

        if registry.is_empty() {
            info!("No auth providers configured");
        }

        Ok(Self { providers })
    }
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub mode: ServerMode,
    pub base_url: String,
    pub session_key: Option<String>,
    pub auth: AuthConfig,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            mode: ServerMode::default(),
            base_url: "http://localhost:3000".to_string(),
            session_key: None,
            auth: AuthConfig::default(),
        }
    }
}

impl ServerConfig {
    pub fn from_env() -> Self {
        let mode_str = std::env::var("WADDLE_MODE").unwrap_or_else(|_| "homeserver".to_string());
        let mode = ServerMode::from_str(&mode_str);

        let base_url = std::env::var("WADDLE_BASE_URL")
            .unwrap_or_else(|_| "http://localhost:3000".to_string());

        let session_key = std::env::var("WADDLE_SESSION_KEY").ok();
        let auth = AuthConfig::from_env()
            .unwrap_or_else(|err| panic!("Failed to parse auth provider config: {}", err));

        Self {
            mode,
            base_url,
            session_key,
            auth,
        }
    }

    pub fn auth_enabled(&self) -> bool {
        self.mode.auth_broker_allowed() && !self.auth.providers.is_empty()
    }

    pub fn log_config(&self) {
        info!("Running in {} mode", self.mode);
        info!("Base URL: {}", self.base_url);
        info!("Auth providers configured: {}", self.auth.providers.len());
        info!(
            "HTTP auth broker: {}",
            if self.auth_enabled() {
                "enabled"
            } else {
                "disabled"
            }
        );
    }

    #[cfg(test)]
    pub fn test_homeserver() -> Self {
        Self {
            mode: ServerMode::HomeServer,
            base_url: "http://localhost:3000".to_string(),
            session_key: Some("test-key-32-bytes-long-for-aes!".to_string()),
            auth: AuthConfig::default(),
        }
    }

    #[cfg(test)]
    pub fn test_standalone() -> Self {
        Self {
            mode: ServerMode::Standalone,
            base_url: "http://localhost:3000".to_string(),
            session_key: Some("test-key-32-bytes-long-for-aes!".to_string()),
            auth: AuthConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub version: String,
    pub license: String,
    pub mode: ServerMode,
    pub auth_enabled: bool,
    pub native_auth_available: bool,
    pub features: ServerFeatures,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerFeatures {
    pub oauth: bool,
    pub device_flow: bool,
    pub xmpp_oauth: bool,
    pub auth_page: bool,
    pub websocket: bool,
    pub communities: bool,
}

impl ServerInfo {
    pub fn from_config(config: &ServerConfig, native_auth_enabled: bool) -> Self {
        let auth_enabled = config.auth_enabled();
        let features = ServerFeatures {
            oauth: auth_enabled,
            device_flow: auth_enabled,
            xmpp_oauth: auth_enabled,
            auth_page: auth_enabled,
            websocket: true,
            communities: true,
        };

        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            license: "AGPL-3.0".to_string(),
            mode: config.mode,
            auth_enabled,
            native_auth_available: native_auth_enabled,
            features,
        }
    }
}
