//! SASL authentication mechanisms.
//!
//! Implements SASL authentication for XMPP connections, including:
//! - SASL PLAIN (username/password or JID/token)
//! - SASL OAUTHBEARER (RFC 7628, XEP-0493)
//! - SASL SCRAM-SHA-256 (RFC 5802, RFC 7677)

pub mod extension_grant;
pub mod principal;
pub mod scram;

use jid::BareJid;

pub use extension_grant::{ExtensionGrantId, ExtensionGrantRef, ExtensionGrantScope};
pub use principal::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};
pub use scram::{
    encode_sasl_name, generate_salt, generate_scram_keys, ScramServer, ScramState,
    ServerFinalMessage, ServerFirstMessage, DEFAULT_ITERATIONS,
};

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
    pub fn parse(s: &str) -> Option<Self> {
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
    pub authzid: Option<BareJid>,
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

/// Invalid OAUTHBEARER framing or authorization identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OAuthBearerParseError {
    #[error("malformed OAUTHBEARER response")]
    Malformed,
    #[error("invalid OAUTHBEARER authorization identity")]
    InvalidAuthzid,
}

/// Parse OAUTHBEARER SASL data (RFC 7628).
///
/// OAUTHBEARER format (RFC 7628 Section 3.1):
/// ```text
/// kvsep = %x01
/// gs2-header = ("n" / "y") "," [authzid] ","
/// authzid = "a=" saslname
/// client-initial-response = gs2-header kvsep "auth=Bearer " token kvsep kvsep
/// ```
///
/// Example with token: `n,,\x01auth=Bearer TOKEN\x01\x01`
/// Empty request for discovery: `n,,\x01auth=Bearer \x01\x01` (or empty data)
///
/// Per XEP-0493:
/// - An empty initial response or empty bearer token is classified as a
///   discovery request; the caller determines the response
/// - Token data is validated against the session store
pub fn parse_oauthbearer(data: &[u8]) -> Result<OAuthBearerResult, OAuthBearerParseError> {
    if data.is_empty() {
        return Ok(OAuthBearerResult::DiscoveryRequest);
    }
    let value = std::str::from_utf8(data).map_err(|_| OAuthBearerParseError::Malformed)?;
    // Both `n` and `y` are unbound GS2 flags. A `p=` header requires channel
    // binding validation and must not be silently accepted here.
    let header = value
        .strip_prefix("n,")
        .or_else(|| value.strip_prefix("y,"))
        .ok_or(OAuthBearerParseError::Malformed)?;
    let (identity, rest) = header
        .split_once(',')
        .ok_or(OAuthBearerParseError::Malformed)?;
    let authzid = if identity.is_empty() {
        None
    } else {
        let encoded = identity
            .strip_prefix("a=")
            .ok_or(OAuthBearerParseError::InvalidAuthzid)?;
        Some(parse_oauth_authzid(encoded)?)
    };
    let fields = rest
        .strip_prefix('\x01')
        .and_then(|value| value.strip_suffix('\x01'))
        .ok_or(OAuthBearerParseError::Malformed)?;
    if !fields.is_empty() && !fields.ends_with('\x01') {
        return Err(OAuthBearerParseError::Malformed);
    }
    let mut token = None;
    for pair in fields.split_terminator('\x01') {
        let (key, value) = pair
            .split_once('=')
            .ok_or(OAuthBearerParseError::Malformed)?;
        if key.is_empty()
            || !key.bytes().all(|byte| byte.is_ascii_alphabetic())
            || !value
                .bytes()
                .all(|byte| matches!(byte, b'\t' | b'\r' | b'\n' | b' '..=b'~'))
        {
            return Err(OAuthBearerParseError::Malformed);
        }
        if key == "auth" {
            if token.is_some() {
                return Err(OAuthBearerParseError::Malformed);
            }
            let scheme = value.get(..6).ok_or(OAuthBearerParseError::Malformed)?;
            if !scheme.eq_ignore_ascii_case("Bearer") {
                return Err(OAuthBearerParseError::Malformed);
            }
            let bearer = value
                .get(6..)
                .and_then(|value| value.strip_prefix(' '))
                .ok_or(OAuthBearerParseError::Malformed)?
                .trim_start_matches(' ');
            let unpadded = bearer.trim_end_matches('=');
            if !bearer.is_empty()
                && (unpadded.is_empty()
                    || !unpadded.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric()
                            || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'+' | b'/')
                    }))
            {
                return Err(OAuthBearerParseError::Malformed);
            }
            token = Some(bearer);
        }
    }
    match token {
        Some("") => Ok(OAuthBearerResult::DiscoveryRequest),
        None => Err(OAuthBearerParseError::Malformed),
        Some(token) => Ok(OAuthBearerResult::Credentials(OAuthBearerCredentials {
            token: token.to_owned(),
            authzid,
        })),
    }
}

fn parse_oauth_authzid(encoded: &str) -> Result<BareJid, OAuthBearerParseError> {
    let mut decoded = String::with_capacity(encoded.len());
    let mut chars = encoded.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '=' => match (chars.next(), chars.next()) {
                (Some('2'), Some('C')) => decoded.push(','),
                (Some('3'), Some('D')) => decoded.push('='),
                _ => return Err(OAuthBearerParseError::InvalidAuthzid),
            },
            ch if ch.is_control() => return Err(OAuthBearerParseError::InvalidAuthzid),
            ch => decoded.push(ch),
        }
    }
    let jid = decoded
        .parse::<BareJid>()
        .map_err(|_| OAuthBearerParseError::InvalidAuthzid)?;
    if jid.node().is_none() {
        return Err(OAuthBearerParseError::InvalidAuthzid);
    }
    Ok(jid)
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
    fn test_parse_oauthbearer_rejects_truncated_gs2() {
        assert!(matches!(
            parse_oauthbearer(b"n,,"),
            Err(OAuthBearerParseError::Malformed)
        ));
    }

    #[test]
    fn test_parse_oauthbearer_discovery_minimal() {
        assert!(matches!(
            parse_oauthbearer(b"n,,\x01\x01"),
            Err(OAuthBearerParseError::Malformed)
        ));
        let explicit_empty = parse_oauthbearer(b"n,,\x01auth=Bearer \x01\x01").unwrap();
        assert!(matches!(
            explicit_empty,
            OAuthBearerResult::DiscoveryRequest
        ));
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
    fn oauthbearer_accepts_unbound_y_flag_and_multiple_bearer_spaces() {
        let result = parse_oauthbearer(b"y,,\x01auth=Bearer   test-token-123\x01\x01").unwrap();
        let OAuthBearerResult::Credentials(credentials) = result else {
            panic!("expected credentials");
        };
        assert_eq!(credentials.token, "test-token-123");
        assert!(credentials.authzid.is_none());
    }

    #[test]
    fn oauthbearer_scheme_is_case_insensitive() {
        let result = parse_oauthbearer(b"n,,\x01auth=bEaReR token\x01\x01").unwrap();
        let OAuthBearerResult::Credentials(credentials) = result else {
            panic!("expected credentials");
        };
        assert_eq!(credentials.token, "token");
    }

    #[test]
    fn test_parse_oauthbearer_with_authzid() {
        // With authzid: n,a=user@example.com,\x01auth=Bearer TOKEN\x01\x01
        let data = b"n,a=user@example.com,\x01auth=Bearer test-token-456\x01\x01";
        let result = parse_oauthbearer(data).unwrap();

        if let OAuthBearerResult::Credentials(creds) = result {
            assert_eq!(creds.token, "test-token-456");
            assert_eq!(creds.authzid, Some("user@example.com".parse().unwrap()));
        } else {
            panic!("Expected Credentials, got DiscoveryRequest");
        }
    }

    #[test]
    fn test_parse_oauthbearer_rejects_missing_bearer_space() {
        let data = b"n,,\x01auth=Bearertest-token-789\x01\x01";
        assert!(matches!(
            parse_oauthbearer(data),
            Err(OAuthBearerParseError::Malformed)
        ));
    }

    #[test]
    fn oauthbearer_saslname_decodes_comma_and_equals() {
        let result =
            parse_oauthbearer(b"n,a=al=2Cice=3Dtest@example.com,\x01auth=Bearer token\x01\x01")
                .unwrap();
        let OAuthBearerResult::Credentials(credentials) = result else {
            panic!("expected credentials");
        };
        assert_eq!(
            credentials.authzid,
            Some("al,ice=test@example.com".parse().unwrap())
        );
    }

    #[test]
    fn oauthbearer_rejects_invalid_authzids() {
        for value in [
            "",
            "example.com",
            "alice@example.com/device",
            "alice=ZZ@example.com",
            "alice=3@example.com",
            "alice example@example.com",
        ] {
            let data = format!("n,a={value},\x01auth=Bearer token\x01\x01");
            assert!(
                matches!(
                    parse_oauthbearer(data.as_bytes()),
                    Err(OAuthBearerParseError::InvalidAuthzid)
                ),
                "{value:?}"
            );
        }
    }

    #[test]
    fn oauthbearer_requires_exact_framing_and_utf8() {
        let cases: &[&[u8]] = &[
            b"\xff",
            b"n,",
            b"n,,",
            b"n,,\x01",
            b"n,,\x01auth=Bearer token\x01",
            b"n,a=user@example.com\x01auth=Bearer token\x01\x01",
            b"n,,\x01auth=Bearertoken\x01\x01",
            b"n,,\x01auth=Bearer token token\x01\x01",
            b"n,,\x01auth=Bearer to=ken\x01\x01",
            b"n,,\x01auth=Bearer token\x01auth=Bearer other\x01\x01",
            b"n,,\x01host=example.com\x01\x01",
        ];
        for data in cases {
            assert!(parse_oauthbearer(data).is_err(), "{data:?}");
        }
    }

    #[test]
    fn test_sasl_mechanism_parse() {
        assert_eq!(SaslMechanism::parse("PLAIN"), Some(SaslMechanism::Plain));
        assert_eq!(SaslMechanism::parse("plain"), Some(SaslMechanism::Plain));
        assert_eq!(
            SaslMechanism::parse("OAUTHBEARER"),
            Some(SaslMechanism::OAuthBearer)
        );
        assert_eq!(
            SaslMechanism::parse("oauthbearer"),
            Some(SaslMechanism::OAuthBearer)
        );
        assert_eq!(
            SaslMechanism::parse("SCRAM-SHA-256"),
            Some(SaslMechanism::ScramSha256)
        );
        assert_eq!(
            SaslMechanism::parse("scram-sha-256"),
            Some(SaslMechanism::ScramSha256)
        );
        assert_eq!(SaslMechanism::parse("UNKNOWN"), None);
    }

    #[test]
    fn test_sasl_mechanism_display() {
        assert_eq!(SaslMechanism::Plain.to_string(), "PLAIN");
        assert_eq!(SaslMechanism::OAuthBearer.to_string(), "OAUTHBEARER");
        assert_eq!(SaslMechanism::ScramSha256.to_string(), "SCRAM-SHA-256");
    }
}
