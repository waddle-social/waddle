//! XEP-0493: OAuth Client Login
//!
//! Waddle offers the OAUTHBEARER SASL mechanism (RFC 7628) and an
//! authorization-code-with-PKCE OAuth provider (RFC 6749, RFC 7636) so that an
//! account owner can grant a client access without sharing a password.
//!
//! This module holds the protocol vocabulary shared by the SASL layer and the
//! HTTP provider:
//!
//! - the scope names XEP-0493 defines (§ OAuth Scopes), of which the server
//!   MUST support `xmpp:client:normal`;
//! - the RFC 7628 §3.2.2 error document a server sends as a SASL challenge
//!   when authentication fails. For a client's empty-token discovery request
//!   (XEP-0493 § Discovering OAuth support) the document carries the URL of
//!   the RFC 8414 discovery document in `openid-configuration`.
//!
//! Managing and revoking the grants is XEP-0494 (`xep0494`).

use serde::Serialize;

/// The scope that gives a client general access to the account but not to its
/// security-relevant features (XEP-0493 § OAuth Scopes).
pub const SCOPE_CLIENT_NORMAL: &str = "xmpp:client:normal";

/// Path of the RFC 8414 authorization server metadata document.
pub const DISCOVERY_PATH: &str = "/.well-known/oauth-authorization-server";

/// URL of the RFC 8414 metadata document of the provider served at
/// `base_url`. This is what a server returns in `openid-configuration`
/// (XEP-0493 § Servers).
pub fn discovery_url(base_url: &str) -> String {
    format!("{}{DISCOVERY_PATH}", base_url.trim_end_matches('/'))
}

/// A scope this server grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize, Serialize)]
pub enum OAuthScope {
    /// `xmpp:client:normal`: most features of the account, without managing
    /// the account's access (XEP-0494) or changing its password.
    ClientNormal,
}

impl OAuthScope {
    /// The scope name used on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClientNormal => SCOPE_CLIENT_NORMAL,
        }
    }

    /// Every scope the server supports, as advertised in its metadata.
    pub const SUPPORTED: &'static [OAuthScope] = &[Self::ClientNormal];

    /// Whether a client holding only this scope may list and revoke the
    /// account's access grants (XEP-0494). `xmpp:client:normal` may not:
    /// "such a client cannot escalate its privileges or take over an
    /// account" (XEP-0493 § OAuth Scopes).
    pub const fn manages_account_access(self) -> bool {
        match self {
            Self::ClientNormal => false,
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::SUPPORTED
            .iter()
            .copied()
            .find(|scope| scope.as_str() == name)
    }
}

/// An authorization request named a scope this server does not grant.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unsupported scope")]
pub struct UnsupportedScope;

/// Parse the space-delimited `scope` parameter of an authorization request
/// (RFC 6749 §3.3). An absent or blank parameter selects the default scope,
/// `xmpp:client:normal`. Duplicates collapse; any unknown name is refused so
/// a client never receives less than it asked for without being told.
pub fn parse_scope_request(raw: Option<&str>) -> Result<Vec<OAuthScope>, UnsupportedScope> {
    let mut scopes = Vec::new();
    for name in raw.unwrap_or_default().split_ascii_whitespace() {
        let scope = OAuthScope::from_name(name).ok_or(UnsupportedScope)?;
        if !scopes.contains(&scope) {
            scopes.push(scope);
        }
    }
    if scopes.is_empty() {
        scopes.push(OAuthScope::ClientNormal);
    }
    Ok(scopes)
}

/// Render granted scopes as the space-delimited `scope` value of RFC 6749.
pub fn scope_value(scopes: &[OAuthScope]) -> String {
    scopes
        .iter()
        .map(|scope| scope.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse the stored space-delimited scope value of a grant.
pub fn parse_scope_value(value: &str) -> Result<Vec<OAuthScope>, UnsupportedScope> {
    if value.split_ascii_whitespace().next().is_none() {
        return Err(UnsupportedScope);
    }
    parse_scope_request(Some(value))
}

/// The `status` of an RFC 7628 §3.2.2 error document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuthBearerStatus {
    /// The client response was malformed.
    InvalidRequest,
    /// The token is absent, unknown, expired or revoked. This is the status
    /// of a discovery request, which carries an empty token.
    InvalidToken,
    /// The server could not decide.
    ServerError,
}

/// The JSON document of an RFC 7628 §3.2.2 error challenge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OAuthBearerError<'a> {
    status: OAuthBearerStatus,
    /// The scope a token must carry (RFC 7628 §3.2.2).
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    /// URL of the RFC 8414 authorization server metadata (XEP-0493 §
    /// Servers: servers MUST provide it in `openid-configuration`).
    #[serde(rename = "openid-configuration")]
    openid_configuration: &'a str,
}

impl<'a> OAuthBearerError<'a> {
    /// An error document that points the client at the discovery document
    /// and names the scope it should request.
    pub fn new(status: OAuthBearerStatus, discovery_url: &'a str) -> Self {
        Self {
            status,
            scope: Some(scope_value(OAuthScope::SUPPORTED)),
            openid_configuration: discovery_url,
        }
    }

    /// The document's UTF-8 JSON encoding.
    pub fn to_json_bytes(&self) -> Vec<u8> {
        // The document holds only strings and a closed enum, which
        // serialization cannot fail on; an empty document would be refused
        // by any client rather than misread.
        serde_json::to_vec(self).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_or_blank_scope_selects_client_normal() {
        assert_eq!(
            parse_scope_request(None),
            Ok(vec![OAuthScope::ClientNormal])
        );
        assert_eq!(
            parse_scope_request(Some("  ")),
            Ok(vec![OAuthScope::ClientNormal])
        );
    }

    #[test]
    fn client_normal_is_recognised_and_duplicates_collapse() {
        assert_eq!(
            parse_scope_request(Some("xmpp:client:normal xmpp:client:normal")),
            Ok(vec![OAuthScope::ClientNormal])
        );
        assert_eq!(
            scope_value(&[OAuthScope::ClientNormal]),
            "xmpp:client:normal"
        );
    }

    #[test]
    fn unknown_scope_is_refused() {
        assert_eq!(parse_scope_request(Some("xmpp")), Err(UnsupportedScope));
        assert_eq!(
            parse_scope_request(Some("xmpp:client:normal admin")),
            Err(UnsupportedScope)
        );
        assert_eq!(parse_scope_value(""), Err(UnsupportedScope));
    }

    #[test]
    fn discovery_url_joins_the_base_url_and_the_metadata_path() {
        assert_eq!(
            discovery_url("https://chat.example/"),
            "https://chat.example/.well-known/oauth-authorization-server"
        );
    }

    #[test]
    fn error_document_carries_the_discovery_url() {
        let url = "https://example.com/.well-known/oauth-authorization-server";
        let json = OAuthBearerError::new(OAuthBearerStatus::InvalidToken, url).to_json_bytes();
        let value: serde_json::Value = serde_json::from_slice(&json).expect("json");
        assert_eq!(value["status"], "invalid_token");
        assert_eq!(value["openid-configuration"], url);
        assert_eq!(value["scope"], "xmpp:client:normal");
    }
}
