//! Server Dialback keys (XEP-0185) with the deployment's secret (ADR-0020
//! D2; gate G5).
//!
//! The key for a stream is
//! `HMAC-SHA256(hex(SHA256(secret)), receiving ' ' originating ' ' stream-id)`
//! in lowercase hexadecimal, as XEP-0185 §2 recommends and its §3 example
//! computes, with both domains in the identity profile's canonical form.
//! The secret belongs to one deployment: every replica that can answer
//! verification requests holds it, so a key one replica issued verifies on
//! any other without shared state, and no other deployment holds it. Keys
//! are compared in constant time and never logged.
//!
//! Keys are not bound to the TLS channel that carries them. A party that
//! obtains the key this deployment issued for a peer's stream ID, for
//! example from a man-in-the-middle position against a peer that does not
//! validate this deployment's certificate, is accepted by that peer as
//! this deployment for that stream. This is the residual risk recorded in
//! `server/docs/planning/s2s-dialback-2047.md`.

use std::fmt;

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::s2s::{DialbackKey, DialbackKeys, StreamId};
use zeroize::{Zeroize, Zeroizing};

/// The deployment's dialback secret, held as the HMAC key XEP-0185 derives
/// from it: the lowercase hexadecimal SHA-256 digest of the secret.
pub struct DialbackSecret {
    hmac_key: Zeroizing<String>,
}

impl DialbackSecret {
    /// Derive the HMAC key from the secret's bytes.
    pub fn new(secret: &[u8]) -> Self {
        let mut digest = Sha256::digest(secret);
        let hmac_key = Zeroizing::new(hex::encode(digest.as_slice()));
        digest.as_mut_slice().zeroize();
        Self { hmac_key }
    }

    /// The MAC over XEP-0185's input, or `None` if the HMAC cannot be
    /// keyed, which HMAC's arbitrary key length rules out.
    fn mac(
        &self,
        receiving: &CanonicalDomain,
        originating: &CanonicalDomain,
        stream_id: &StreamId,
    ) -> Option<Vec<u8>> {
        let mut mac = Hmac::<Sha256>::new_from_slice(self.hmac_key.as_bytes()).ok()?;
        mac.update(receiving.as_str().as_bytes());
        mac.update(b" ");
        mac.update(originating.as_str().as_bytes());
        mac.update(b" ");
        mac.update(stream_id.as_str().as_bytes());
        Some(mac.finalize().into_bytes().to_vec())
    }
}

impl fmt::Debug for DialbackSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DialbackSecret(..)")
    }
}

impl DialbackKeys for DialbackSecret {
    fn key(
        &self,
        receiving: &CanonicalDomain,
        originating: &CanonicalDomain,
        stream_id: &StreamId,
    ) -> Option<DialbackKey> {
        let mac = self.mac(receiving, originating, stream_id)?;
        DialbackKey::parse(&hex::encode(mac))
    }

    fn verify(
        &self,
        receiving: &CanonicalDomain,
        originating: &CanonicalDomain,
        stream_id: &StreamId,
        key: &DialbackKey,
    ) -> bool {
        let Some(expected) = self.key(receiving, originating, stream_id) else {
            return false;
        };
        // Hexadecimal is case-insensitive; this deployment issues lowercase.
        let presented = key.as_str().to_ascii_lowercase();
        let presented = presented.as_bytes();
        let expected = expected.as_str().as_bytes();
        presented.len() == expected.len() && bool::from(presented.ct_eq(expected))
    }
}
