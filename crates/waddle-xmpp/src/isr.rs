//! XEP-0397 Instant Stream Resumption Implementation
//!
//! This module implements Instant Stream Resumption as defined in XEP-0397,
//! providing:
//!
//! - Resumption tokens that allow reconnecting without re-authenticating
//! - Token delivery in SASL success responses
//! - Token refresh mechanism during active sessions
//! - Integration with XEP-0198 Stream Management for stream state preservation
//!
//! ## Protocol Overview
//!
//! ISR adds the following elements in the `urn:xmpp:isr:0` namespace:
//! - `<isr/>` - Feature advertisement in stream features
//! - `<token/>` - Resumption token (in SASL success or refresh response)
//! - `<token-request/>` - Request for a new token
//!
//! ## Flow
//!
//! 1. Server advertises `<isr xmlns='urn:xmpp:isr:0'/>` in stream features
//! 2. After SASL success, server includes `<token xmlns='urn:xmpp:isr:0' expiry='...'>`
//! 3. On reconnect, client sends SM `<resume/>` with the token as `previd`
//! 4. Server validates token and resumes stream without requiring SASL
//! 5. Client can request token refresh during active session

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use chrono::{DateTime, Utc};
use tracing::{debug, warn};

/// XEP-0397 Instant Stream Resumption namespace
pub const ISR_NS: &str = "urn:xmpp:isr:0";

/// Default token validity duration (5 minutes)
pub const DEFAULT_TOKEN_VALIDITY_SECS: u64 = 300;

/// Maximum token validity duration (24 hours)
pub const MAX_TOKEN_VALIDITY_SECS: u64 = 86400;

/// ISR token for stream resumption.
///
/// This token allows a client to resume a stream without re-authenticating.
/// It contains all the information needed to restore the session state.
#[derive(Debug, Clone)]
pub struct IsrToken {
    /// The token string (opaque to clients)
    pub token: String,
    /// When the token expires
    pub expiry: DateTime<Utc>,
    /// Associated session DID
    pub did: String,
    /// Associated JID
    pub jid: jid::BareJid,
    /// Stream Management stream ID (for SM state restoration)
    pub sm_stream_id: Option<String>,
    /// Last known inbound stanza count (for SM)
    pub sm_inbound_count: u32,
    /// Last known outbound stanza count (for SM)
    pub sm_outbound_count: u32,
    /// When the token was created
    pub created_at: Instant,
}

impl IsrToken {
    /// Create a new ISR token.
    pub fn new(
        did: String,
        jid: jid::BareJid,
        validity_secs: u64,
    ) -> Self {
        let validity = validity_secs.min(MAX_TOKEN_VALIDITY_SECS);
        let token = generate_token();
        let expiry = Utc::now() + chrono::Duration::seconds(validity as i64);

        Self {
            token,
            expiry,
            did,
            jid,
            sm_stream_id: None,
            sm_inbound_count: 0,
            sm_outbound_count: 0,
            created_at: Instant::now(),
        }
    }

    /// Create a token with SM state.
    pub fn with_sm_state(
        did: String,
        jid: jid::BareJid,
        validity_secs: u64,
        sm_stream_id: String,
        inbound_count: u32,
        outbound_count: u32,
    ) -> Self {
        let mut token = Self::new(did, jid, validity_secs);
        token.sm_stream_id = Some(sm_stream_id);
        token.sm_inbound_count = inbound_count;
        token.sm_outbound_count = outbound_count;
        token
    }

    /// Check if the token has expired.
    pub fn is_expired(&self) -> bool {
        Utc::now() > self.expiry
    }

    /// Get remaining validity in seconds.
    pub fn remaining_secs(&self) -> i64 {
        (self.expiry - Utc::now()).num_seconds().max(0)
    }

    /// Update SM state in the token.
    pub fn update_sm_state(&mut self, inbound: u32, outbound: u32) {
        self.sm_inbound_count = inbound;
        self.sm_outbound_count = outbound;
    }

    /// Generate XML for the token element.
    ///
    /// Format: `<token xmlns='urn:xmpp:isr:0' expiry='ISO8601'>TOKEN</token>`
    pub fn to_xml(&self) -> String {
        format!(
            "<token xmlns='{}' expiry='{}'>{}</token>",
            ISR_NS,
            self.expiry.to_rfc3339(),
            self.token
        )
    }
}

/// Generate a secure random token.
fn generate_token() -> String {
    use std::time::SystemTime;

    // Generate a token from UUID + timestamp for uniqueness
    let uuid = uuid::Uuid::new_v4();
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    // Create a base64-encoded token
    let combined = format!("{}-{:x}", uuid, ts);
    base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, combined.as_bytes())
}

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
    pub fn create_token(&self, did: String, jid: jid::BareJid) -> IsrToken {
        let token = IsrToken::new(did, jid, self.default_validity);
        self.store_token(token.clone());
        token
    }

    /// Create and store a token with SM state.
    pub fn create_token_with_sm(
        &self,
        did: String,
        jid: jid::BareJid,
        sm_stream_id: String,
        inbound_count: u32,
        outbound_count: u32,
    ) -> IsrToken {
        let token = IsrToken::with_sm_state(
            did,
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
        let mut tokens = self.tokens.write().expect("ISR token store lock poisoned");

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
        let tokens = self.tokens.read().expect("ISR token store lock poisoned");

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
        let mut tokens = self.tokens.write().expect("ISR token store lock poisoned");
        let token = tokens.remove(token_str);

        if token.is_some() {
            debug!(token_id = %&token_str[..token_str.len().min(8)], "ISR token consumed");
        }

        token
    }

    /// Update SM state for an existing token.
    pub fn update_sm_state(&self, token_str: &str, inbound: u32, outbound: u32) -> bool {
        let mut tokens = self.tokens.write().expect("ISR token store lock poisoned");

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
            let tokens = self.tokens.read().expect("ISR token store lock poisoned");
            tokens.get(old_token_str).cloned()
        };

        match old_token {
            Some(old) if !old.is_expired() => {
                // Create new token with same session info
                let new_token = IsrToken::with_sm_state(
                    old.did,
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
                    let mut tokens = self.tokens.write().expect("ISR token store lock poisoned");
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

    /// Remove all tokens for a specific DID (e.g., on logout).
    pub fn revoke_tokens_for_did(&self, did: &str) {
        let mut tokens = self.tokens.write().expect("ISR token store lock poisoned");
        let initial_count = tokens.len();
        tokens.retain(|_, t| t.did != did);
        let removed = initial_count - tokens.len();

        if removed > 0 {
            debug!(did = %did, removed = removed, "Revoked ISR tokens for DID");
        }
    }

    /// Clean up expired tokens.
    pub fn cleanup_expired(&self) {
        let mut tokens = self.tokens.write().expect("ISR token store lock poisoned");
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
        self.tokens.read().expect("ISR token store lock poisoned").len()
    }
}

/// ISR-aware SASL success response builder.
///
/// Generates a SASL success response that includes the ISR token.
pub fn build_sasl_success_with_isr(token: &IsrToken) -> String {
    format!(
        "<success xmlns='urn:ietf:params:xml:ns:xmpp-sasl'>{}</success>",
        token.to_xml()
    )
}

/// Parse an ISR token from a SASL success response or token element.
pub fn parse_isr_token(xml: &str) -> Option<(String, DateTime<Utc>)> {
    // Look for token element
    if !xml.contains("<token") || !xml.contains(ISR_NS) {
        return None;
    }

    // Extract token content
    let token_start = xml.find("<token")?;
    let content_start = xml[token_start..].find('>')? + token_start + 1;
    let content_end = xml[content_start..].find("</token>")? + content_start;
    let token = xml[content_start..content_end].trim().to_string();

    // Extract expiry attribute
    let expiry_str = extract_attr(&xml[token_start..], "expiry")?;
    let expiry = DateTime::parse_from_rfc3339(&expiry_str)
        .ok()?
        .with_timezone(&Utc);

    Some((token, expiry))
}

/// Extract an attribute value from XML.
fn extract_attr(xml: &str, name: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let pattern = format!("{}={}", name, quote);
        if let Some(start) = xml.find(&pattern) {
            let value_start = start + pattern.len();
            if let Some(value_end) = xml[value_start..].find(quote) {
                return Some(xml[value_start..value_start + value_end].to_string());
            }
        }
    }
    None
}

/// Shared ISR token store that can be used across connections.
pub type SharedIsrTokenStore = Arc<IsrTokenStore>;

/// Create a new shared ISR token store.
pub fn create_shared_store() -> SharedIsrTokenStore {
    Arc::new(IsrTokenStore::new())
}

/// Create a shared store with custom configuration.
pub fn create_shared_store_with_config(validity_secs: u64, max_tokens: usize) -> SharedIsrTokenStore {
    Arc::new(IsrTokenStore::with_config(validity_secs, max_tokens))
}

/// Check if an IQ is an ISR token-request.
///
/// Returns true if the IQ contains:
/// ```xml
/// <iq type='get'>
///   <token-request xmlns='urn:xmpp:isr:0'/>
/// </iq>
/// ```
///
/// Per XEP-0397 §4, clients can request a new token during an active session
/// using this IQ stanza.
pub fn is_isr_token_request(iq: &xmpp_parsers::iq::Iq) -> bool {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Get(elem) => {
            elem.name() == "token-request" && elem.ns() == ISR_NS
        }
        _ => false,
    }
}

/// Build an IQ result containing a new ISR token.
///
/// Response format per XEP-0397:
/// ```xml
/// <iq type='result' id='...'>
///   <token xmlns='urn:xmpp:isr:0' expiry='ISO8601'>NEW_TOKEN</token>
/// </iq>
/// ```
pub fn build_isr_token_result(original_iq: &xmpp_parsers::iq::Iq, token: &IsrToken) -> xmpp_parsers::iq::Iq {
    use minidom::Element;

    let token_elem = Element::builder("token", ISR_NS)
        .attr("expiry", token.expiry.to_rfc3339())
        .append(token.token.clone())
        .build();

    xmpp_parsers::iq::Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: xmpp_parsers::iq::IqType::Result(Some(token_elem)),
    }
}

/// Build an IQ error for ISR token-request failure.
///
/// Returns an error IQ when token refresh is not possible.
///
/// Supports the following conditions:
/// - "not-authorized": Session not established or JID not bound
/// - "service-unavailable": Token refresh not available
pub fn build_isr_token_error(original_iq: &xmpp_parsers::iq::Iq, condition: &str) -> xmpp_parsers::iq::Iq {
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

    let (error_type, defined_condition) = match condition {
        "not-authorized" => (ErrorType::Auth, DefinedCondition::NotAuthorized),
        "service-unavailable" => (ErrorType::Cancel, DefinedCondition::ServiceUnavailable),
        "item-not-found" => (ErrorType::Cancel, DefinedCondition::ItemNotFound),
        _ => (ErrorType::Cancel, DefinedCondition::UndefinedCondition),
    };

    let stanza_error = StanzaError::new(
        error_type,
        defined_condition,
        "en",
        "",  // Empty text
    );

    xmpp_parsers::iq::Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: xmpp_parsers::iq::IqType::Error(stanza_error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;

    #[test]
    fn test_token_creation() {
        let token = IsrToken::new(
            "did:plc:test123".to_string(),
            "user@example.com".parse().unwrap(),
            300,
        );

        assert!(!token.is_expired());
        assert!(token.remaining_secs() > 290);
        assert!(token.remaining_secs() <= 300);
        assert!(!token.token.is_empty());
    }

    #[test]
    fn test_token_with_sm_state() {
        let token = IsrToken::with_sm_state(
            "did:plc:test123".to_string(),
            "user@example.com".parse().unwrap(),
            300,
            "stream-123".to_string(),
            10,
            20,
        );

        assert_eq!(token.sm_stream_id, Some("stream-123".to_string()));
        assert_eq!(token.sm_inbound_count, 10);
        assert_eq!(token.sm_outbound_count, 20);
    }

    #[test]
    fn test_token_xml() {
        let token = IsrToken::new(
            "did:plc:test123".to_string(),
            "user@example.com".parse().unwrap(),
            300,
        );

        let xml = token.to_xml();
        assert!(xml.contains("xmlns='urn:xmpp:isr:0'"));
        assert!(xml.contains("expiry='"));
        assert!(xml.contains(&token.token));
    }

    #[test]
    fn test_token_store_create_and_validate() {
        let store = IsrTokenStore::new();

        let token = store.create_token(
            "did:plc:test123".to_string(),
            "user@example.com".parse().unwrap(),
        );

        // Should be able to validate
        let validated = store.validate_token(&token.token);
        assert!(validated.is_some());
        assert_eq!(validated.unwrap().did, "did:plc:test123");
    }

    #[test]
    fn test_token_store_consume() {
        let store = IsrTokenStore::new();

        let token = store.create_token(
            "did:plc:test123".to_string(),
            "user@example.com".parse().unwrap(),
        );

        // First consume should succeed
        let consumed = store.consume_token(&token.token);
        assert!(consumed.is_some());

        // Second consume should fail
        let consumed_again = store.consume_token(&token.token);
        assert!(consumed_again.is_none());

        // Validation should also fail
        let validated = store.validate_token(&token.token);
        assert!(validated.is_none());
    }

    #[test]
    fn test_token_store_refresh() {
        let store = IsrTokenStore::new();

        let old_token = store.create_token(
            "did:plc:test123".to_string(),
            "user@example.com".parse().unwrap(),
        );
        let old_token_str = old_token.token.clone();

        // Refresh should return a new token
        let new_token = store.refresh_token(&old_token_str);
        assert!(new_token.is_some());
        let new_token = new_token.unwrap();

        // New token should be different
        assert_ne!(new_token.token, old_token_str);

        // Old token should be invalid
        assert!(store.validate_token(&old_token_str).is_none());

        // New token should be valid
        assert!(store.validate_token(&new_token.token).is_some());
    }

    #[test]
    fn test_token_store_revoke_for_did() {
        let store = IsrTokenStore::new();

        // Create tokens for two different DIDs
        let _token1 = store.create_token(
            "did:plc:user1".to_string(),
            "user1@example.com".parse().unwrap(),
        );
        let token2 = store.create_token(
            "did:plc:user2".to_string(),
            "user2@example.com".parse().unwrap(),
        );

        assert_eq!(store.token_count(), 2);

        // Revoke tokens for user1
        store.revoke_tokens_for_did("did:plc:user1");

        assert_eq!(store.token_count(), 1);

        // user2's token should still be valid
        assert!(store.validate_token(&token2.token).is_some());
    }

    #[test]
    fn test_parse_isr_token() {
        let xml = "<token xmlns='urn:xmpp:isr:0' expiry='2024-01-01T12:00:00Z'>test-token-123</token>";

        let result = parse_isr_token(xml);
        assert!(result.is_some());

        let (token, expiry) = result.unwrap();
        assert_eq!(token, "test-token-123");
        assert_eq!(expiry.year(), 2024);
    }

    #[test]
    fn test_sasl_success_with_isr() {
        let token = IsrToken::new(
            "did:plc:test123".to_string(),
            "user@example.com".parse().unwrap(),
            300,
        );

        let success = build_sasl_success_with_isr(&token);

        assert!(success.contains("<success"));
        assert!(success.contains("urn:ietf:params:xml:ns:xmpp-sasl"));
        assert!(success.contains("<token"));
        assert!(success.contains("urn:xmpp:isr:0"));
    }

    #[test]
    fn test_update_sm_state() {
        let store = IsrTokenStore::new();

        let token = store.create_token_with_sm(
            "did:plc:test123".to_string(),
            "user@example.com".parse().unwrap(),
            "stream-123".to_string(),
            0,
            0,
        );

        // Update SM state
        assert!(store.update_sm_state(&token.token, 10, 20));

        // Verify update
        let validated = store.validate_token(&token.token).unwrap();
        assert_eq!(validated.sm_inbound_count, 10);
        assert_eq!(validated.sm_outbound_count, 20);
    }

    #[test]
    fn test_is_isr_token_request() {
        use minidom::Element;
        use xmpp_parsers::iq::{Iq, IqType};

        // Valid token-request IQ
        let token_request_elem = Element::builder("token-request", ISR_NS).build();
        let iq = Iq {
            from: Some("user@example.com/resource".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "token-1".to_string(),
            payload: IqType::Get(token_request_elem),
        };

        assert!(is_isr_token_request(&iq));
    }

    #[test]
    fn test_is_not_isr_token_request_wrong_ns() {
        use minidom::Element;
        use xmpp_parsers::iq::{Iq, IqType};

        // Wrong namespace
        let token_request_elem = Element::builder("token-request", "wrong:ns").build();
        let iq = Iq {
            from: None,
            to: None,
            id: "token-1".to_string(),
            payload: IqType::Get(token_request_elem),
        };

        assert!(!is_isr_token_request(&iq));
    }

    #[test]
    fn test_is_not_isr_token_request_wrong_element() {
        use minidom::Element;
        use xmpp_parsers::iq::{Iq, IqType};

        // Wrong element name
        let other_elem = Element::builder("other", ISR_NS).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "token-1".to_string(),
            payload: IqType::Get(other_elem),
        };

        assert!(!is_isr_token_request(&iq));
    }

    #[test]
    fn test_is_not_isr_token_request_set_type() {
        use minidom::Element;
        use xmpp_parsers::iq::{Iq, IqType};

        // Set type instead of Get
        let token_request_elem = Element::builder("token-request", ISR_NS).build();
        let iq = Iq {
            from: None,
            to: None,
            id: "token-1".to_string(),
            payload: IqType::Set(token_request_elem),
        };

        assert!(!is_isr_token_request(&iq));
    }

    #[test]
    fn test_build_isr_token_result() {
        use minidom::Element;
        use xmpp_parsers::iq::{Iq, IqType};

        let token_request_elem = Element::builder("token-request", ISR_NS).build();
        let original_iq = Iq {
            from: Some("user@example.com/resource".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "token-1".to_string(),
            payload: IqType::Get(token_request_elem),
        };

        let isr_token = IsrToken::new(
            "did:plc:test123".to_string(),
            "user@example.com".parse().unwrap(),
            300,
        );

        let result = build_isr_token_result(&original_iq, &isr_token);

        assert_eq!(result.id, "token-1");
        assert_eq!(
            result.from.as_ref().map(|j| j.to_string()),
            Some("example.com".to_string())
        );
        assert_eq!(
            result.to.as_ref().map(|j| j.to_string()),
            Some("user@example.com/resource".to_string())
        );

        // Check the payload is a Result with a token element
        if let IqType::Result(Some(elem)) = &result.payload {
            assert_eq!(elem.name(), "token");
            assert_eq!(elem.ns(), ISR_NS);
            assert!(elem.attr("expiry").is_some());
            assert_eq!(elem.text(), isr_token.token);
        } else {
            panic!("Expected IqType::Result with Some(Element)");
        }
    }

    #[test]
    fn test_build_isr_token_error() {
        use minidom::Element;
        use xmpp_parsers::iq::{Iq, IqType};
        use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

        let token_request_elem = Element::builder("token-request", ISR_NS).build();
        let original_iq = Iq {
            from: Some("user@example.com/resource".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "token-1".to_string(),
            payload: IqType::Get(token_request_elem),
        };

        let error = build_isr_token_error(&original_iq, "not-authorized");

        assert_eq!(error.id, "token-1");
        assert_eq!(
            error.from.as_ref().map(|j| j.to_string()),
            Some("example.com".to_string())
        );

        // Check the payload is an Error with the correct condition
        if let IqType::Error(stanza_error) = &error.payload {
            assert_eq!(stanza_error.type_, ErrorType::Auth);
            assert_eq!(stanza_error.defined_condition, DefinedCondition::NotAuthorized);
        } else {
            panic!("Expected IqType::Error");
        }
    }

    #[test]
    fn test_build_isr_token_error_service_unavailable() {
        use minidom::Element;
        use xmpp_parsers::iq::{Iq, IqType};
        use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

        let token_request_elem = Element::builder("token-request", ISR_NS).build();
        let original_iq = Iq {
            from: Some("user@example.com/resource".parse().unwrap()),
            to: Some("example.com".parse().unwrap()),
            id: "token-2".to_string(),
            payload: IqType::Get(token_request_elem),
        };

        let error = build_isr_token_error(&original_iq, "service-unavailable");

        // Check the payload is an Error with service-unavailable condition
        if let IqType::Error(stanza_error) = &error.payload {
            assert_eq!(stanza_error.type_, ErrorType::Cancel);
            assert_eq!(stanza_error.defined_condition, DefinedCondition::ServiceUnavailable);
        } else {
            panic!("Expected IqType::Error");
        }
    }
}
