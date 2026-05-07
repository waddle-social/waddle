use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use tracing::{debug, warn};

use super::{IsrToken, DEFAULT_TOKEN_VALIDITY_SECS, MAX_TOKEN_VALIDITY_SECS};

/// ISR token store for managing resumption tokens.
///
/// This store maintains the mapping between tokens and session state.
/// It provides thread-safe access and automatic expiration handling.
#[derive(Debug)]
pub struct IsrTokenStore {
    /// Tokens indexed by token string
    tokens: RwLock<HashMap<String, IsrToken>>,
    /// Default token validity in seconds
    default_validity: u64,
    /// Maximum number of tokens to store (prevents unbounded growth)
    max_tokens: usize,
}

impl Default for IsrTokenStore {
    fn default() -> Self {
        Self::new()
    }
}

impl IsrTokenStore {
    /// Create a new token store with default settings.
    pub fn new() -> Self {
        Self {
            tokens: RwLock::new(HashMap::new()),
            default_validity: DEFAULT_TOKEN_VALIDITY_SECS,
            max_tokens: 10000,
        }
    }

    /// Create a token store with custom settings.
    pub fn with_config(default_validity_secs: u64, max_tokens: usize) -> Self {
        Self {
            tokens: RwLock::new(HashMap::new()),
            default_validity: default_validity_secs.min(MAX_TOKEN_VALIDITY_SECS),
            max_tokens,
        }
    }

    /// Create and store a new token for a session.
    pub fn create_token(&self, user_id: String, jid: jid::BareJid) -> IsrToken {
        let token = IsrToken::new(user_id, jid, self.default_validity);
        self.store_token(token.clone());
        token
    }

    /// Create and store a token with SM state.
    pub fn create_token_with_sm(
        &self,
        user_id: String,
        jid: jid::BareJid,
        sm_stream_id: String,
        inbound_count: u32,
        outbound_count: u32,
    ) -> IsrToken {
        let token = IsrToken::with_sm_state(
            user_id,
            jid,
            self.default_validity,
            sm_stream_id,
            inbound_count,
            outbound_count,
        );
        self.store_token(token.clone());
        token
    }

    /// Store a token in the store.
    fn store_token(&self, token: IsrToken) {
        let mut tokens = self.tokens.write().unwrap_or_else(|e| {
            warn!("ISR token store write lock was poisoned, recovering");
            e.into_inner()
        });

        // Clean up expired tokens if we're at capacity
        if tokens.len() >= self.max_tokens {
            self.cleanup_expired_internal(&mut tokens);
        }

        // If still at capacity, remove oldest token
        if tokens.len() >= self.max_tokens {
            if let Some(oldest_key) = tokens
                .iter()
                .min_by_key(|(_, t)| t.created_at)
                .map(|(k, _)| k.clone())
            {
                tokens.remove(&oldest_key);
            }
        }

        debug!(token_id = %&token.token[..token.token.len().min(8)], "Storing ISR token");
        tokens.insert(token.token.clone(), token);
    }

    /// Validate and retrieve a token.
    ///
    /// Returns the token if valid, or None if expired/not found.
    /// The token is NOT removed - use `consume_token` to remove after successful resume.
    pub fn validate_token(&self, token_str: &str) -> Option<IsrToken> {
        let tokens = self.tokens.read().unwrap_or_else(|e| {
            warn!("ISR token store read lock was poisoned, recovering");
            e.into_inner()
        });

        match tokens.get(token_str) {
            Some(token) => {
                if token.is_expired() {
                    debug!(token_id = %&token_str[..token_str.len().min(8)], "ISR token expired");
                    None
                } else {
                    debug!(token_id = %&token_str[..token_str.len().min(8)], "ISR token valid");
                    Some(token.clone())
                }
            }
            None => {
                debug!(token_id = %&token_str[..token_str.len().min(8)], "ISR token not found");
                None
            }
        }
    }

    /// Consume (remove) a token after successful resumption.
    ///
    /// This prevents token reuse.
    pub fn consume_token(&self, token_str: &str) -> Option<IsrToken> {
        let mut tokens = self.tokens.write().unwrap_or_else(|e| {
            warn!("ISR token store write lock was poisoned, recovering");
            e.into_inner()
        });
        let token = tokens.remove(token_str);

        if token.is_some() {
            debug!(token_id = %&token_str[..token_str.len().min(8)], "ISR token consumed");
        }

        token
    }

    /// Update SM state for an existing token.
    pub fn update_sm_state(&self, token_str: &str, inbound: u32, outbound: u32) -> bool {
        let mut tokens = self.tokens.write().unwrap_or_else(|e| {
            warn!("ISR token store write lock was poisoned, recovering");
            e.into_inner()
        });

        if let Some(token) = tokens.get_mut(token_str) {
            token.update_sm_state(inbound, outbound);
            debug!(
                token_id = %&token_str[..token_str.len().min(8)],
                inbound = inbound,
                outbound = outbound,
                "Updated ISR token SM state"
            );
            true
        } else {
            false
        }
    }

    /// Refresh a token, returning a new token with extended validity.
    ///
    /// The old token is invalidated and a new one is created.
    pub fn refresh_token(&self, old_token_str: &str) -> Option<IsrToken> {
        // First validate and get the old token
        let old_token = {
            let tokens = self.tokens.read().unwrap_or_else(|e| {
                warn!("ISR token store read lock was poisoned, recovering");
                e.into_inner()
            });
            tokens.get(old_token_str).cloned()
        };

        match old_token {
            Some(old) if !old.is_expired() => {
                // Create new token with same session info
                let new_token = IsrToken::with_sm_state(
                    old.user_id,
                    old.jid,
                    self.default_validity,
                    old.sm_stream_id.unwrap_or_default(),
                    old.sm_inbound_count,
                    old.sm_outbound_count,
                );

                // Store new token
                self.store_token(new_token.clone());

                // Remove old token
                {
                    let mut tokens = self.tokens.write().unwrap_or_else(|e| {
                        warn!("ISR token store write lock was poisoned, recovering");
                        e.into_inner()
                    });
                    tokens.remove(old_token_str);
                }

                debug!(
                    old_token = %&old_token_str[..old_token_str.len().min(8)],
                    new_token = %&new_token.token[..new_token.token.len().min(8)],
                    "Refreshed ISR token"
                );

                Some(new_token)
            }
            Some(_) => {
                warn!(token_id = %&old_token_str[..old_token_str.len().min(8)], "Cannot refresh expired ISR token");
                None
            }
            None => {
                warn!(token_id = %&old_token_str[..old_token_str.len().min(8)], "Cannot refresh unknown ISR token");
                None
            }
        }
    }

    /// Remove all tokens for a specific user identifier (e.g., on logout).
    pub fn revoke_tokens_for_user_id(&self, user_id: &str) {
        let mut tokens = self.tokens.write().unwrap_or_else(|e| {
            warn!("ISR token store write lock was poisoned, recovering");
            e.into_inner()
        });
        let initial_count = tokens.len();
        tokens.retain(|_, t| t.user_id != user_id);
        let removed = initial_count - tokens.len();

        if removed > 0 {
            debug!(user_id = %user_id, removed = removed, "Revoked ISR tokens for user");
        }
    }

    /// Clean up expired tokens.
    pub fn cleanup_expired(&self) {
        let mut tokens = self.tokens.write().unwrap_or_else(|e| {
            warn!("ISR token store write lock was poisoned, recovering");
            e.into_inner()
        });
        self.cleanup_expired_internal(&mut tokens);
    }

    /// Internal cleanup helper (requires write lock already held).
    fn cleanup_expired_internal(&self, tokens: &mut HashMap<String, IsrToken>) {
        let initial_count = tokens.len();
        tokens.retain(|_, t| !t.is_expired());
        let removed = initial_count - tokens.len();

        if removed > 0 {
            debug!(removed = removed, "Cleaned up expired ISR tokens");
        }
    }

    /// Get the number of active tokens.
    pub fn token_count(&self) -> usize {
        self.tokens
            .read()
            .unwrap_or_else(|e| {
                warn!("ISR token store read lock was poisoned, recovering");
                e.into_inner()
            })
            .len()
    }
}

/// Shared ISR token store that can be used across connections.
pub type SharedIsrTokenStore = Arc<IsrTokenStore>;

/// Create a new shared ISR token store.
pub fn create_shared_store() -> SharedIsrTokenStore {
    Arc::new(IsrTokenStore::new())
}

/// Create a shared store with custom configuration.
pub fn create_shared_store_with_config(
    validity_secs: u64,
    max_tokens: usize,
) -> SharedIsrTokenStore {
    Arc::new(IsrTokenStore::with_config(validity_secs, max_tokens))
}
