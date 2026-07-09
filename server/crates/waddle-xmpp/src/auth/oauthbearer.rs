//! SASL OAUTHBEARER wire types and parsing (RFC 7628).
//!
//! Waddle uses OAUTHBEARER only to authenticate an already-issued Waddle
//! session token. This module intentionally does not implement or advertise
//! XEP-0493 OAuth client login or authorization-server discovery.

use std::{collections::HashSet, fmt, str};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use jid::BareJid;
use serde::Serialize;
use thiserror::Error;
use xmpp_parsers::minidom::Element;

use crate::ns;

const RFC7628_ERROR_RESPONSE: &[u8] = b"\x01";

/// Opaque bearer credential parsed from an RFC 7628 client response.
#[derive(Clone)]
pub struct OAuthBearerToken(String);

impl OAuthBearerToken {
    /// Borrow the credential only at the authentication adapter boundary.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for OAuthBearerToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OAuthBearerToken(<redacted>)")
    }
}

/// Parsed RFC 7628 OAUTHBEARER credentials.
#[derive(Debug)]
pub struct OAuthBearerCredentials {
    token: OAuthBearerToken,
    authorization_identity: Option<BareJid>,
}

impl OAuthBearerCredentials {
    pub fn token(&self) -> &OAuthBearerToken {
        &self.token
    }

    /// Requested c2s authorization identity.
    ///
    /// RFC 6120 requires this to be a bare JID. The GS2 `saslname` escaping
    /// is removed before the value crosses this parser boundary.
    pub fn authorization_identity(&self) -> Option<&BareJid> {
        self.authorization_identity.as_ref()
    }
}

/// Result of parsing one complete RFC 7628 client response.
#[derive(Debug)]
pub enum OAuthBearerResult {
    /// A syntactically valid response with no bearer credential.
    EmptyCredentials,
    /// A syntactically valid Bearer authorization value.
    Credentials(OAuthBearerCredentials),
}

/// Structural failures in an RFC 7628 client response.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum OAuthBearerParseError {
    #[error("OAUTHBEARER response is not UTF-8")]
    InvalidUtf8,
    #[error("OAUTHBEARER response has an invalid GS2 header")]
    InvalidGs2Header,
    #[error("OAUTHBEARER response is missing its final two separators")]
    MissingTerminator,
    #[error("OAUTHBEARER response contains a malformed attribute")]
    MalformedAttribute,
    #[error("OAUTHBEARER response repeats an attribute")]
    DuplicateAttribute,
    #[error("OAUTHBEARER response has no authorization attribute")]
    MissingAuthorization,
    #[error("OAUTHBEARER response uses an unsupported authorization scheme")]
    UnsupportedAuthorizationScheme,
    #[error("OAUTHBEARER response contains an invalid RFC 6750 bearer token")]
    InvalidBearerToken,
    #[error("OAUTHBEARER response has an invalid authorization identity")]
    InvalidAuthorizationIdentity,
}

/// Parse a complete OAUTHBEARER client response per RFC 7628 section 3.1.
pub fn parse_oauthbearer(data: &[u8]) -> Result<OAuthBearerResult, OAuthBearerParseError> {
    let response = str::from_utf8(data).map_err(|_| OAuthBearerParseError::InvalidUtf8)?;
    let (gs2_header, attributes) = response
        .split_once('\x01')
        .ok_or(OAuthBearerParseError::InvalidGs2Header)?;
    let authorization_identity = parse_gs2_header(gs2_header)?;
    let attributes = attributes
        .strip_suffix("\x01\x01")
        .ok_or(OAuthBearerParseError::MissingTerminator)?;

    let mut seen = HashSet::new();
    let mut authorization = None;
    if attributes.is_empty() {
        return Err(OAuthBearerParseError::MissingAuthorization);
    }
    for field in attributes.split('\x01') {
        let (key, value) = field
            .split_once('=')
            .filter(|(key, _)| !key.is_empty())
            .ok_or(OAuthBearerParseError::MalformedAttribute)?;
        if !key.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            return Err(OAuthBearerParseError::MalformedAttribute);
        }
        if !seen.insert(key) {
            return Err(OAuthBearerParseError::DuplicateAttribute);
        }
        if key == "auth" {
            authorization = Some(value);
        }
    }

    let authorization = authorization.ok_or(OAuthBearerParseError::MissingAuthorization)?;
    if authorization.is_empty() {
        return Ok(OAuthBearerResult::EmptyCredentials);
    }

    let Some(separator) = authorization.find(' ') else {
        return Err(OAuthBearerParseError::UnsupportedAuthorizationScheme);
    };
    let scheme = &authorization[..separator];
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return Err(OAuthBearerParseError::UnsupportedAuthorizationScheme);
    }
    let credential = authorization[separator..].trim_start_matches(' ');
    if credential.is_empty() {
        return Ok(OAuthBearerResult::EmptyCredentials);
    }
    if !is_rfc6750_b64token(credential) {
        return Err(OAuthBearerParseError::InvalidBearerToken);
    }

    Ok(OAuthBearerResult::Credentials(OAuthBearerCredentials {
        token: OAuthBearerToken(credential.to_owned()),
        authorization_identity,
    }))
}

fn is_rfc6750_b64token(token: &str) -> bool {
    let token_bytes = token.as_bytes();
    let payload_len = token_bytes
        .iter()
        .position(|byte| *byte == b'=')
        .unwrap_or(token_bytes.len());
    payload_len > 0
        && token_bytes[..payload_len].iter().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'+' | b'/')
        })
        && token_bytes[payload_len..].iter().all(|byte| *byte == b'=')
}

fn parse_gs2_header(header: &str) -> Result<Option<BareJid>, OAuthBearerParseError> {
    let authorization_identity = header
        .strip_prefix("n,")
        .and_then(|header| header.strip_suffix(','))
        .ok_or(OAuthBearerParseError::InvalidGs2Header)?;
    if authorization_identity.is_empty() {
        return Ok(None);
    }
    let encoded_identity = authorization_identity
        .strip_prefix("a=")
        .ok_or(OAuthBearerParseError::InvalidGs2Header)?;
    let identity = decode_saslname(encoded_identity)?;
    identity
        .parse()
        .map(Some)
        .map_err(|_| OAuthBearerParseError::InvalidAuthorizationIdentity)
}

fn decode_saslname(value: &str) -> Result<String, OAuthBearerParseError> {
    let mut decoded = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '=' {
            decoded.push(ch);
            continue;
        }
        let first = chars
            .next()
            .ok_or(OAuthBearerParseError::InvalidAuthorizationIdentity)?;
        let second = chars
            .next()
            .ok_or(OAuthBearerParseError::InvalidAuthorizationIdentity)?;
        match (first, second) {
            ('2', 'C') => decoded.push(','),
            ('3', 'D') => decoded.push('='),
            _ => return Err(OAuthBearerParseError::InvalidAuthorizationIdentity),
        }
    }
    Ok(decoded)
}

/// RFC 7628 authorization error status.
#[derive(Clone, Copy, Debug, Serialize)]
enum OAuthAuthorizationError {
    #[serde(rename = "invalid_token")]
    InvalidToken,
}

/// Typed RFC 7628 JSON error challenge for an unusable bearer credential.
#[derive(Debug, Serialize)]
pub struct OAuthBearerErrorChallenge {
    status: OAuthAuthorizationError,
}

impl OAuthBearerErrorChallenge {
    pub const fn invalid_token() -> Self {
        Self {
            status: OAuthAuthorizationError::InvalidToken,
        }
    }

    /// Build the RFC 6120 SASL `<challenge/>` with base64-encoded JSON.
    pub fn to_element(&self) -> Result<Element, serde_json::Error> {
        let encoded = BASE64_STANDARD.encode(serde_json::to_vec(self)?);
        Ok(Element::builder("challenge", ns::SASL)
            .append(encoded)
            .build())
    }
}

/// The RFC 7628 section 3.2.3 response used to complete a failed exchange.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OAuthBearerErrorResponse {
    Acknowledged,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("OAUTHBEARER error completion must contain the single RFC 7628 response octet")]
pub struct InvalidOAuthBearerErrorResponse;

pub fn parse_oauthbearer_error_response(
    decoded: &[u8],
) -> Result<OAuthBearerErrorResponse, InvalidOAuthBearerErrorResponse> {
    if decoded == RFC7628_ERROR_RESPONSE {
        Ok(OAuthBearerErrorResponse::Acknowledged)
    } else {
        Err(InvalidOAuthBearerErrorResponse)
    }
}
