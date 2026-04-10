//! XEP-0300: Cryptographic Hash Functions
//!
//! Provides helpers for working with standardized hash algorithm references
//! and computing hashes. Wraps `xmpp_parsers::hashes` with computation
//! capabilities using SHA-1 and SHA-2 crates.
//!
//! ## Supported Algorithms
//!
//! | Name | XEP-0300 Algo String | Status |
//! |------|---------------------|--------|
//! | SHA-1 | `sha-1` | SHOULD NOT (legacy only) |
//! | SHA-256 | `sha-256` | MUST |
//! | SHA-512 | `sha-512` | SHOULD |
//!
//! ## XML Format
//!
//! ```xml
//! <hash xmlns='urn:xmpp:hashes:2' algo='sha-256'>base64encodedvalue=</hash>
//! ```
//!
//! ## Use Cases
//!
//! - Entity Capabilities (XEP-0115): capability hash verification
//! - HTTP File Upload (XEP-0363): file integrity
//! - User Avatar (XEP-0084): avatar hash
//! - Sticker packs, file metadata, etc.

use base64::{engine::general_purpose::STANDARD as Base64Engine, Engine as _};
use minidom::Element;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use thiserror::Error;

/// Namespace for XEP-0300 Cryptographic Hashes.
pub const NS_HASHES: &str = "urn:xmpp:hashes:2";

/// Errors that can occur during hash operations.
#[derive(Debug, Error)]
pub enum HashError {
    /// The requested algorithm is not supported.
    #[error("unsupported hash algorithm: {0}")]
    UnsupportedAlgorithm(String),
    /// Invalid base64 encoding.
    #[error("invalid base64: {0}")]
    InvalidBase64(String),
}

/// Supported hash algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HashAlgo {
    /// SHA-1 (legacy, SHOULD NOT use for new applications).
    Sha1,
    /// SHA-256 (MUST support).
    Sha256,
    /// SHA-512 (SHOULD support).
    Sha512,
}

impl HashAlgo {
    /// The XEP-0300 algorithm string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sha1 => "sha-1",
            Self::Sha256 => "sha-256",
            Self::Sha512 => "sha-512",
        }
    }

    /// Parse from an XEP-0300 algorithm string.
    pub fn from_algo_str(s: &str) -> Option<Self> {
        match s {
            "sha-1" => Some(Self::Sha1),
            "sha-256" => Some(Self::Sha256),
            "sha-512" => Some(Self::Sha512),
            _ => None,
        }
    }

    /// The output length in bytes.
    pub fn output_len(self) -> usize {
        match self {
            Self::Sha1 => 20,
            Self::Sha256 => 32,
            Self::Sha512 => 64,
        }
    }
}

impl std::fmt::Display for HashAlgo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A computed hash value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashValue {
    /// The algorithm used.
    pub algo: HashAlgo,
    /// The raw hash bytes.
    pub bytes: Vec<u8>,
}

impl HashValue {
    /// Create from raw bytes.
    pub fn new(algo: HashAlgo, bytes: Vec<u8>) -> Self {
        Self { algo, bytes }
    }

    /// Encode the hash as base64.
    pub fn to_base64(&self) -> String {
        Base64Engine.encode(&self.bytes)
    }

    /// Encode the hash as hex.
    pub fn to_hex(&self) -> String {
        self.bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Parse from base64.
    pub fn from_base64(algo: HashAlgo, b64: &str) -> Result<Self, HashError> {
        let bytes = Base64Engine
            .decode(b64)
            .map_err(|e| HashError::InvalidBase64(e.to_string()))?;
        Ok(Self { algo, bytes })
    }
}

/// Trait for types that can be hashed.
pub trait Hashable {
    /// Get the bytes to hash.
    fn hash_bytes(&self) -> &[u8];

    /// Compute a hash with the given algorithm.
    fn compute_hash(&self, algo: HashAlgo) -> HashValue {
        compute_hash(algo, self.hash_bytes())
    }

    /// Compute a SHA-256 hash.
    fn sha256(&self) -> HashValue {
        self.compute_hash(HashAlgo::Sha256)
    }

    /// Compute a SHA-1 hash (legacy).
    fn sha1(&self) -> HashValue {
        self.compute_hash(HashAlgo::Sha1)
    }
}

impl Hashable for [u8] {
    fn hash_bytes(&self) -> &[u8] {
        self
    }
}

impl Hashable for str {
    fn hash_bytes(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl Hashable for String {
    fn hash_bytes(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl Hashable for Vec<u8> {
    fn hash_bytes(&self) -> &[u8] {
        self
    }
}

// ── Computation ──────────────────────────────────────────────────────

/// Compute a hash of the given data.
pub fn compute_hash(algo: HashAlgo, data: &[u8]) -> HashValue {
    let bytes = match algo {
        HashAlgo::Sha1 => {
            let mut hasher = Sha1::new();
            hasher.update(data);
            hasher.finalize().to_vec()
        }
        HashAlgo::Sha256 => {
            let mut hasher = Sha256::new();
            hasher.update(data);
            hasher.finalize().to_vec()
        }
        HashAlgo::Sha512 => {
            let mut hasher = Sha512::new();
            hasher.update(data);
            hasher.finalize().to_vec()
        }
    };
    HashValue::new(algo, bytes)
}

/// Compute a SHA-256 hash and return as base64.
pub fn sha256_base64(data: &[u8]) -> String {
    compute_hash(HashAlgo::Sha256, data).to_base64()
}

/// Compute a SHA-256 hash and return as hex.
pub fn sha256_hex(data: &[u8]) -> String {
    compute_hash(HashAlgo::Sha256, data).to_hex()
}

/// Compute a SHA-1 hash and return as hex (legacy, for entity capabilities).
pub fn sha1_hex(data: &[u8]) -> String {
    compute_hash(HashAlgo::Sha1, data).to_hex()
}

// ── XML Building ─────────────────────────────────────────────────────

/// Build a `<hash xmlns='urn:xmpp:hashes:2' algo='...'/>` element.
pub fn build_hash_element(hash: &HashValue) -> Element {
    let mut elem = Element::builder("hash", NS_HASHES)
        .attr("algo", hash.algo.as_str())
        .build();
    elem.append_text_node(hash.to_base64());
    elem
}

/// Parse a `<hash/>` element into a HashValue.
pub fn parse_hash_element(elem: &Element) -> Result<HashValue, HashError> {
    if elem.name() != "hash" || elem.ns() != NS_HASHES {
        return Err(HashError::UnsupportedAlgorithm("not a hash element".into()));
    }
    let algo_str = elem
        .attr("algo")
        .ok_or_else(|| HashError::UnsupportedAlgorithm("missing algo attribute".into()))?;
    let algo = HashAlgo::from_algo_str(algo_str)
        .ok_or_else(|| HashError::UnsupportedAlgorithm(algo_str.into()))?;
    let b64 = elem.text();
    HashValue::from_base64(algo, &b64)
}

/// Verify that a hash matches expected data.
pub fn verify_hash(hash: &HashValue, data: &[u8]) -> bool {
    let computed = compute_hash(hash.algo, data);
    computed.bytes == hash.bytes
}

// ── Conversion ───────────────────────────────────────────────────────

impl From<xmpp_parsers::hashes::Hash> for HashValue {
    fn from(h: xmpp_parsers::hashes::Hash) -> Self {
        let algo = match h.algo {
            xmpp_parsers::hashes::Algo::Sha_1 => HashAlgo::Sha1,
            xmpp_parsers::hashes::Algo::Sha_256 => HashAlgo::Sha256,
            xmpp_parsers::hashes::Algo::Sha_512 => HashAlgo::Sha512,
            _ => HashAlgo::Sha256, // fallback
        };
        Self::new(algo, h.hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_known_vector() {
        // SHA-256 of empty string
        let hash = compute_hash(HashAlgo::Sha256, b"");
        assert_eq!(
            hash.to_hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_sha256_hello() {
        let hash = compute_hash(HashAlgo::Sha256, b"hello");
        assert_eq!(
            hash.to_hex(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn test_sha1_known_vector() {
        let hash = compute_hash(HashAlgo::Sha1, b"hello");
        assert_eq!(hash.to_hex(), "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d");
    }

    #[test]
    fn test_sha512_output_length() {
        let hash = compute_hash(HashAlgo::Sha512, b"test");
        assert_eq!(hash.bytes.len(), 64);
    }

    #[test]
    fn test_hash_algo_roundtrip() {
        for algo in [HashAlgo::Sha1, HashAlgo::Sha256, HashAlgo::Sha512] {
            let s = algo.as_str();
            assert_eq!(HashAlgo::from_algo_str(s), Some(algo));
        }
    }

    #[test]
    fn test_hash_algo_unknown() {
        assert_eq!(HashAlgo::from_algo_str("md5"), None);
    }

    #[test]
    fn test_hash_algo_display() {
        assert_eq!(HashAlgo::Sha256.to_string(), "sha-256");
    }

    #[test]
    fn test_hash_algo_output_len() {
        assert_eq!(HashAlgo::Sha1.output_len(), 20);
        assert_eq!(HashAlgo::Sha256.output_len(), 32);
        assert_eq!(HashAlgo::Sha512.output_len(), 64);
    }

    #[test]
    fn test_base64_roundtrip() {
        let hash = compute_hash(HashAlgo::Sha256, b"test data");
        let b64 = hash.to_base64();
        let parsed = HashValue::from_base64(HashAlgo::Sha256, &b64).expect("valid base64");
        assert_eq!(parsed.bytes, hash.bytes);
    }

    #[test]
    fn test_base64_invalid() {
        let err = HashValue::from_base64(HashAlgo::Sha256, "not valid base64!!!")
            .expect_err("should fail");
        assert!(err.to_string().contains("base64"));
    }

    #[test]
    fn test_build_hash_element() {
        let hash = compute_hash(HashAlgo::Sha256, b"hello");
        let elem = build_hash_element(&hash);

        assert_eq!(elem.name(), "hash");
        assert_eq!(elem.ns(), NS_HASHES);
        assert_eq!(elem.attr("algo"), Some("sha-256"));
        assert!(!elem.text().is_empty());
    }

    #[test]
    fn test_parse_hash_element() {
        let hash = compute_hash(HashAlgo::Sha256, b"hello");
        let elem = build_hash_element(&hash);

        let parsed = parse_hash_element(&elem).expect("parseable");
        assert_eq!(parsed.algo, HashAlgo::Sha256);
        assert_eq!(parsed.bytes, hash.bytes);
    }

    #[test]
    fn test_verify_hash() {
        let hash = compute_hash(HashAlgo::Sha256, b"test");
        assert!(verify_hash(&hash, b"test"));
        assert!(!verify_hash(&hash, b"other"));
    }

    #[test]
    fn test_hashable_trait() {
        let h1 = "hello".sha256();
        let h2 = b"hello".sha256();
        assert_eq!(h1.bytes, h2.bytes);

        let h3 = "hello".sha1();
        assert_eq!(h3.algo, HashAlgo::Sha1);
    }

    #[test]
    fn test_convenience_functions() {
        let b64 = sha256_base64(b"hello");
        assert!(!b64.is_empty());

        let hex = sha256_hex(b"hello");
        assert_eq!(hex.len(), 64); // 32 bytes = 64 hex chars

        let hex1 = sha1_hex(b"hello");
        assert_eq!(hex1.len(), 40); // 20 bytes = 40 hex chars
    }

    #[test]
    fn test_conversion_from_xmpp_parsers() {
        let parser_hash =
            xmpp_parsers::hashes::Hash::new(xmpp_parsers::hashes::Algo::Sha_256, vec![1, 2, 3]);
        let local: HashValue = parser_hash.into();
        assert_eq!(local.algo, HashAlgo::Sha256);
        assert_eq!(local.bytes, vec![1, 2, 3]);
    }
}
