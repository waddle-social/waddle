//! SASL authentication mechanisms.
//!
//! Implements SASL authentication for XMPP connections, including:
//! - SASL PLAIN (username/password or JID/token)
//! - SASL OAUTHBEARER (RFC 7628, XEP-0493)
//! - SASL SCRAM-SHA-256 (RFC 5802, RFC 7677)

pub mod scram;

use jid::BareJid;

pub use scram::{generate_salt, generate_scram_keys, encode_sasl_name, ScramServer, ScramState, ServerFirstMessage, ServerFinalMessage, DEFAULT_ITERATIONS};

use crate::XmppError;

/// SASL authentication mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaslMechanism {
    /// PLAIN mechanism (RFC 4616)
    Plain,
    /// OAUTHBEARER mechanism (RFC 7628, XEP-0493)
    OAuthBearer,
    /// SCRAM-SHA-256 mechanism (RFC 5802, RFC 7677)
    ScramSha256,
}

impl SaslMechanism {
    /// Parse a mechanism name string into a SaslMechanism.
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "PLAIN" => Some(SaslMechanism::Plain),
            "OAUTHBEARER" => Some(SaslMechanism::OAuthBearer),
            "SCRAM-SHA-256" => Some(SaslMechanism::ScramSha256),
            _ => None,
        }
    }
}

impl std::fmt::Display for SaslMechanism {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaslMechanism::Plain => write!(f, "PLAIN"),
            SaslMechanism::OAuthBearer => write!(f, "OAUTHBEARER"),
            SaslMechanism::ScramSha256 => write!(f, "SCRAM-SHA-256"),
        }
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

/// Parsed OAUTHBEARER credentials.
#[derive(Debug, Clone)]
pub struct OAuthBearerCredentials {
    /// The OAuth bearer token
    pub token: String,
    /// Optional authorization identity (authzid)
    pub authzid: Option<String>,
}

/// Result of parsing OAUTHBEARER SASL data.
#[derive(Debug, Clone)]
pub enum OAuthBearerResult {
    /// Client sent an empty/discovery request
    /// Per XEP-0493 §3.2, server should respond with discovery URL
    DiscoveryRequest,
    /// Client sent valid credentials with a token
    Credentials(OAuthBearerCredentials),
}

/// Parse OAUTHBEARER SASL data (RFC 7628).
///
/// OAUTHBEARER format (RFC 7628 Section 3.1):
/// ```text
/// kvsep = %x01
/// gs2-header = "n,," / ("n," authzid ",")
/// authzid = "a=" saslname
/// client-initial-response = gs2-header kvsep "auth=Bearer " token kvsep kvsep
/// ```
///
/// Example with token: `n,,\x01auth=Bearer TOKEN\x01\x01`
/// Empty request for discovery: `n,,\x01\x01` (or just empty data)
///
/// Per XEP-0493:
/// - Empty or minimal data triggers OAuth discovery response
/// - Token data is validated against the session store
pub fn parse_oauthbearer(data: &[u8]) -> Result<OAuthBearerResult, XmppError> {
    // Empty data or just "n,," means discovery request
    if data.is_empty() {
        return Ok(OAuthBearerResult::DiscoveryRequest);
    }

    let data_str = String::from_utf8_lossy(data);

    // Check for minimal/discovery request patterns
    // XEP-0493 specifies sending empty OAUTHBEARER or just GS2 header for discovery
    // Common patterns: "", "n,,\x01\x01", "n,,"
    if data_str.trim().is_empty()
        || data_str == "n,,"
        || data_str == "n,,\x01\x01"
        || data_str == "n,,\x01"
    {
        return Ok(OAuthBearerResult::DiscoveryRequest);
    }

    // Parse GS2 header to extract optional authzid
    let mut authzid = None;
    let mut rest = data_str.as_ref();

    // GS2 header starts with "n," optionally followed by "a=authzid"
    if let Some(stripped) = rest.strip_prefix("n,") {
        rest = stripped;

        // Check for authzid (a=...)
        if let Some(stripped) = rest.strip_prefix("a=") {
            // Find the end of authzid (comma)
            if let Some(comma_pos) = stripped.find(',') {
                authzid = Some(stripped[..comma_pos].to_string());
                rest = &stripped[comma_pos + 1..];
            }
        } else if let Some(stripped) = rest.strip_prefix(',') {
            // Empty authzid case: "n,,"
            rest = stripped;
        }
    }

    // Now parse the key-value pairs separated by \x01
    let parts: Vec<&str> = rest.split('\x01').collect();

    // Look for auth=Bearer token
    for part in parts {
        if let Some(token_part) = part.strip_prefix("auth=Bearer ") {
            let token = token_part.trim().to_string();
            if !token.is_empty() {
                return Ok(OAuthBearerResult::Credentials(OAuthBearerCredentials {
                    token,
                    authzid,
                }));
            }
        }
        // Also handle "auth=Bearer" without space (some clients)
        if let Some(token_part) = part.strip_prefix("auth=Bearer") {
            let token = token_part.trim().to_string();
            if !token.is_empty() {
                return Ok(OAuthBearerResult::Credentials(OAuthBearerCredentials {
                    token,
                    authzid,
                }));
            }
        }
    }

    // No token found - treat as discovery request
    Ok(OAuthBearerResult::DiscoveryRequest)
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
    fn test_parse_oauthbearer_empty() {
        let result = parse_oauthbearer(b"").unwrap();
        assert!(matches!(result, OAuthBearerResult::DiscoveryRequest));
    }

    #[test]
    fn test_parse_oauthbearer_discovery_gs2_only() {
        let result = parse_oauthbearer(b"n,,").unwrap();
        assert!(matches!(result, OAuthBearerResult::DiscoveryRequest));
    }

    #[test]
    fn test_parse_oauthbearer_discovery_minimal() {
        let result = parse_oauthbearer(b"n,,\x01\x01").unwrap();
        assert!(matches!(result, OAuthBearerResult::DiscoveryRequest));
    }

    #[test]
    fn test_parse_oauthbearer_with_token() {
        // Standard format: n,,\x01auth=Bearer TOKEN\x01\x01
        let data = b"n,,\x01auth=Bearer test-token-123\x01\x01";
        let result = parse_oauthbearer(data).unwrap();

        if let OAuthBearerResult::Credentials(creds) = result {
            assert_eq!(creds.token, "test-token-123");
            assert!(creds.authzid.is_none());
        } else {
            panic!("Expected Credentials, got DiscoveryRequest");
        }
    }

    #[test]
    fn test_parse_oauthbearer_with_authzid() {
        // With authzid: n,a=user@example.com,\x01auth=Bearer TOKEN\x01\x01
        let data = b"n,a=user@example.com,\x01auth=Bearer test-token-456\x01\x01";
        let result = parse_oauthbearer(data).unwrap();

        if let OAuthBearerResult::Credentials(creds) = result {
            assert_eq!(creds.token, "test-token-456");
            assert_eq!(creds.authzid, Some("user@example.com".to_string()));
        } else {
            panic!("Expected Credentials, got DiscoveryRequest");
        }
    }

    #[test]
    fn test_parse_oauthbearer_no_space_after_bearer() {
        // Some clients don't include the space after "Bearer"
        let data = b"n,,\x01auth=Bearertest-token-789\x01\x01";
        let result = parse_oauthbearer(data).unwrap();

        if let OAuthBearerResult::Credentials(creds) = result {
            assert_eq!(creds.token, "test-token-789");
        } else {
            panic!("Expected Credentials, got DiscoveryRequest");
        }
    }

    #[test]
    fn test_sasl_mechanism_from_str() {
        assert_eq!(SaslMechanism::from_str("PLAIN"), Some(SaslMechanism::Plain));
        assert_eq!(SaslMechanism::from_str("plain"), Some(SaslMechanism::Plain));
        assert_eq!(SaslMechanism::from_str("OAUTHBEARER"), Some(SaslMechanism::OAuthBearer));
        assert_eq!(SaslMechanism::from_str("oauthbearer"), Some(SaslMechanism::OAuthBearer));
        assert_eq!(SaslMechanism::from_str("SCRAM-SHA-256"), Some(SaslMechanism::ScramSha256));
        assert_eq!(SaslMechanism::from_str("scram-sha-256"), Some(SaslMechanism::ScramSha256));
        assert_eq!(SaslMechanism::from_str("UNKNOWN"), None);
    }

    #[test]
    fn test_sasl_mechanism_display() {
        assert_eq!(SaslMechanism::Plain.to_string(), "PLAIN");
        assert_eq!(SaslMechanism::OAuthBearer.to_string(), "OAUTHBEARER");
        assert_eq!(SaslMechanism::ScramSha256.to_string(), "SCRAM-SHA-256");
    }
}
