//! SASL authentication mechanisms.
//!
//! Implements SASL authentication for XMPP connections, including:
//! - SASL PLAIN (username/password or JID/token)
//! - Custom ATProto token authentication (future)

use jid::BareJid;

use crate::XmppError;

/// SASL authentication mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaslMechanism {
    /// PLAIN mechanism (RFC 4616)
    Plain,
}

impl std::fmt::Display for SaslMechanism {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaslMechanism::Plain => write!(f, "PLAIN"),
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
}
