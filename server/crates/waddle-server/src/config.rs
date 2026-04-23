//! Server configuration.

use crate::auth::providers::AuthProviderConfig;
use crate::db::DatabaseDriver;
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use tracing::info;
use waddle_extensions::ExtensionConfig;

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
    pub fn auth_broker_allowed(&self) -> bool {
        matches!(self, ServerMode::HomeServer)
    }
}

impl FromStr for ServerMode {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "standalone" | "xmpp" | "xmpp-only" => ServerMode::Standalone,
            _ => ServerMode::HomeServer,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct AuthConfig {
    pub providers: Vec<AuthProviderConfig>,
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

/// SpiceDB schema configuration for bootstrap behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpiceDbSchemaConfig {
    pub bootstrap: bool,
    pub schema_version: u64,
}

/// SpiceDB backend configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpiceDbConfig {
    pub endpoint: String,
    pub preshared_key: String,
    pub insecure: bool,
    pub schema: SpiceDbSchemaConfig,
}

impl SpiceDbConfig {
    pub fn from_env() -> Result<Option<Self>, String> {
        let endpoint = std::env::var("WADDLE_SPICEDB_ENDPOINT")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let preshared_key = std::env::var("WADDLE_SPICEDB_PRESHARED_KEY")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        match (endpoint, preshared_key) {
            (None, None) => Ok(None),
            (Some(_), None) => Err(
                "WADDLE_SPICEDB_PRESHARED_KEY is required when WADDLE_SPICEDB_ENDPOINT is set"
                    .to_string(),
            ),
            (None, Some(_)) => Err(
                "WADDLE_SPICEDB_ENDPOINT is required when WADDLE_SPICEDB_PRESHARED_KEY is set"
                    .to_string(),
            ),
            (Some(endpoint), Some(preshared_key)) => {
                let insecure = std::env::var("WADDLE_SPICEDB_INSECURE")
                    .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
                    .unwrap_or(false);

                let bootstrap = std::env::var("WADDLE_SPICEDB_BOOTSTRAP_SCHEMA")
                    .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
                    .unwrap_or(false);

                let schema_version = match std::env::var("WADDLE_SPICEDB_SCHEMA_VERSION") {
                    Ok(value) => value.parse::<u64>().map_err(|e| {
                        format!(
                            "invalid WADDLE_SPICEDB_SCHEMA_VERSION value '{}': {}",
                            value, e
                        )
                    })?,
                    Err(_) => 1,
                };

                Ok(Some(Self {
                    endpoint,
                    preshared_key,
                    insecure,
                    schema: SpiceDbSchemaConfig {
                        bootstrap,
                        schema_version,
                    },
                }))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub mode: ServerMode,
    pub base_url: String,
    pub session_key: Option<String>,
    pub auth: AuthConfig,
    /// Runtime extension configuration.
    pub extensions: ExtensionConfig,
    /// SpiceDB backend configuration.
    /// Runtime startup requires this to be set.
    pub spicedb: Option<SpiceDbConfig>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            mode: ServerMode::default(),
            base_url: "http://localhost:3000".to_string(),
            session_key: None,
            auth: AuthConfig::default(),
            extensions: ExtensionConfig::default(),
            spicedb: None,
        }
    }
}

impl ServerConfig {
    pub fn from_env() -> Result<Self, String> {
        let mode_str = std::env::var("WADDLE_MODE").unwrap_or_else(|_| "homeserver".to_string());
        let mode = mode_str.parse().unwrap_or_default();

        let base_url = std::env::var("WADDLE_BASE_URL")
            .unwrap_or_else(|_| "http://localhost:3000".to_string());

        let session_key = std::env::var("WADDLE_SESSION_KEY").ok();
        let auth = AuthConfig::from_env()?;

        let extensions =
            ExtensionConfig::from_env().map_err(|e| format!("invalid extension config: {e}"))?;
        let spicedb = SpiceDbConfig::from_env()?;

        Ok(Self {
            mode,
            base_url,
            session_key,
            auth,
            extensions,
            spicedb,
        })
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
            extensions: ExtensionConfig::default(),
            spicedb: None,
        }
    }

    #[cfg(test)]
    pub fn test_standalone() -> Self {
        Self {
            mode: ServerMode::Standalone,
            base_url: "http://localhost:3000".to_string(),
            session_key: Some("test-key-32-bytes-long-for-aes!".to_string()),
            auth: AuthConfig::default(),
            extensions: ExtensionConfig::default(),
            spicedb: None,
        }
    }
}


#[derive(Debug, Clone)]
pub struct DatabaseRuntimeConfig {
    pub driver: DatabaseDriver,
    pub database_url: String,
}

impl Default for DatabaseRuntimeConfig {
    fn default() -> Self {
        Self {
            driver: DatabaseDriver::Sqlite,
            database_url: "sqlite::memory:".to_string(),
        }
    }
}

impl DatabaseRuntimeConfig {
    pub fn from_env() -> Result<Self, String> {
        let driver = std::env::var("WADDLE_DB_DRIVER")
            .unwrap_or_else(|_| "sqlite".to_string())
            .parse::<DatabaseDriver>()
            .map_err(|e| format!("invalid WADDLE_DB_DRIVER: {}", e))?;

        let database_url = std::env::var("WADDLE_DATABASE_URL").unwrap_or_else(|_| match driver {
            DatabaseDriver::Sqlite => "sqlite::memory:".to_string(),
            DatabaseDriver::Postgres => {
                "postgres://postgres:postgres@localhost:5432/waddle".to_string()
            }
        });

        Ok(Self {
            driver,
            database_url,
        })
    }
}
