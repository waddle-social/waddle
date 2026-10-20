//! Keyed hashing for bearer tokens persisted by the server.
//!
//! Stored token hashes are HMAC-SHA256 over the token when a server key is
//! configured, and plain SHA-256 otherwise. The plaintext token is never
//! persisted.

use std::sync::Arc;

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub(crate) struct TokenHasher {
    key: Option<Arc<[u8]>>,
}

impl TokenHasher {
    pub(crate) fn new(key: Option<&[u8]>) -> Self {
        Self {
            key: key.map(Arc::from),
        }
    }

    /// Hex digest stored in `sessions.token_hash` for a session id.
    pub(crate) fn session(&self, token: &str) -> String {
        match &self.key {
            Some(key) => {
                let mut mac = HmacSha256::new_from_slice(key).expect("HMAC supports any key");
                mac.update(token.as_bytes());
                hex::encode(mac.finalize().into_bytes())
            }
            None => {
                let mut hasher = Sha256::new();
                hasher.update(token.as_bytes());
                hex::encode(hasher.finalize())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TokenHasher;

    #[test]
    fn session_hash_matches_legacy_vector() {
        // Computed from the pre-refactor `SessionManager::token_hash`; stored
        // session rows must keep validating after the move.
        assert_eq!(
            TokenHasher::new(Some(b"test-session-key")).session("abc"),
            "b350e99163562cc12897e1aa46ae392018e2e777d7b54049ab1a37d6b3f57f18"
        );
        assert_eq!(
            TokenHasher::new(None).session("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
