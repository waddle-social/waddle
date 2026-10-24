//! Validated endpoints, credentials, session resources, and stream-management settings.

use std::fmt;
use std::time::Duration;

use jid::BareJid;
use url::Url;
use waddle_xmpp_core::ConnectionConfig;

use crate::error::{ClientError, ClientResult};
use crate::stream_management::SmResumeState;

/// Immutable client bootstrap configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientConfig {
    /// XMPP server identity used by this client.
    pub connection: ConnectionConfig,
    /// WebSocket endpoint, origin, and connection timeout settings.
    pub transport: WebSocketConfig,
    /// Authentication credentials and mechanism selection.
    pub auth: AuthenticationConfig,
    /// Session resource and stream-management settings.
    pub session: SessionConfig,
}

impl ClientConfig {
    /// Create and validate an OAUTHBEARER client with default session and stream-management settings.
    ///
    /// # Errors
    ///
    /// Returns a validation error if the endpoint is not `ws`/`wss`, lacks a host, the origin is not `http`/`https`, or the client resource is blank.
    pub fn new(
        connection: ConnectionConfig,
        transport: WebSocketConfig,
        auth: OAuthBearerConfig,
    ) -> ClientResult<Self> {
        let config = Self {
            connection,
            transport,
            auth: AuthenticationConfig::OAuthBearer(auth),
            session: SessionConfig::default(),
        };
        config.validate()?;
        Ok(config)
    }

    /// Check this configuration's endpoint, resource, and authentication invariants.
    ///
    /// # Errors
    ///
    /// Returns a validation error if the endpoint is not `ws`/`wss`, lacks a host, the origin is not `http`/`https`, or the client resource is blank.
    pub fn validate(&self) -> ClientResult<()> {
        self.transport.validate()?;
        self.auth.validate()?;
        Ok(())
    }
}

/// v1 auth boundary: OAUTHBEARER only.
#[derive(Debug, Clone, PartialEq)]
pub enum AuthenticationConfig {
    /// SASL OAUTHBEARER credentials and the requested account resource.
    OAuthBearer(OAuthBearerConfig),
}

impl AuthenticationConfig {
    /// Check this configuration's endpoint, resource, and authentication invariants.
    ///
    /// # Errors
    ///
    /// Returns the selected authentication configuration's resource validation error.
    pub fn validate(&self) -> ClientResult<()> {
        match self {
            Self::OAuthBearer(config) => config.validate(),
        }
    }
}

/// WebSocket-only transport settings for the native client runtime.
#[derive(Debug, Clone, PartialEq)]
pub struct WebSocketConfig {
    /// WebSocket URL used to establish the XMPP transport.
    pub endpoint: Url,
    /// HTTP Origin header supplied when opening the WebSocket connection.
    pub origin: Option<Url>,
    /// Maximum time allowed to establish the WebSocket connection.
    pub connect_timeout: Duration,
}

impl WebSocketConfig {
    /// Validate a WebSocket endpoint and use no origin with a 15-second connection timeout.
    ///
    /// # Errors
    ///
    /// Returns an error if the endpoint is not `ws`/`wss`, lacks a host, or the optional origin is not `http`/`https`.
    pub fn new(endpoint: Url) -> ClientResult<Self> {
        let config = Self {
            endpoint,
            origin: None,
            connect_timeout: Duration::from_secs(15),
        };
        config.validate()?;
        Ok(config)
    }

    /// Check this configuration's endpoint, resource, and authentication invariants.
    ///
    /// # Errors
    ///
    /// Returns an error if the endpoint is not `ws`/`wss`, lacks a host, or the optional origin is not `http`/`https`.
    pub fn validate(&self) -> ClientResult<()> {
        match self.endpoint.scheme() {
            "ws" | "wss" => {}
            other => {
                return Err(ClientError::InvalidTransportScheme {
                    scheme: other.to_string(),
                })
            }
        }

        if self.endpoint.host_str().is_none() {
            return Err(ClientError::MissingWebSocketHost);
        }

        if let Some(origin) = &self.origin {
            match origin.scheme() {
                "http" | "https" => {}
                other => {
                    return Err(ClientError::InvalidTransportScheme {
                        scheme: other.to_string(),
                    })
                }
            }
        }

        Ok(())
    }
}

/// OAUTHBEARER credentials and bind target for v1.
#[derive(Debug, Clone, PartialEq)]
pub struct OAuthBearerConfig {
    /// Bare JID of the account being authenticated.
    pub account: BareJid,
    /// Requested XMPP resource name for account binding.
    pub resource: ClientResource,
    /// Bearer credential used during OAUTHBEARER authentication.
    pub access_token: AccessToken,
}

impl OAuthBearerConfig {
    /// Create authentication credentials and validate the requested resource name.
    ///
    /// # Errors
    ///
    /// Returns the resource validation error if the requested resource is blank.
    pub fn new(
        account: BareJid,
        resource: ClientResource,
        access_token: AccessToken,
    ) -> ClientResult<Self> {
        let config = Self {
            account,
            resource,
            access_token,
        };
        config.validate()?;
        Ok(config)
    }

    /// Check this configuration's endpoint, resource, and authentication invariants.
    ///
    /// # Errors
    ///
    /// Returns the resource validation error if the requested resource is blank.
    pub fn validate(&self) -> ClientResult<()> {
        self.resource.validate()
    }
}

/// Human-controlled resource name requested during bind.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClientResource(String);

impl ClientResource {
    /// Validate a non-blank resource name while retaining its original text.
    ///
    /// # Errors
    ///
    /// Returns `ClientError::EmptyResource` if the requested resource is blank after trimming.
    pub fn new(resource: impl Into<String>) -> ClientResult<Self> {
        let resource = Self(resource.into());
        resource.validate()?;
        Ok(resource)
    }

    /// Borrow the stored text without allocating.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Check this configuration's endpoint, resource, and authentication invariants.
    ///
    /// # Errors
    ///
    /// Returns `ClientError::EmptyResource` if the requested resource is blank after trimming.
    pub fn validate(&self) -> ClientResult<()> {
        if self.0.trim().is_empty() {
            return Err(ClientError::EmptyResource);
        }

        Ok(())
    }
}

impl fmt::Display for ClientResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Secret bearer token material.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct AccessToken(String);

impl AccessToken {
    /// Wrap bearer token material without exposing it in Debug output.
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// Borrow the stored text without allocating.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessToken(REDACTED)")
    }
}

/// Session-scoped bootstrap flags owned by the client runtime.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionConfig {
    /// Whether the server advertised XEP-0198 support.
    pub stream_management: StreamManagementConfig,
    /// Language tag associated with the displayed text.
    pub language: Option<String>,
}

/// Stream management feature flags reserved for future parity slices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamManagementConfig {
    /// Whether to negotiate XEP-0198 after binding.
    pub enable_stream_management: bool,
    /// Whether the client requests stream resumption.
    pub allow_resume: bool,
    /// Requested maximum stream resumption window in seconds.
    pub resume_max: Option<u32>,
    /// Persisted XEP-0198 counters and unhandled outbound entries.
    pub resume_state: Option<SmResumeState>,
}

impl Default for StreamManagementConfig {
    fn default() -> Self {
        Self {
            enable_stream_management: true,
            allow_resume: true,
            resume_max: Some(300),
            resume_state: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn websocket_config_rejects_non_websocket_schemes() {
        let endpoint = Url::parse("https://chat.example.com/ws").unwrap();
        let error = WebSocketConfig::new(endpoint).unwrap_err();
        assert!(matches!(
            error,
            ClientError::InvalidTransportScheme { scheme } if scheme == "https"
        ));
    }

    #[test]
    fn client_config_validates_successfully() {
        let connection = ConnectionConfig::new(BareJid::from_str("waddle.example").unwrap());
        let transport =
            WebSocketConfig::new(Url::parse("wss://chat.example.com/ws").unwrap()).unwrap();
        let auth = OAuthBearerConfig::new(
            BareJid::from_str("alice@example.com").unwrap(),
            ClientResource::new("phone").unwrap(),
            AccessToken::new("token"),
        )
        .unwrap();

        let config = ClientConfig::new(connection, transport, auth).unwrap();
        assert!(matches!(config.auth, AuthenticationConfig::OAuthBearer(_)));
    }
}
