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

use std::time::Instant;

use chrono::{DateTime, Utc};

mod store;

pub use store::{
    create_shared_store, create_shared_store_with_config, IsrTokenStore, SharedIsrTokenStore,
};

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
    /// Associated session user identifier
    pub user_id: String,
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
    pub fn new(user_id: String, jid: jid::BareJid, validity_secs: u64) -> Self {
        let validity = validity_secs.min(MAX_TOKEN_VALIDITY_SECS);
        let token = generate_token();
        let expiry = Utc::now() + chrono::Duration::seconds(validity as i64);

        Self {
            token,
            expiry,
            user_id,
            jid,
            sm_stream_id: None,
            sm_inbound_count: 0,
            sm_outbound_count: 0,
            created_at: Instant::now(),
        }
    }

    /// Create a token with SM state.
    pub fn with_sm_state(
        user_id: String,
        jid: jid::BareJid,
        validity_secs: u64,
        sm_stream_id: String,
        inbound_count: u32,
        outbound_count: u32,
    ) -> Self {
        let mut token = Self::new(user_id, jid, validity_secs);
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
    base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        combined.as_bytes(),
    )
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
pub fn build_isr_token_result(
    original_iq: &xmpp_parsers::iq::Iq,
    token: &IsrToken,
) -> xmpp_parsers::iq::Iq {
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
pub fn build_isr_token_error(
    original_iq: &xmpp_parsers::iq::Iq,
    condition: &str,
) -> xmpp_parsers::iq::Iq {
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
        "", // Empty text
    );

    xmpp_parsers::iq::Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: xmpp_parsers::iq::IqType::Error(stanza_error),
    }
}

#[cfg(test)]
mod tests;
