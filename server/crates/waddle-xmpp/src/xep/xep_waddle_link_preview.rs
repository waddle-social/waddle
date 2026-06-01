//! Waddle link-preview request payload.
//!
//! This is intentionally not XEP-0511. It is a private request element the
//! sender includes after an XMPP-native composer lookup. The server consumes
//! it before fanout/archive and stamps conformant XEP-0511 metadata instead.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, KeyInit, Mac};
use jid::BareJid;
use minidom::{rxml::xml_ncname, Element};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;
use url::Url;
use xmpp_parsers::message::Message;

/// Waddle-private link preview namespace.
pub const NS_WADDLE_LINK_PREVIEW: &str = "urn:waddle:link-preview:0";

/// Root element for a send-time preview token request.
pub const ELEMENT_PREVIEW_REQUEST: &str = "preview-request";

type HmacSha256 = Hmac<Sha256>;

/// Opaque composer preview token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPreviewToken(String);

impl LinkPreviewToken {
    /// Wrap an already encoded token.
    pub fn new(token: impl Into<String>) -> Option<Self> {
        let token = token.into();
        (!token.trim().is_empty()).then_some(Self(token))
    }

    /// Borrow the encoded token text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Typed payload carried inside a preview token for the first tracer bullet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPreviewTokenData {
    pub sender_jid: BareJid,
    pub scope_jid: BareJid,
    pub original_url: Url,
    pub normalized_url: Url,
    pub title: Option<String>,
    pub description: Option<String>,
    pub expires_at_unix: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LinkPreviewTokenWire {
    sender_jid: String,
    scope_jid: String,
    original_url: String,
    normalized_url: String,
    title: Option<String>,
    description: Option<String>,
    expires_at_unix: i64,
}

/// Errors for private Waddle link-preview request parsing.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum WaddleLinkPreviewError {
    #[error("expected <preview-request/> in namespace '{NS_WADDLE_LINK_PREVIEW}'")]
    WrongRoot,
    #[error("missing preview token")]
    MissingToken,
    #[error("invalid preview token encoding")]
    InvalidTokenEncoding,
    #[error("invalid preview token JSON")]
    InvalidTokenJson,
    #[error("invalid preview token signature")]
    InvalidTokenSignature,
    #[error("invalid preview token JID")]
    InvalidTokenJid,
    #[error("invalid preview token URL")]
    InvalidTokenUrl,
    #[error("preview token expired")]
    Expired,
}

/// Check if an element is a Waddle link-preview request.
pub fn is_link_preview_request_element(elem: &Element) -> bool {
    elem.name() == ELEMENT_PREVIEW_REQUEST && elem.ns() == NS_WADDLE_LINK_PREVIEW
}

/// Parse a `<preview-request/>` element into its opaque token.
pub fn parse_link_preview_request_element(
    elem: &Element,
) -> Result<LinkPreviewToken, WaddleLinkPreviewError> {
    if !is_link_preview_request_element(elem) {
        return Err(WaddleLinkPreviewError::WrongRoot);
    }
    elem.attr("token")
        .and_then(LinkPreviewToken::new)
        .ok_or(WaddleLinkPreviewError::MissingToken)
}

/// Extract the first preview request token from a message.
pub fn extract_link_preview_request_from_message(msg: &Message) -> Option<LinkPreviewToken> {
    msg.payloads
        .iter()
        .find(|payload| is_link_preview_request_element(payload))
        .and_then(|payload| parse_link_preview_request_element(payload).ok())
}

/// Build a private send-time preview request payload.
pub fn build_link_preview_request_element(token: &LinkPreviewToken) -> Element {
    Element::builder(ELEMENT_PREVIEW_REQUEST, NS_WADDLE_LINK_PREVIEW)
        .attr(xml_ncname!("token").to_owned(), token.as_str())
        .build()
}

/// Remove private Waddle link-preview requests from a message.
pub fn strip_link_preview_requests(msg: &mut Message) {
    msg.payloads
        .retain(|payload| !is_link_preview_request_element(payload));
}

/// Encode the narrow plaintext preview data into an opaque token.
pub fn encode_link_preview_token(data: &LinkPreviewTokenData, secret: &[u8]) -> LinkPreviewToken {
    let wire = LinkPreviewTokenWire {
        sender_jid: data.sender_jid.to_string(),
        scope_jid: data.scope_jid.to_string(),
        original_url: data.original_url.as_str().to_string(),
        normalized_url: data.normalized_url.as_str().to_string(),
        title: data.title.clone(),
        description: data.description.clone(),
        expires_at_unix: data.expires_at_unix,
    };
    let json = serde_json::to_vec(&wire).expect("wire token is serializable");
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key size");
    mac.update(&json);
    let signature = mac.finalize().into_bytes();
    LinkPreviewToken(format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(json),
        URL_SAFE_NO_PAD.encode(signature)
    ))
}

/// Decode and validate an opaque preview token.
pub fn decode_link_preview_token(
    token: &LinkPreviewToken,
    secret: &[u8],
    now_unix: i64,
) -> Result<LinkPreviewTokenData, WaddleLinkPreviewError> {
    let (payload, signature) = token
        .as_str()
        .split_once('.')
        .ok_or(WaddleLinkPreviewError::InvalidTokenEncoding)?;
    let json = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| WaddleLinkPreviewError::InvalidTokenEncoding)?;
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| WaddleLinkPreviewError::InvalidTokenEncoding)?;
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key size");
    mac.update(&json);
    mac.verify_slice(&signature)
        .map_err(|_| WaddleLinkPreviewError::InvalidTokenSignature)?;
    let wire: LinkPreviewTokenWire =
        serde_json::from_slice(&json).map_err(|_| WaddleLinkPreviewError::InvalidTokenJson)?;
    if wire.expires_at_unix < now_unix {
        return Err(WaddleLinkPreviewError::Expired);
    }
    Ok(LinkPreviewTokenData {
        sender_jid: wire
            .sender_jid
            .parse()
            .map_err(|_| WaddleLinkPreviewError::InvalidTokenJid)?,
        scope_jid: wire
            .scope_jid
            .parse()
            .map_err(|_| WaddleLinkPreviewError::InvalidTokenJid)?,
        original_url: Url::parse(&wire.original_url)
            .map_err(|_| WaddleLinkPreviewError::InvalidTokenUrl)?,
        normalized_url: Url::parse(&wire.normalized_url)
            .map_err(|_| WaddleLinkPreviewError::InvalidTokenUrl)?,
        title: wire.title,
        description: wire.description,
        expires_at_unix: wire.expires_at_unix,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"test-link-preview-secret";

    #[test]
    fn request_element_round_trips_token() {
        let token = LinkPreviewToken::new("token-1").expect("token");
        let elem = build_link_preview_request_element(&token);

        assert_eq!(
            parse_link_preview_request_element(&elem).expect("request"),
            token
        );
    }

    #[test]
    fn token_round_trips_typed_preview_data() {
        let data = LinkPreviewTokenData {
            sender_jid: "alice@example.com".parse().expect("jid"),
            scope_jid: "room@muc.example.com".parse().expect("jid"),
            original_url: Url::parse("https://example.com/path").expect("url"),
            normalized_url: Url::parse("https://example.com/path").expect("url"),
            title: Some("Example".to_string()),
            description: Some("Plain text preview".to_string()),
            expires_at_unix: 1_900_000_000,
        };

        let token = encode_link_preview_token(&data, SECRET);

        assert_eq!(
            decode_link_preview_token(&token, SECRET, 1_800_000_000),
            Ok(data)
        );
    }

    #[test]
    fn tampered_token_signature_is_rejected() {
        let data = LinkPreviewTokenData {
            sender_jid: "alice@example.com".parse().expect("jid"),
            scope_jid: "room@muc.example.com".parse().expect("jid"),
            original_url: Url::parse("https://example.com/path").expect("url"),
            normalized_url: Url::parse("https://example.com/path").expect("url"),
            title: Some("Example".to_string()),
            description: Some("Plain text preview".to_string()),
            expires_at_unix: 1_900_000_000,
        };

        let mut token = encode_link_preview_token(&data, SECRET);
        token.0.push('x');

        assert_eq!(
            decode_link_preview_token(&token, SECRET, 1_800_000_000),
            Err(WaddleLinkPreviewError::InvalidTokenSignature)
        );
    }

    #[test]
    fn expired_token_is_rejected() {
        let data = LinkPreviewTokenData {
            sender_jid: "alice@example.com".parse().expect("jid"),
            scope_jid: "room@muc.example.com".parse().expect("jid"),
            original_url: Url::parse("https://example.com/").expect("url"),
            normalized_url: Url::parse("https://example.com/").expect("url"),
            title: None,
            description: None,
            expires_at_unix: 10,
        };

        let token = encode_link_preview_token(&data, SECRET);

        assert_eq!(
            decode_link_preview_token(&token, SECRET, 11),
            Err(WaddleLinkPreviewError::Expired)
        );
    }
}
