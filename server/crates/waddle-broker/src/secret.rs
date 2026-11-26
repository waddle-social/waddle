//! Zeroizing secret containers with redacted `Debug` output.
//!
//! Neither [`SecretBytes`] nor [`Key256`] implements `Display`, `Serialize`, or
//! `Deserialize` so plaintext credential material cannot accidentally serialize
//! into logs, tracing spans, XMPP stanzas, or storage rows.

use std::fmt;

use ring::rand::{SecureRandom, SystemRandom};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::error::CryptoError;

/// Maximum permitted length of a provider credential secret (16 KiB).
pub const MAX_SECRET_BYTES: usize = 16 * 1024;

/// Zeroizing container for arbitrary credential plaintext bytes.
#[derive(Clone)]
pub struct SecretBytes {
    inner: Zeroizing<Vec<u8>>,
}

impl SecretBytes {
    /// Validates non-empty and `<= MAX_SECRET_BYTES` bounds before wrapping.
    pub fn new(bytes: Vec<u8>) -> Result<Self, CryptoError> {
        if bytes.is_empty() {
            return Err(CryptoError::EmptySecret);
        }
        if bytes.len() > MAX_SECRET_BYTES {
            return Err(CryptoError::SecretTooLarge {
                max: MAX_SECRET_BYTES,
            });
        }
        Ok(Self {
            inner: Zeroizing::new(bytes),
        })
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self, CryptoError> {
        Self::new(bytes.to_vec())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.inner
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl PartialEq for SecretBytes {
    fn eq(&self, other: &Self) -> bool {
        bool::from(self.inner.as_slice().ct_eq(other.inner.as_slice()))
    }
}

impl Eq for SecretBytes {}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretBytes([REDACTED; {} bytes])", self.inner.len())
    }
}

/// Zeroizing 256-bit symmetric key (KEK/DEK/CEK).
#[derive(Clone)]
pub struct Key256 {
    inner: Zeroizing<[u8; 32]>,
}

impl Key256 {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self {
            inner: Zeroizing::new(bytes),
        }
    }

    pub fn generate() -> Result<Self, CryptoError> {
        let mut buf = [0u8; 32];
        SystemRandom::new()
            .fill(&mut buf)
            .map_err(|_| CryptoError::RngFailure)?;
        Ok(Self::from_bytes(buf))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.inner
    }
}

impl PartialEq for Key256 {
    fn eq(&self, other: &Self) -> bool {
        bool::from(self.inner.as_slice().ct_eq(other.inner.as_slice()))
    }
}

impl Eq for Key256 {}

impl fmt::Debug for Key256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Key256([REDACTED])")
    }
}
