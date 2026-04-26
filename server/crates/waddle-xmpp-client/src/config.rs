use std::fmt;
use std::time::Duration;

use jid::BareJid;
use url::Url;
use waddle_xmpp_core::ConnectionConfig;

use crate::error::{ClientError, ClientResult};

/// Immutable client bootstrap configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientConfig {
    pub connection: ConnectionConfig,
    pub transport: WebSocketConfig,
    pub auth: AuthenticationConfig,
    pub session: SessionConfig,
}

impl ClientConfig {
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

    pub fn validate(&self) -> ClientResult<()> {
        self.transport.validate()?;
        self.auth.validate()?;
        Ok(())
    }
}

/// v1 auth boundary: OAUTHBEARER only.
#[derive(Debug, Clone, PartialEq)]
pub enum AuthenticationConfig {
    OAuthBearer(OAuthBearerConfig),
}

impl AuthenticationConfig {
    pub fn validate(&self) -> ClientResult<()> {
        match self {
            Self::OAuthBearer(config) => config.validate(),
        }
    }
}

/// WebSocket-only transport settings for the native client runtime.
#[derive(Debug, Clone, PartialEq)]
pub struct WebSocketConfig {
    pub endpoint: Url,
    pub origin: Option<Url>,
    pub connect_timeout: Duration,
}

impl WebSocketConfig {
    pub fn new(endpoint: Url) -> ClientResult<Self> {
        let config = Self {
            endpoint,
            origin: None,
            connect_timeout: Duration::from_secs(15),
        };
        config.validate()?;
        Ok(config)
    }

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
    pub account: BareJid,
    pub resource: ClientResource,
    pub access_token: AccessToken,
}

impl OAuthBearerConfig {
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

    pub fn validate(&self) -> ClientResult<()> {
        self.resource.validate()
    }
}

/// Human-controlled resource name requested during bind.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClientResource(String);

impl ClientResource {
    pub fn new(resource: impl Into<String>) -> ClientResult<Self> {
        let resource = Self(resource.into());
        resource.validate()?;
        Ok(resource)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

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
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

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
    pub stream_management: StreamManagementConfig,
    pub language: Option<String>,
}

/// Stream management feature flags reserved for future parity slices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamManagementConfig {
    pub enable_stream_management: bool,
    pub allow_resume: bool,
}

impl Default for StreamManagementConfig {
    fn default() -> Self {
        Self {
            enable_stream_management: true,
            allow_resume: true,
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
