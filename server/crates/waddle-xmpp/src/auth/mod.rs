//! SASL authentication mechanisms.
//!
//! Implements SASL authentication for XMPP connections, including:
//! - SASL PLAIN (username/password or JID/token)
//! - SASL OAUTHBEARER (RFC 7628)
//! - SASL SCRAM-SHA-256 (RFC 5802, RFC 7677)

pub mod oauthbearer;
pub mod scram;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use jid::BareJid;
use thiserror::Error;

pub use oauthbearer::{
    parse_oauthbearer, OAuthBearerCredentials, OAuthBearerParseError, OAuthBearerResult,
};
pub use scram::{
    encode_sasl_name, generate_salt, generate_scram_keys, ScramFinalError, ScramServer, ScramState,
    ServerFinalMessage, ServerFirstMessage, DEFAULT_ITERATIONS,
};

use crate::XmppError;

/// SASL authentication mechanism.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaslMechanism {
    /// PLAIN mechanism (RFC 4616)
    Plain,
    /// OAUTHBEARER mechanism (RFC 7628)
    OAuthBearer,
    /// SCRAM-SHA-256 mechanism (RFC 5802, RFC 7677)
    ScramSha256,
    /// A syntactically valid but unsupported mechanism, with the attacker
    /// supplied wire name discarded at the XML boundary.
    Unsupported,
}

impl SaslMechanism {
    /// Parse a mechanism name string into a SaslMechanism.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "PLAIN" => Some(SaslMechanism::Plain),
            "OAUTHBEARER" => Some(SaslMechanism::OAuthBearer),
            "SCRAM-SHA-256" => Some(SaslMechanism::ScramSha256),
            _ => None,
        }
    }

    /// Classify a mechanism name received from the wire without allowing the
    /// untyped string to flow beyond the XML parser boundary.
    pub fn from_wire_name(name: &str) -> Self {
        Self::parse(name).unwrap_or(Self::Unsupported)
    }
}

impl std::fmt::Display for SaslMechanism {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaslMechanism::Plain => write!(f, "PLAIN"),
            SaslMechanism::OAuthBearer => write!(f, "OAUTHBEARER"),
            SaslMechanism::ScramSha256 => write!(f, "SCRAM-SHA-256"),
            SaslMechanism::Unsupported => f.write_str("UNSUPPORTED"),
        }
    }
}

/// Invalid base64 at the SASL XML boundary.
#[derive(Debug, Error)]
#[error("invalid SASL base64 payload")]
pub struct InvalidSaslPayload(#[source] base64::DecodeError);

fn decode_sasl_payload(encoded: &str) -> Result<Vec<u8>, InvalidSaslPayload> {
    let encoded = encoded.trim();
    if encoded.is_empty() || encoded == "=" {
        return Ok(Vec::new());
    }
    BASE64_STANDARD.decode(encoded).map_err(InvalidSaslPayload)
}

/// Decoded SASL initial response carried by `<auth/>` or SASL2
/// `<initial-response/>`.
#[derive(Clone, PartialEq, Eq)]
pub struct SaslInitialResponse(Vec<u8>);

impl std::fmt::Debug for SaslInitialResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SaslInitialResponse")
            .field("len", &self.0.len())
            .field("payload", &"<redacted>")
            .finish()
    }
}

impl SaslInitialResponse {
    pub const fn empty() -> Self {
        Self(Vec::new())
    }

    pub fn decode(encoded: &str) -> Result<Self, InvalidSaslPayload> {
        decode_sasl_payload(encoded).map(Self)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Decoded SASL continuation response carried by `<response/>`.
#[derive(Clone, PartialEq, Eq)]
pub struct SaslResponsePayload(Vec<u8>);

impl std::fmt::Debug for SaslResponsePayload {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SaslResponsePayload")
            .field("len", &self.0.len())
            .field("payload", &"<redacted>")
            .finish()
    }
}

impl SaslResponsePayload {
    pub fn decode(encoded: &str) -> Result<Self, InvalidSaslPayload> {
        decode_sasl_payload(encoded).map(Self)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Parsed SASL credentials.
#[derive(Debug, Clone)]
pub struct SaslCredentials {
    /// Authentication identity (JID)
    pub authcid: BareJid,
    /// Password/token
    pub password: String,
    /// Optional authorization identity
    pub authzid: Option<String>,
}

/// Parse SASL PLAIN credentials.
///
/// SASL PLAIN format: `authzid \0 authcid \0 password`
/// For our use case: `\0 jid \0 token` (authzid empty)
pub fn parse_plain(data: &[u8]) -> Result<SaslCredentials, XmppError> {
    let parts: Vec<&[u8]> = data.split(|&b| b == 0).collect();

    if parts.len() < 2 {
        return Err(XmppError::auth_failed("Invalid SASL PLAIN format"));
    }

    let (authzid, authcid_bytes, password_bytes) = if parts.len() == 3 {
        let authzid = if parts[0].is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(parts[0]).to_string())
        };
        (authzid, parts[1], parts[2])
    } else {
        (None, parts[0], parts[1])
    };

    let authcid_str = String::from_utf8_lossy(authcid_bytes);
    let authcid: BareJid = authcid_str
        .parse()
        .map_err(|e| XmppError::auth_failed(format!("Invalid JID: {}", e)))?;

    let password = String::from_utf8_lossy(password_bytes).to_string();

    Ok(SaslCredentials {
        authcid,
        password,
        authzid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_plain_simple() {
        let data = b"\0user@domain.com\0password123";
        let creds = parse_plain(data).unwrap();
        assert_eq!(creds.authcid.to_string(), "user@domain.com");
        assert_eq!(creds.password, "password123");
        assert!(creds.authzid.is_none());
    }

    #[test]
    fn test_parse_plain_with_authzid() {
        let data = b"admin\0user@domain.com\0password123";
        let creds = parse_plain(data).unwrap();
        assert_eq!(creds.authcid.to_string(), "user@domain.com");
        assert_eq!(creds.password, "password123");
        assert_eq!(creds.authzid, Some("admin".to_string()));
    }

    // OAUTHBEARER tests

    #[test]
    fn test_parse_oauthbearer_empty_credentials() {
        let result = parse_oauthbearer(b"n,,\x01auth=\x01\x01").unwrap();
        assert!(matches!(result, OAuthBearerResult::EmptyCredentials));
    }

    #[test]
    fn test_parse_oauthbearer_with_token() {
        // Standard format: n,,\x01auth=Bearer TOKEN\x01\x01
        let data = b"n,,\x01auth=Bearer test-token-123\x01\x01";
        let result = parse_oauthbearer(data).unwrap();

        if let OAuthBearerResult::Credentials(creds) = result {
            assert_eq!(creds.token().expose_secret(), "test-token-123");
            assert!(creds.authorization_identity().is_none());
        } else {
            panic!("expected OAUTHBEARER credentials");
        }
    }

    #[test]
    fn test_parse_oauthbearer_with_authzid() {
        // With authzid: n,a=user@example.com,\x01auth=Bearer TOKEN\x01\x01
        let data = b"n,a=user@example.com,\x01auth=Bearer test-token-456\x01\x01";
        let result = parse_oauthbearer(data).unwrap();

        if let OAuthBearerResult::Credentials(creds) = result {
            assert_eq!(creds.token().expose_secret(), "test-token-456");
            assert_eq!(
                creds.authorization_identity().map(ToString::to_string),
                Some("user@example.com".to_string())
            );
        } else {
            panic!("expected OAUTHBEARER credentials");
        }
    }

    #[test]
    fn test_parse_oauthbearer_no_space_after_bearer() {
        let data = b"n,,\x01auth=Bearertest-token-789\x01\x01";
        assert_eq!(
            parse_oauthbearer(data).unwrap_err(),
            OAuthBearerParseError::UnsupportedAuthorizationScheme
        );
    }

    #[test]
    fn test_sasl_mechanism_parse() {
        assert_eq!(SaslMechanism::parse("PLAIN"), Some(SaslMechanism::Plain));
        assert_eq!(SaslMechanism::parse("plain"), None);
        assert_eq!(
            SaslMechanism::parse("OAUTHBEARER"),
            Some(SaslMechanism::OAuthBearer)
        );
        assert_eq!(SaslMechanism::parse("oauthbearer"), None);
        assert_eq!(
            SaslMechanism::parse("SCRAM-SHA-256"),
            Some(SaslMechanism::ScramSha256)
        );
        assert_eq!(SaslMechanism::parse("scram-sha-256"), None);
        assert_eq!(SaslMechanism::parse("UNKNOWN"), None);
    }

    #[test]
    fn test_sasl_mechanism_display() {
        assert_eq!(SaslMechanism::Plain.to_string(), "PLAIN");
        assert_eq!(SaslMechanism::OAuthBearer.to_string(), "OAUTHBEARER");
        assert_eq!(SaslMechanism::ScramSha256.to_string(), "SCRAM-SHA-256");
    }

    #[test]
    fn typed_sasl_payload_debug_is_redacted() {
        let initial = SaslInitialResponse(b"Bearer session-secret".to_vec());
        let response = SaslResponsePayload(b"session-secret".to_vec());
        for debug in [format!("{initial:?}"), format!("{response:?}")] {
            assert!(debug.contains("<redacted>"));
            assert!(!debug.contains("session-secret"));
        }
    }
}
