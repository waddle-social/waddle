//! Server configuration module for WADDLE_MODE environment variable support.
//!
//! This module provides configuration for running the server in different modes:
//! - **HomeServer**: Full functionality with ATProto OAuth authentication
//! - **Standalone**: XMPP-only mode without ATProto integration
//!
//! # Environment Variables
//!
//! - `WADDLE_MODE`: Server mode (`homeserver` or `standalone`). Default: `homeserver`
//!
//! # Examples
//!
//! Running in standalone mode (XMPP only):
//! ```bash
//! WADDLE_MODE=standalone cargo run
//! ```
//!
//! Running in homeserver mode (full features):
//! ```bash
//! WADDLE_MODE=homeserver cargo run
//! # or simply (default):
//! cargo run
//! ```

use serde::{Deserialize, Serialize};
use std::fmt;
use tracing::info;

/// Server operating mode.
///
/// Determines which features and routes are available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ServerMode {
    /// Full server mode with ATProto OAuth authentication.
    ///
    /// Enables:
    /// - ATProto OAuth routes (/oauth/*, /v1/auth/atproto/*)
    /// - Device flow routes (/v1/auth/device/*)
    /// - XMPP OAuth routes (/.well-known/oauth-authorization-server, /v1/auth/xmpp/*)
    /// - Auth page routes (/auth, /auth/*)
    /// - All community features (waddles, channels, permissions)
    /// - XMPP server with AT Protocol authentication
    #[default]
    HomeServer,

    /// Standalone XMPP mode without ATProto integration.
    ///
    /// Enables:
    /// - XMPP server with native JID authentication only
    /// - Community features (waddles, channels, permissions)
    /// - WebSocket endpoint for XMPP
    /// - Health endpoints
    ///
    /// Disables:
    /// - ATProto OAuth routes
    /// - Device flow routes
    /// - XMPP OAuth routes
    /// - Auth page routes
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
    /// Parse server mode from a string.
    ///
    /// Valid values (case-insensitive):
    /// - "homeserver", "home", "full" -> HomeServer
    /// - "standalone", "xmpp", "xmpp-only" -> Standalone
    ///
    /// Any other value defaults to HomeServer.
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "standalone" | "xmpp" | "xmpp-only" => ServerMode::Standalone,
            "homeserver" | "home" | "full" | _ => ServerMode::HomeServer,
        }
    }

    /// Check if ATProto features should be enabled.
    pub fn atproto_enabled(&self) -> bool {
        matches!(self, ServerMode::HomeServer)
    }

    /// Check if this is standalone mode.
    pub fn is_standalone(&self) -> bool {
        matches!(self, ServerMode::Standalone)
    }
}

/// Server configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Server operating mode
    pub mode: ServerMode,
    /// Base URL for the server (used in OAuth redirects)
    pub base_url: String,
    /// Session encryption key
    pub session_key: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            mode: ServerMode::default(),
            base_url: "http://localhost:3000".to_string(),
            session_key: None,
        }
    }
}

impl ServerConfig {
    /// Load server configuration from environment variables.
    ///
    /// # Environment Variables
    ///
    /// - `WADDLE_MODE`: Server mode (`homeserver` or `standalone`). Default: `homeserver`
    /// - `WADDLE_BASE_URL`: Base URL for OAuth redirects. Default: `http://localhost:3000`
    /// - `WADDLE_SESSION_KEY`: Session encryption key (optional)
    pub fn from_env() -> Self {
        let mode_str = std::env::var("WADDLE_MODE").unwrap_or_else(|_| "homeserver".to_string());
        let mode = ServerMode::from_str(&mode_str);

        let base_url = std::env::var("WADDLE_BASE_URL")
            .unwrap_or_else(|_| "http://localhost:3000".to_string());

        let session_key = std::env::var("WADDLE_SESSION_KEY").ok();

        Self {
            mode,
            base_url,
            session_key,
        }
    }

    /// Log the current server configuration.
    pub fn log_config(&self) {
        info!("Running in {} mode", self.mode);
        info!("Base URL: {}", self.base_url);

        if self.mode.atproto_enabled() {
            info!("ATProto OAuth: enabled");
            info!("Device flow: enabled");
            info!("XMPP OAuth (XEP-0493): enabled");
        } else {
            info!("ATProto OAuth: disabled (standalone mode)");
            info!("Device flow: disabled (standalone mode)");
            info!("XMPP OAuth (XEP-0493): disabled (standalone mode)");
            info!("Native XMPP authentication: enabled");
        }
    }

    /// Create a test configuration.
    #[cfg(test)]
    pub fn test_homeserver() -> Self {
        Self {
            mode: ServerMode::HomeServer,
            base_url: "http://localhost:3000".to_string(),
            session_key: Some("test-key-32-bytes-long-for-aes!".to_string()),
        }
    }

    /// Create a test configuration for standalone mode.
    #[cfg(test)]
    pub fn test_standalone() -> Self {
        Self {
            mode: ServerMode::Standalone,
            base_url: "http://localhost:3000".to_string(),
            session_key: Some("test-key-32-bytes-long-for-aes!".to_string()),
        }
    }
}

/// Response structure for the /api/v1/server-info endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    /// Server version
    pub version: String,
    /// Server license
    pub license: String,
    /// Current server mode
    pub mode: ServerMode,
    /// Whether ATProto OAuth is enabled
    pub atproto_enabled: bool,
    /// Whether native XMPP authentication is available
    pub native_auth_available: bool,
    /// Features available in current mode
    pub features: ServerFeatures,
}

/// Features available based on server mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerFeatures {
    /// OAuth routes available
    pub oauth: bool,
    /// Device flow available
    pub device_flow: bool,
    /// XMPP OAuth (XEP-0493) available
    pub xmpp_oauth: bool,
    /// Web auth page available
    pub auth_page: bool,
    /// WebSocket endpoint available
    pub websocket: bool,
    /// Community features (waddles, channels) available
    pub communities: bool,
}

impl ServerInfo {
    /// Create server info from configuration and XMPP config.
    pub fn from_config(config: &ServerConfig, native_auth_enabled: bool) -> Self {
        let features = ServerFeatures {
            oauth: config.mode.atproto_enabled(),
            device_flow: config.mode.atproto_enabled(),
            xmpp_oauth: config.mode.atproto_enabled(),
            auth_page: config.mode.atproto_enabled(),
            websocket: true,   // Always available
            communities: true, // Always available
        };

        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            license: "AGPL-3.0".to_string(),
            mode: config.mode,
            atproto_enabled: config.mode.atproto_enabled(),
            native_auth_available: native_auth_enabled,
            features,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_mode_from_str() {
        // HomeServer variants
        assert_eq!(ServerMode::from_str("homeserver"), ServerMode::HomeServer);
        assert_eq!(ServerMode::from_str("HOMESERVER"), ServerMode::HomeServer);
        assert_eq!(ServerMode::from_str("home"), ServerMode::HomeServer);
        assert_eq!(ServerMode::from_str("full"), ServerMode::HomeServer);

        // Standalone variants
        assert_eq!(ServerMode::from_str("standalone"), ServerMode::Standalone);
        assert_eq!(ServerMode::from_str("STANDALONE"), ServerMode::Standalone);
        assert_eq!(ServerMode::from_str("xmpp"), ServerMode::Standalone);
        assert_eq!(ServerMode::from_str("xmpp-only"), ServerMode::Standalone);

        // Default to HomeServer for unknown values
        assert_eq!(ServerMode::from_str("unknown"), ServerMode::HomeServer);
        assert_eq!(ServerMode::from_str(""), ServerMode::HomeServer);
    }

    #[test]
    fn test_server_mode_atproto_enabled() {
        assert!(ServerMode::HomeServer.atproto_enabled());
        assert!(!ServerMode::Standalone.atproto_enabled());
    }

    #[test]
    fn test_server_mode_display() {
        assert_eq!(format!("{}", ServerMode::HomeServer), "HomeServer");
        assert_eq!(format!("{}", ServerMode::Standalone), "Standalone");
    }

    #[test]
    fn test_server_config_default() {
        let config = ServerConfig::default();
        assert_eq!(config.mode, ServerMode::HomeServer);
        assert_eq!(config.base_url, "http://localhost:3000");
        assert!(config.session_key.is_none());
    }

    #[test]
    fn test_server_info_homeserver_mode() {
        let config = ServerConfig::test_homeserver();
        let info = ServerInfo::from_config(&config, true);

        assert_eq!(info.mode, ServerMode::HomeServer);
        assert!(info.atproto_enabled);
        assert!(info.native_auth_available);
        assert!(info.features.oauth);
        assert!(info.features.device_flow);
        assert!(info.features.xmpp_oauth);
        assert!(info.features.auth_page);
        assert!(info.features.websocket);
        assert!(info.features.communities);
    }

    #[test]
    fn test_server_info_standalone_mode() {
        let config = ServerConfig::test_standalone();
        let info = ServerInfo::from_config(&config, true);

        assert_eq!(info.mode, ServerMode::Standalone);
        assert!(!info.atproto_enabled);
        assert!(info.native_auth_available);
        assert!(!info.features.oauth);
        assert!(!info.features.device_flow);
        assert!(!info.features.xmpp_oauth);
        assert!(!info.features.auth_page);
        assert!(info.features.websocket);
        assert!(info.features.communities);
    }

    #[test]
    fn test_server_mode_serialization() {
        assert_eq!(
            serde_json::to_string(&ServerMode::HomeServer).unwrap(),
            "\"homeserver\""
        );
        assert_eq!(
            serde_json::to_string(&ServerMode::Standalone).unwrap(),
            "\"standalone\""
        );
    }
}
