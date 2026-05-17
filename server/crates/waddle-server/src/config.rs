//! Server configuration.

use crate::auth::providers::AuthProviderConfig;
use crate::db::DatabaseDriver;
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use tracing::info;
use waddle_extensions::ExtensionConfig;
use waddle_xmpp::xep::xep0421::{OccupantIdSecret, OCCUPANT_ID_SECRET_MIN_BYTES};

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

/// SpiceDB backend configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpiceDbConfig {
    pub endpoint: String,
    pub preshared_key: String,
    pub insecure: bool,
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

                Ok(Some(Self {
                    endpoint,
                    preshared_key,
                    insecure,
                }))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub mode: ServerMode,
    pub base_url: String,
    pub session_key: String,
    pub auth: AuthConfig,
    /// Runtime extension configuration.
    pub extensions: ExtensionConfig,
    /// SpiceDB backend configuration.
    /// Runtime startup requires this to be set.
    pub spicedb: Option<SpiceDbConfig>,
    /// Per-deployment HMAC key used to derive XEP-0421 occupant
    /// identifiers. Loaded from `WADDLE_OCCUPANT_ID_SECRET` and shared
    /// across the WebSocket dependencies and `RoomRegistryActor` so
    /// every stamping site reads the same key. Required at startup;
    /// see [`parse_occupant_id_secret`] for the validation rules.
    pub occupant_id_secret: OccupantIdSecret,
}

/// Validate the `WADDLE_OCCUPANT_ID_SECRET` env var into a typed secret.
///
/// Pure function so the validation logic is unit-testable without
/// mutating process-global env state. Called by [`ServerConfig::from_env`]
/// with the result of `std::env::var(...).ok().as_deref()`.
fn parse_occupant_id_secret(raw: Option<&str>) -> Result<OccupantIdSecret, String> {
    let value = raw.ok_or_else(|| {
        format!(
            "WADDLE_OCCUPANT_ID_SECRET is required (≥{OCCUPANT_ID_SECRET_MIN_BYTES} bytes; \
             generate with: openssl rand -base64 48)"
        )
    })?;
    OccupantIdSecret::new(value.as_bytes().to_vec()).map_err(|e| {
        format!(
            "WADDLE_OCCUPANT_ID_SECRET invalid: {e} \
             (generate with: openssl rand -base64 48)"
        )
    })
}

const SESSION_KEY_MIN_BYTES: usize = 32;

fn parse_session_key(raw: Option<&str>) -> Result<String, String> {
    let value = raw.filter(|value| !value.is_empty()).ok_or_else(|| {
        "WADDLE_SESSION_KEY is required (generate with: openssl rand -base64 48)".to_string()
    })?;
    if value.len() < SESSION_KEY_MIN_BYTES {
        return Err(format!(
            "WADDLE_SESSION_KEY must be at least {SESSION_KEY_MIN_BYTES} bytes \
             (generate with: openssl rand -base64 48)"
        ));
    }
    Ok(value.to_string())
}

#[cfg(test)]
const TEST_OCCUPANT_ID_SECRET: &str = "test-occupant-id-secret-32-bytes-long";

#[cfg(test)]
fn test_occupant_id_secret() -> OccupantIdSecret {
    OccupantIdSecret::new(TEST_OCCUPANT_ID_SECRET.as_bytes().to_vec())
        .expect("test secret meets length floor")
}

// `Default` is gated to `#[cfg(test)]`. Production startup MUST go
// through `ServerConfig::from_env`, which enforces the deployment-keyed
// `WADDLE_SESSION_KEY` and `WADDLE_OCCUPANT_ID_SECRET`; a non-test
// `Default` impl could be silently used (e.g. via `..Default::default()`
// in scaffolding) and reintroduce the cross-deployment linkability that
// #283 closes.
#[cfg(test)]
impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            mode: ServerMode::default(),
            base_url: "http://localhost:3000".to_string(),
            session_key: "test-session-key-32-bytes-minimum".to_string(),
            auth: AuthConfig::default(),
            extensions: ExtensionConfig::default(),
            spicedb: None,
            occupant_id_secret: test_occupant_id_secret(),
        }
    }
}

impl ServerConfig {
    pub fn from_env() -> Result<Self, String> {
        let mode_str = std::env::var("WADDLE_MODE").unwrap_or_else(|_| "homeserver".to_string());
        let mode = mode_str.parse().unwrap_or_default();

        let base_url = std::env::var("WADDLE_BASE_URL")
            .unwrap_or_else(|_| "http://localhost:3000".to_string());

        let session_key = parse_session_key(std::env::var("WADDLE_SESSION_KEY").ok().as_deref())?;
        let auth = AuthConfig::from_env()?;

        let extensions =
            ExtensionConfig::from_env().map_err(|e| format!("invalid extension config: {e}"))?;
        let spicedb = SpiceDbConfig::from_env()?;

        let occupant_id_secret =
            parse_occupant_id_secret(std::env::var("WADDLE_OCCUPANT_ID_SECRET").ok().as_deref())?;

        Ok(Self {
            mode,
            base_url,
            session_key,
            auth,
            extensions,
            spicedb,
            occupant_id_secret,
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
            session_key: "test-key-32-bytes-long-for-aes!".to_string(),
            auth: AuthConfig::default(),
            extensions: ExtensionConfig::default(),
            spicedb: None,
            occupant_id_secret: test_occupant_id_secret(),
        }
    }

    #[cfg(test)]
    pub fn test_standalone() -> Self {
        Self {
            mode: ServerMode::Standalone,
            base_url: "http://localhost:3000".to_string(),
            session_key: "test-key-32-bytes-long-for-aes!".to_string(),
            auth: AuthConfig::default(),
            extensions: ExtensionConfig::default(),
            spicedb: None,
            occupant_id_secret: test_occupant_id_secret(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_session_key_rejects_unset() {
        let err = parse_session_key(None).unwrap_err();
        assert!(
            err.contains("WADDLE_SESSION_KEY is required"),
            "error must name the env var; got: {err}"
        );
        assert!(
            err.contains("openssl rand"),
            "error must include the generation recipe; got: {err}"
        );
    }

    #[test]
    fn parse_session_key_rejects_empty() {
        let err = parse_session_key(Some("")).unwrap_err();
        assert!(
            err.contains("WADDLE_SESSION_KEY is required"),
            "empty key must be treated as unset; got: {err}"
        );
    }

    #[test]
    fn parse_session_key_accepts_value() {
        let value = "test-session-key-32-bytes-minimum";
        assert_eq!(parse_session_key(Some(value)).unwrap(), value);
    }

    #[test]
    fn parse_session_key_rejects_short_value() {
        let err = parse_session_key(Some("short")).unwrap_err();
        assert!(
            err.contains("at least"),
            "error must mention the length floor; got: {err}"
        );
        assert!(
            err.contains("openssl rand"),
            "error must include the generation recipe; got: {err}"
        );
    }

    #[test]
    fn parse_occupant_id_secret_rejects_unset() {
        let err = parse_occupant_id_secret(None).unwrap_err();
        assert!(
            err.contains("WADDLE_OCCUPANT_ID_SECRET is required"),
            "error must name the env var; got: {err}"
        );
        assert!(
            err.contains("openssl rand"),
            "error must include the generation recipe; got: {err}"
        );
    }

    #[test]
    fn parse_occupant_id_secret_rejects_short_value() {
        let err = parse_occupant_id_secret(Some("short")).unwrap_err();
        assert!(
            err.contains("at least"),
            "error must mention the length floor; got: {err}"
        );
        assert!(
            err.contains("openssl rand"),
            "error must include the generation recipe; got: {err}"
        );
    }

    #[test]
    fn parse_occupant_id_secret_accepts_minimum_length() {
        // Exactly the floor — must succeed.
        let value: String = "x".repeat(OCCUPANT_ID_SECRET_MIN_BYTES);
        let secret = parse_occupant_id_secret(Some(&value)).expect("32 bytes is accepted");
        assert_eq!(secret.key().len(), OCCUPANT_ID_SECRET_MIN_BYTES);
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
