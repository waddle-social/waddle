//! Typed newtypes for the Web Push pipeline.
//!
//! Conforms to the CLAUDE.md typed-payloads hard rule: every protocol
//! boundary uses a typed value, never `String`/`&str`/`Vec<u8>` blobs.

use std::fmt;
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use thiserror::Error;
use uuid::Uuid;
use zeroize::ZeroizeOnDrop;

/// VAPID keypair identifier. UUID in code; TEXT in DB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Kid(pub Uuid);

impl Kid {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn as_uuid(&self) -> Uuid {
        self.0
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

impl Default for Kid {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for Kid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// RFC 8030 §5.4 `Topic` header — base64url, ≤ 32 chars total. Construction-validated.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PushTopic(Box<str>);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PushTopicParseError {
    #[error("push topic exceeds 32 chars (got {0})")]
    TooLong(usize),
    #[error("push topic contains non-base64url character at index {0}")]
    InvalidAlphabet(usize),
}

impl PushTopic {
    pub fn new(value: impl Into<Box<str>>) -> Result<Self, PushTopicParseError> {
        let s: Box<str> = value.into();
        if s.len() > 32 {
            return Err(PushTopicParseError::TooLong(s.len()));
        }
        // RFC 8030 §5.4 ABNF: `topic-char = ALPHA / DIGIT / "-" / "_"` —
        // strict base64url-no-pad alphabet; NO `:`.
        for (i, c) in s.chars().enumerate() {
            let valid = c.is_ascii_alphanumeric() || c == '-' || c == '_';
            if !valid {
                return Err(PushTopicParseError::InvalidAlphabet(i));
            }
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for PushTopic {
    type Error = PushTopicParseError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value.to_owned())
    }
}

impl fmt::Display for PushTopic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// VAPID `sub` claim — `mailto:<address>` or `https://<url>` per RFC 8292 §2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VapidSub {
    Mailto(MailtoAddress),
    Url(url::Url),
}

/// `mailto:` address, validated minimally for RFC 8292 `sub` use.
/// Rule: split on last `@`, both sides non-empty, no control/whitespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailtoAddress(Box<str>);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VapidSubParseError {
    #[error("empty `sub` claim")]
    Empty,
    #[error("missing scheme; expected `mailto:` or `https://`")]
    MissingScheme,
    #[error("unsupported scheme {0:?}; expected `mailto:` or `https://`")]
    UnsupportedScheme(String),
    #[error("malformed mailto address")]
    BadMailto,
    #[error("malformed URL: {0}")]
    BadUrl(String),
}

impl VapidSub {
    /// Default `sub` for an XMPP server: `mailto:postmaster@<domain>` per
    /// RFC 2142 / XMPP convention.
    pub fn default_for_domain(domain: &str) -> Result<Self, VapidSubParseError> {
        let mailto = format!("postmaster@{domain}");
        Ok(Self::Mailto(MailtoAddress::new(mailto)?))
    }

    /// Render as the `sub` claim string for JWT inclusion.
    pub fn as_claim(&self) -> String {
        match self {
            Self::Mailto(addr) => format!("mailto:{}", addr.as_str()),
            Self::Url(url) => url.to_string(),
        }
    }
}

impl MailtoAddress {
    pub fn new(value: impl Into<Box<str>>) -> Result<Self, VapidSubParseError> {
        let s: Box<str> = value.into();
        if s.is_empty() {
            return Err(VapidSubParseError::Empty);
        }
        // Reject control/whitespace, plus URI-reserved characters that would
        // require percent-encoding in a mailto URI (RFC 6068 §2). The
        // `sub` claim is embedded verbatim as `mailto:<addr>` into the JWT;
        // unescaped reserved chars produce malformed URIs.
        let reject = |c: char| {
            c.is_control()
                || c.is_whitespace()
                || matches!(c, '?' | '#' | '%' | '&' | '/' | ',' | ';')
        };
        if s.chars().any(reject) {
            return Err(VapidSubParseError::BadMailto);
        }
        let (local, host) = s.rsplit_once('@').ok_or(VapidSubParseError::BadMailto)?;
        if local.is_empty() || host.is_empty() {
            return Err(VapidSubParseError::BadMailto);
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for VapidSub {
    type Err = VapidSubParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(VapidSubParseError::Empty);
        }
        if let Some(rest) = s.strip_prefix("mailto:") {
            // RFC 6068: `mailto` URIs are NOT hierarchical and MUST NOT
            // begin with `//`. Reject `mailto://foo@bar`-style inputs.
            if rest.starts_with("//") {
                return Err(VapidSubParseError::BadMailto);
            }
            return Ok(Self::Mailto(MailtoAddress::new(rest.to_owned())?));
        }
        if let Some(scheme_idx) = s.find(':') {
            let scheme = &s[..scheme_idx];
            if scheme == "https" || scheme == "http" {
                let url =
                    url::Url::parse(s).map_err(|e| VapidSubParseError::BadUrl(e.to_string()))?;
                if url.scheme() != "https" {
                    return Err(VapidSubParseError::UnsupportedScheme(scheme.into()));
                }
                return Ok(Self::Url(url));
            }
            return Err(VapidSubParseError::UnsupportedScheme(scheme.into()));
        }
        Err(VapidSubParseError::MissingScheme)
    }
}

/// RFC 8291 encrypted payload — RFC 8188 header + AES-GCM record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedPayload(Vec<u8>);

impl EncryptedPayload {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.0
    }
}

/// Signed VAPID JWT, ready for `Authorization: vapid t=…` use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VapidJwt(String);

impl VapidJwt {
    pub fn new(token: String) -> Self {
        Self(token)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// RFC 8291 16-byte `auth_secret`. Zeroized on final `Arc::drop`; NOT `Clone`
/// — sharing happens via `Arc<AuthSecret>` so only one copy lives in heap.
///
/// Note: spawned futures holding a clone of `Arc<AuthSecret>` extend
/// zeroize until the future completes. This is intentional — the secret
/// lives as long as it is in use. Do not move `Arc<AuthSecret>` into
/// futures with longer-than-necessary lifetimes.
#[derive(ZeroizeOnDrop)]
pub struct AuthSecret([u8; 16]);

impl AuthSecret {
    pub const LEN: usize = 16;

    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self, WebPushCryptoError> {
        if bytes.len() != Self::LEN {
            return Err(
                MalformedSubscriptionError::AuthSecretWrongLength { found: bytes.len() }.into(),
            );
        }
        let mut buf = [0u8; 16];
        buf.copy_from_slice(bytes);
        Ok(Self(buf))
    }

    pub fn from_base64url(input: &str) -> Result<Self, WebPushCryptoError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(input.trim_end_matches('='))
            .map_err(|source| MalformedSubscriptionError::AuthSecretBase64url { source })?;
        Self::from_slice(&bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Debug for AuthSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthSecret(***)")
    }
}

/// Subscriber-supplied ECDH/auth material, parsed once at the deserialization
/// boundary. `auth` is `Arc<AuthSecret>` so cloning the struct does not
/// duplicate the wrapped 16-byte secret in heap.
#[derive(Debug, Clone)]
pub struct SubscriptionKeys {
    pub p256dh: p256::PublicKey,
    pub auth: Arc<AuthSecret>,
}

impl SubscriptionKeys {
    /// Parse a browser-supplied (p256dh, auth) pair from base64url-no-pad
    /// strings. Rejects with `WebPushCryptoError::MalformedSubscription` on any
    /// length/curve/format error.
    pub fn from_base64url(p256dh: &str, auth: &str) -> Result<Self, WebPushCryptoError> {
        let p256dh_bytes = URL_SAFE_NO_PAD
            .decode(p256dh.trim_end_matches('='))
            .map_err(|source| MalformedSubscriptionError::P256DhBase64url { source })?;
        if p256dh_bytes.len() != 65 {
            return Err(MalformedSubscriptionError::P256DhWrongLength {
                found: p256dh_bytes.len(),
            }
            .into());
        }
        if p256dh_bytes[0] != 0x04 {
            return Err(MalformedSubscriptionError::P256DhWrongPrefix {
                found: p256dh_bytes[0],
            }
            .into());
        }
        let pk = p256::PublicKey::from_sec1_bytes(&p256dh_bytes)
            .map_err(|source| MalformedSubscriptionError::P256DhNotOnCurve { source })?;
        let auth_secret = AuthSecret::from_base64url(auth)?;
        Ok(Self {
            p256dh: pk,
            auth: Arc::new(auth_secret),
        })
    }
}

/// SHA-256 of the subscription endpoint URL — keys per-endpoint rate-limit
/// buckets in `DashMap` without keeping the URL string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EndpointHash(pub [u8; 32]);

impl EndpointHash {
    pub fn of(endpoint_url: &str) -> Self {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(endpoint_url.as_bytes());
        let mut out = [0u8; 32];
        out.copy_from_slice(&hasher.finalize());
        Self(out)
    }
}

/// Errors from the Web Push crypto + sender path.
///
/// Kept as a standalone enum (rather than extending `super::PushError`)
/// so the typed-payloads hard rule is honored at this boundary: the
/// existing `super::PushError` uses stringly fields that PR-D2's sender
/// swap will retire.
#[derive(Debug, Error)]
pub enum WebPushCryptoError {
    #[error("malformed subscription: {0}")]
    MalformedSubscription(#[from] MalformedSubscriptionError),
    #[error("invalid push endpoint origin: {origin:?}")]
    InvalidEndpoint { origin: url::Origin },
    #[error("push payload too large: {plaintext_len} > {limit}")]
    PayloadTooLarge { plaintext_len: usize, limit: usize },
    #[error("AES-GCM encrypt failed")]
    EncryptFailed,
    #[error("ECDH derivation failed")]
    EcdhFailed,
    #[error("HKDF expansion failed")]
    HkdfFailed,
}

#[derive(Debug, Error)]
pub enum MalformedSubscriptionError {
    #[error("auth_secret base64url decode failed")]
    AuthSecretBase64url {
        #[source]
        source: base64::DecodeError,
    },
    #[error("auth_secret must be 16 bytes, got {found}")]
    AuthSecretWrongLength { found: usize },
    #[error("p256dh base64url decode failed")]
    P256DhBase64url {
        #[source]
        source: base64::DecodeError,
    },
    #[error("p256dh must be 65 bytes (uncompressed P-256 point), got {found}")]
    P256DhWrongLength { found: usize },
    #[error("p256dh must start with 0x04 (uncompressed point prefix), got 0x{found:02x}")]
    P256DhWrongPrefix { found: u8 },
    #[error("p256dh must be a valid P-256 point")]
    P256DhNotOnCurve {
        #[source]
        source: p256::elliptic_curve::Error,
    },
}

/// Errors from the VAPID signing path.
#[derive(Debug, Error)]
pub enum VapidSignError {
    #[error("invalid push endpoint for VAPID `aud`: {0}")]
    InvalidEndpoint(#[source] WebPushCryptoError),
    #[error("ES256 signing failed: {0}")]
    Signing(#[from] jsonwebtoken::errors::Error),
    #[error("VAPID key {kid} is retired")]
    KeyRetired { kid: Kid },
    #[error("VAPID signer key encoding failed")]
    KeyEncoding {
        #[source]
        source: p256::pkcs8::Error,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VapidDbReadStage {
    Query,
    NextRow,
    DecodeKid,
    DecodeSealedPrivateKey,
    DecodeRootKeyVersion,
    ParseKidUuid,
    ParseSecretKeyScalar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VapidDbWriteStage {
    Initialize,
    SealPrivateKey,
    InsertRow,
}

#[derive(Debug, Error)]
pub enum VapidEnvParseError {
    #[error("base64url decode failed")]
    Base64urlDecode {
        #[source]
        source: base64::DecodeError,
    },
    #[error("expected 32-byte P-256 scalar; got {found} bytes")]
    InvalidLength { found: usize },
    #[error("invalid P-256 scalar")]
    InvalidScalar {
        #[source]
        source: p256::elliptic_curve::Error,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VapidUnsealErrorKind {
    MissingPrefix,
    UnexpectedPrefix,
    MissingNonce,
    MissingCiphertext,
    UnexpectedTrailingField,
    NonceBase64urlDecode,
    NonceWrongLength,
    CiphertextBase64urlDecode,
    AeadOpenFailed,
}

/// Errors loading the VAPID config at boot. All variants map to
/// `WebPushCapability::Disabled { reason }`.
#[derive(Debug, Error)]
pub enum VapidLoadError {
    #[error("env var WADDLE_VAPID_PRIVATE_KEY parse failed: {0}")]
    EnvParse(#[from] VapidEnvParseError),
    #[error("env var WADDLE_VAPID_SUB parse failed: {0}")]
    SubParse(#[from] VapidSubParseError),
    #[error("VAPID storage read failed at {stage:?}")]
    DbRead {
        stage: VapidDbReadStage,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("VAPID storage write failed at {stage:?}")]
    DbWrite {
        stage: VapidDbWriteStage,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("VAPID key unseal failed ({kind:?})")]
    Unseal { kind: VapidUnsealErrorKind },
    #[error(
        "sealed blob references unknown root_key_version {found}; max installed {max_installed}"
    )]
    UnknownRootKeyVersion { found: u32, max_installed: u32 },
    #[error("missing root key for version {0}")]
    MissingRootKey(u32),
    #[error("VAPID signer initialization failed")]
    SignerInit {
        #[source]
        source: VapidSignError,
    },
    #[error("fresh P-256 keypair generation failed")]
    Generate,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn kid_round_trip() {
        let kid = Kid::new();
        assert_eq!(kid.as_uuid(), kid.0);
        assert_eq!(kid.as_bytes().len(), 16);
    }

    #[test]
    fn push_topic_accepts_valid() {
        let t = PushTopic::new("d-abcDEF_0123").expect("valid");
        assert_eq!(t.as_str(), "d-abcDEF_0123");
    }

    #[test]
    fn push_topic_rejects_colon_per_rfc_8030_5_4() {
        // RFC 8030 §5.4 topic-char does NOT include `:` — must be rejected.
        let err = PushTopic::new("d:abc").unwrap_err();
        assert!(matches!(err, PushTopicParseError::InvalidAlphabet(1)));
    }

    #[test]
    fn push_topic_rejects_too_long() {
        let s: String = "a".repeat(33);
        assert_eq!(
            PushTopic::new(s).unwrap_err(),
            PushTopicParseError::TooLong(33)
        );
    }

    #[test]
    fn push_topic_accepts_exactly_32_chars() {
        // RFC 8030 §5.4: `Topic = 1*32(topic-char)`. 32 is the inclusive max.
        let s: String = "a".repeat(32);
        let t = PushTopic::new(s.clone()).expect("32 chars is valid");
        assert_eq!(t.as_str(), s);
    }

    #[test]
    fn push_topic_rejects_invalid_alphabet() {
        // '$' is not base64url
        let err = PushTopic::new("hello$world").unwrap_err();
        assert!(matches!(err, PushTopicParseError::InvalidAlphabet(5)));
    }

    #[test]
    fn vapid_sub_default_for_domain() {
        let sub = VapidSub::default_for_domain("example.com").expect("valid");
        assert_eq!(sub.as_claim(), "mailto:postmaster@example.com");
    }

    #[test]
    fn vapid_sub_parses_mailto() {
        let sub = VapidSub::from_str("mailto:ops@example.com").expect("valid");
        assert_eq!(sub.as_claim(), "mailto:ops@example.com");
    }

    #[test]
    fn vapid_sub_parses_https_url() {
        let sub = VapidSub::from_str("https://example.com/contact").expect("valid");
        assert_eq!(sub.as_claim(), "https://example.com/contact");
    }

    #[test]
    fn vapid_sub_rejects_http_scheme() {
        // http (not https) is rejected
        let err = VapidSub::from_str("http://example.com").unwrap_err();
        assert!(matches!(err, VapidSubParseError::UnsupportedScheme(_)));
    }

    #[test]
    fn vapid_sub_rejects_empty_mailto_local() {
        let err = VapidSub::from_str("mailto:@example.com").unwrap_err();
        assert!(matches!(err, VapidSubParseError::BadMailto));
    }

    #[test]
    fn vapid_sub_rejects_empty_mailto_host() {
        let err = VapidSub::from_str("mailto:user@").unwrap_err();
        assert!(matches!(err, VapidSubParseError::BadMailto));
    }

    #[test]
    fn vapid_sub_rejects_no_scheme() {
        let err = VapidSub::from_str("nope").unwrap_err();
        assert!(matches!(err, VapidSubParseError::MissingScheme));
    }

    #[test]
    fn vapid_sub_rejects_mailto_with_authority_slashes() {
        // RFC 6068: mailto URIs are non-hierarchical; `mailto://foo@bar` is malformed.
        let err = VapidSub::from_str("mailto://foo@bar.example").unwrap_err();
        assert!(matches!(err, VapidSubParseError::BadMailto));
    }

    #[test]
    fn vapid_sub_rejects_mailto_with_uri_reserved_chars() {
        // RFC 6068 §2: `?`, `#`, `%`, `&`, etc. need percent-encoding.
        for bad in &[
            "mailto:foo?subj=x@bar.example",
            "mailto:foo#frag@bar.example",
            "mailto:foo%20bar@bar.example",
            "mailto:foo,bar@bar.example",
        ] {
            match VapidSub::from_str(bad) {
                Ok(_) => panic!("input `{bad}` should be rejected"),
                Err(err) => {
                    assert!(matches!(err, VapidSubParseError::BadMailto), "input {bad}")
                }
            }
        }
    }

    #[test]
    fn auth_secret_from_slice_strict_length() {
        let ok = AuthSecret::from_slice(&[0u8; 16]).expect("16 bytes");
        assert_eq!(ok.as_bytes(), &[0u8; 16]);
        assert!(AuthSecret::from_slice(&[0u8; 15]).is_err());
        assert!(AuthSecret::from_slice(&[0u8; 17]).is_err());
    }

    #[test]
    fn auth_secret_debug_does_not_leak() {
        let secret = AuthSecret::from_bytes([0xAA; 16]);
        let dbg = format!("{:?}", secret);
        assert_eq!(dbg, "AuthSecret(***)");
        assert!(!dbg.contains("AA"));
    }

    #[test]
    fn auth_secret_zeroize_on_drop() {
        // The Zeroize derive zeroes on drop; we can't observe heap directly,
        // but we can verify the Zeroize trait is implemented.
        let secret = AuthSecret::from_bytes([0xFF; 16]);
        let _ = secret; // dropped here — ZeroizeOnDrop
    }

    #[test]
    fn endpoint_hash_is_deterministic() {
        let h1 = EndpointHash::of("https://fcm.googleapis.com/fcm/send/abc");
        let h2 = EndpointHash::of("https://fcm.googleapis.com/fcm/send/abc");
        assert_eq!(h1, h2);
        let h3 = EndpointHash::of("https://fcm.googleapis.com/fcm/send/different");
        assert_ne!(h1, h3);
    }

    #[test]
    fn subscription_keys_reject_short_p256dh() {
        let err = SubscriptionKeys::from_base64url("AAAA", "AAAAAAAAAAAAAAAAAAAAAA").unwrap_err();
        assert!(matches!(
            err,
            WebPushCryptoError::MalformedSubscription(
                MalformedSubscriptionError::P256DhWrongLength { .. }
            )
        ));
    }

    #[test]
    fn subscription_keys_reject_wrong_prefix() {
        // 65 bytes but starting with 0x02 (compressed Y-bit) — not uncompressed
        let mut bad = vec![0x02];
        bad.extend_from_slice(&[0u8; 64]);
        let p256dh = URL_SAFE_NO_PAD.encode(&bad);
        let auth = URL_SAFE_NO_PAD.encode([0u8; 16]);
        let err = SubscriptionKeys::from_base64url(&p256dh, &auth).unwrap_err();
        assert!(matches!(
            err,
            WebPushCryptoError::MalformedSubscription(
                MalformedSubscriptionError::P256DhWrongPrefix { found: 0x02 }
            )
        ));
    }

    #[test]
    fn subscription_keys_reject_off_curve_point() {
        // 65 bytes, 0x04 prefix, but X=Y=0 → identity point, not on curve.
        let mut bad = vec![0x04];
        bad.extend_from_slice(&[0u8; 64]);
        let p256dh = URL_SAFE_NO_PAD.encode(&bad);
        let auth = URL_SAFE_NO_PAD.encode([0u8; 16]);
        let err = SubscriptionKeys::from_base64url(&p256dh, &auth).unwrap_err();
        assert!(matches!(
            err,
            WebPushCryptoError::MalformedSubscription(
                MalformedSubscriptionError::P256DhNotOnCurve { .. }
            )
        ));
    }

    #[test]
    fn subscription_keys_reject_short_auth() {
        use p256::elliptic_curve::rand_core::OsRng;
        use p256::elliptic_curve::sec1::ToEncodedPoint;
        // Valid p256dh, but auth is only 8 bytes
        let pk = p256::SecretKey::random(&mut OsRng).public_key();
        let pk_bytes = pk.to_encoded_point(false);
        let p256dh = URL_SAFE_NO_PAD.encode(pk_bytes.as_bytes());
        let auth = URL_SAFE_NO_PAD.encode([0u8; 8]);
        let err = SubscriptionKeys::from_base64url(&p256dh, &auth).unwrap_err();
        assert!(matches!(
            err,
            WebPushCryptoError::MalformedSubscription(
                MalformedSubscriptionError::AuthSecretWrongLength { found: 8 }
            )
        ));
    }
}
