//! Server Dialback (XEP-0220) implementation.
//!
//! This module implements the Server Dialback protocol for authenticating
//! server-to-server (S2S) connections. Dialback provides a mechanism for
//! verifying the identity of remote servers through a challenge-response
//! protocol.
//!
//! # Protocol Overview
//!
//! 1. Originating Server sends `db:result` with a dialback key
//! 2. Receiving Server opens a connection to Originating Server and sends `db:verify`
//! 3. Originating Server responds to `db:verify` with valid/invalid
//! 4. Receiving Server sends `db:result` response (valid/invalid) to Originating Server
//!
//! # Key Generation (Section 2.4)
//!
//! The dialback key is generated using HMAC-SHA256:
//! ```text
//! key = HMAC-SHA256(secret, stream_id + receiving_domain + originating_domain)
//! ```
//!
//! # References
//!
//! - [XEP-0220: Server Dialback](https://xmpp.org/extensions/xep-0220.html)
//! - [RFC 3920](https://tools.ietf.org/html/rfc3920) - Original dialback spec (obsolete)

use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::fmt;

type HmacSha256 = Hmac<Sha256>;

/// Server Dialback namespace.
pub const NS_DIALBACK: &str = "jabber:server:dialback";

/// Server Dialback feature namespace (for stream features).
pub const NS_DIALBACK_FEATURES: &str = "urn:xmpp:features:dialback";

/// State of a dialback verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialbackState {
    /// Dialback not yet initiated.
    None,
    /// Waiting for verification (db:result sent, waiting for response).
    Pending,
    /// Dialback verification successful.
    Verified,
    /// Dialback verification failed.
    Failed,
}

impl Default for DialbackState {
    fn default() -> Self {
        Self::None
    }
}

impl fmt::Display for DialbackState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::Pending => write!(f, "pending"),
            Self::Verified => write!(f, "verified"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

/// Result type for dialback verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialbackResult {
    /// Verification successful.
    Valid,
    /// Verification failed.
    Invalid,
}

impl DialbackResult {
    /// Get the XEP-0220 type attribute value.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Invalid => "invalid",
        }
    }

    /// Parse from XEP-0220 type attribute value.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "valid" => Some(Self::Valid),
            "invalid" => Some(Self::Invalid),
            _ => None,
        }
    }
}

/// Dialback key generator following XEP-0220 Section 2.4.
///
/// The dialback key provides a way for the receiving server to verify that
/// the originating server is who it claims to be, without requiring TLS
/// client certificates or SASL EXTERNAL.
#[derive(Clone)]
pub struct DialbackKey {
    /// Secret key for HMAC (should be persistent per-server)
    secret: Vec<u8>,
}

impl DialbackKey {
    /// Create a new dialback key generator with the given secret.
    ///
    /// The secret should be randomly generated and kept consistent for the
    /// lifetime of the server (to allow verification of previously generated keys).
    pub fn new(secret: impl AsRef<[u8]>) -> Self {
        Self {
            secret: secret.as_ref().to_vec(),
        }
    }

    /// Generate a new dialback key generator with a random secret.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn random() -> Self {
        use rand::RngCore;
        let mut secret = [0u8; 32];
        rand::rng().fill_bytes(&mut secret);
        Self::new(secret)
    }

    /// Generate a dialback key for the given parameters.
    ///
    /// Per XEP-0220 Section 2.4, the key is computed as:
    /// ```text
    /// key = HMAC-SHA256(secret, stream_id || receiving_domain || originating_domain)
    /// ```
    ///
    /// # Arguments
    ///
    /// * `stream_id` - The stream ID assigned by the receiving server
    /// * `receiving_domain` - The domain of the receiving server (where connection was made)
    /// * `originating_domain` - The domain of the originating server (who initiated the connection)
    ///
    /// # Returns
    ///
    /// A hex-encoded dialback key string.
    pub fn generate(
        &self,
        stream_id: &str,
        receiving_domain: &str,
        originating_domain: &str,
    ) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.secret)
            .expect("HMAC can take key of any size");

        // Concatenate the input data as specified in XEP-0220
        mac.update(stream_id.as_bytes());
        mac.update(receiving_domain.as_bytes());
        mac.update(originating_domain.as_bytes());

        // Get the HMAC result and encode as hex
        let result = mac.finalize();
        hex::encode(result.into_bytes())
    }

    /// Verify a dialback key against expected parameters.
    ///
    /// # Arguments
    ///
    /// * `key` - The dialback key to verify (hex-encoded)
    /// * `stream_id` - The stream ID from the `db:verify` request
    /// * `receiving_domain` - The domain in the `to` attribute
    /// * `originating_domain` - The domain in the `from` attribute
    ///
    /// # Returns
    ///
    /// `true` if the key is valid, `false` otherwise.
    pub fn verify(
        &self,
        key: &str,
        stream_id: &str,
        receiving_domain: &str,
        originating_domain: &str,
    ) -> bool {
        let expected = self.generate(stream_id, receiving_domain, originating_domain);

        // Constant-time comparison to prevent timing attacks
        constant_time_eq(key.as_bytes(), expected.as_bytes())
    }
}

/// Constant-time comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

/// Information about a dialback request being processed.
#[derive(Debug, Clone)]
pub struct DialbackRequest {
    /// The originating domain (from attribute)
    pub from: String,
    /// The receiving domain (to attribute)
    pub to: String,
    /// The dialback key
    pub key: String,
}

impl DialbackRequest {
    /// Create a new dialback request.
    pub fn new(from: impl Into<String>, to: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            key: key.into(),
        }
    }
}

/// Information about a dialback verification request.
#[derive(Debug, Clone)]
pub struct DialbackVerify {
    /// The originating domain (from attribute)
    pub from: String,
    /// The receiving domain (to attribute)
    pub to: String,
    /// The stream ID being verified
    pub id: String,
    /// The dialback key to verify
    pub key: String,
}

impl DialbackVerify {
    /// Create a new dialback verification request.
    pub fn new(
        from: impl Into<String>,
        to: impl Into<String>,
        id: impl Into<String>,
        key: impl Into<String>,
    ) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            id: id.into(),
            key: key.into(),
        }
    }
}

/// Build a `db:result` element for sending to a remote server.
///
/// This is sent by the originating server to initiate dialback.
pub fn build_db_result(from: &str, to: &str, key: &str) -> String {
    format!(
        "<db:result xmlns:db='{}' from='{}' to='{}'>{}</db:result>",
        NS_DIALBACK, from, to, key
    )
}

/// Build a `db:result` response element (valid/invalid).
///
/// This is sent by the receiving server after verification.
pub fn build_db_result_response(from: &str, to: &str, result: DialbackResult) -> String {
    format!(
        "<db:result xmlns:db='{}' from='{}' to='{}' type='{}'/>",
        NS_DIALBACK, from, to, result.as_str()
    )
}

/// Build a `db:verify` element for sending to the authoritative server.
///
/// This is sent by the receiving server to verify a dialback key.
pub fn build_db_verify(from: &str, to: &str, id: &str, key: &str) -> String {
    format!(
        "<db:verify xmlns:db='{}' from='{}' to='{}' id='{}'>{}</db:verify>",
        NS_DIALBACK, from, to, id, key
    )
}

/// Build a `db:verify` response element (valid/invalid).
///
/// This is sent by the authoritative server in response to a `db:verify` request.
pub fn build_db_verify_response(from: &str, to: &str, id: &str, result: DialbackResult) -> String {
    format!(
        "<db:verify xmlns:db='{}' from='{}' to='{}' id='{}' type='{}'/>",
        NS_DIALBACK, from, to, id, result.as_str()
    )
}

/// Convert a hex-encoded string to bytes.
fn hex_decode(s: &str) -> Option<Vec<u8>> {
    hex::decode(s).ok()
}

/// Helper module for hex encoding.
mod hex {
    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";

    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        let bytes = bytes.as_ref();
        let mut result = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            result.push(HEX_CHARS[(byte >> 4) as usize] as char);
            result.push(HEX_CHARS[(byte & 0x0f) as usize] as char);
        }
        result
    }

    pub fn decode(s: &str) -> Result<Vec<u8>, ()> {
        if s.len() % 2 != 0 {
            return Err(());
        }

        let mut result = Vec::with_capacity(s.len() / 2);
        let bytes = s.as_bytes();

        for chunk in bytes.chunks(2) {
            let high = hex_char_to_nibble(chunk[0])?;
            let low = hex_char_to_nibble(chunk[1])?;
            result.push((high << 4) | low);
        }

        Ok(result)
    }

    fn hex_char_to_nibble(c: u8) -> Result<u8, ()> {
        match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            b'A'..=b'F' => Ok(c - b'A' + 10),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dialback_key_generation() {
        let secret = b"test-secret-key";
        let key_gen = DialbackKey::new(secret);

        let key1 = key_gen.generate("stream-id-1", "receiving.example", "originating.example");
        let key2 = key_gen.generate("stream-id-1", "receiving.example", "originating.example");

        // Same inputs should produce same key
        assert_eq!(key1, key2);

        // Different stream ID should produce different key
        let key3 = key_gen.generate("stream-id-2", "receiving.example", "originating.example");
        assert_ne!(key1, key3);

        // Different domains should produce different key
        let key4 = key_gen.generate("stream-id-1", "other.example", "originating.example");
        assert_ne!(key1, key4);
    }

    #[test]
    fn test_dialback_key_verification() {
        let secret = b"verification-secret";
        let key_gen = DialbackKey::new(secret);

        let stream_id = "stream-123";
        let receiving = "waddle.social";
        let originating = "example.com";

        let key = key_gen.generate(stream_id, receiving, originating);

        // Verification should succeed with correct parameters
        assert!(key_gen.verify(&key, stream_id, receiving, originating));

        // Verification should fail with wrong stream ID
        assert!(!key_gen.verify(&key, "wrong-stream", receiving, originating));

        // Verification should fail with wrong receiving domain
        assert!(!key_gen.verify(&key, stream_id, "wrong.domain", originating));

        // Verification should fail with wrong originating domain
        assert!(!key_gen.verify(&key, stream_id, receiving, "wrong.domain"));

        // Verification should fail with tampered key
        let mut tampered_key = key.clone();
        if let Some(last) = tampered_key.pop() {
            tampered_key.push(if last == 'a' { 'b' } else { 'a' });
        }
        assert!(!key_gen.verify(&tampered_key, stream_id, receiving, originating));
    }

    #[test]
    fn test_dialback_key_hex_encoding() {
        let secret = b"hex-test";
        let key_gen = DialbackKey::new(secret);

        let key = key_gen.generate("stream", "to.example", "from.example");

        // Key should be hex-encoded (64 chars for SHA256)
        assert_eq!(key.len(), 64);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_dialback_state() {
        assert_eq!(DialbackState::default(), DialbackState::None);
        assert_eq!(format!("{}", DialbackState::Verified), "verified");
        assert_eq!(format!("{}", DialbackState::Failed), "failed");
        assert_eq!(format!("{}", DialbackState::Pending), "pending");
    }

    #[test]
    fn test_dialback_result() {
        assert_eq!(DialbackResult::Valid.as_str(), "valid");
        assert_eq!(DialbackResult::Invalid.as_str(), "invalid");

        assert_eq!(DialbackResult::from_str("valid"), Some(DialbackResult::Valid));
        assert_eq!(DialbackResult::from_str("invalid"), Some(DialbackResult::Invalid));
        assert_eq!(DialbackResult::from_str("unknown"), None);
    }

    #[test]
    fn test_build_db_result() {
        let xml = build_db_result("originating.example", "receiving.example", "abc123");
        assert!(xml.contains("db:result"));
        assert!(xml.contains("from='originating.example'"));
        assert!(xml.contains("to='receiving.example'"));
        assert!(xml.contains("abc123"));
    }

    #[test]
    fn test_build_db_result_response() {
        let xml = build_db_result_response("receiving.example", "originating.example", DialbackResult::Valid);
        assert!(xml.contains("db:result"));
        assert!(xml.contains("type='valid'"));

        let xml = build_db_result_response("receiving.example", "originating.example", DialbackResult::Invalid);
        assert!(xml.contains("type='invalid'"));
    }

    #[test]
    fn test_build_db_verify() {
        let xml = build_db_verify("receiving.example", "originating.example", "stream-123", "key456");
        assert!(xml.contains("db:verify"));
        assert!(xml.contains("from='receiving.example'"));
        assert!(xml.contains("to='originating.example'"));
        assert!(xml.contains("id='stream-123'"));
        assert!(xml.contains("key456"));
    }

    #[test]
    fn test_build_db_verify_response() {
        let xml = build_db_verify_response("originating.example", "receiving.example", "stream-123", DialbackResult::Valid);
        assert!(xml.contains("db:verify"));
        assert!(xml.contains("id='stream-123'"));
        assert!(xml.contains("type='valid'"));
    }

    #[test]
    fn test_hex_encode_decode_roundtrip() {
        let original = b"Hello, World!";
        let encoded = hex::encode(original);
        let decoded = hex::decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"hello", b"hell"));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(constant_time_eq(b"", b""));
    }
}
