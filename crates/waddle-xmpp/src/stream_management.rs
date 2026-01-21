//! XEP-0198 Stream Management Implementation
//!
//! This module implements Stream Management as defined in XEP-0198,
//! providing reliability features for XMPP streams including:
//!
//! - Stanza acknowledgments (tracking which stanzas have been received)
//! - Stream resumption (reconnecting without losing messages)
//!
//! ## Protocol Overview
//!
//! Stream Management adds the following elements in the `urn:xmpp:sm:3` namespace:
//! - `<enable/>` - Client request to enable stream management
//! - `<enabled/>` - Server confirmation that SM is enabled
//! - `<r/>` - Request acknowledgment of received stanzas
//! - `<a h='N'/>` - Acknowledge receipt of N stanzas
//! - `<resume/>` - Request to resume a previous stream
//! - `<resumed/>` - Confirmation that stream was resumed
//! - `<failed/>` - Stream management operation failed

/// XEP-0198 Stream Management namespace (version 3)
pub const SM_NS: &str = "urn:xmpp:sm:3";

/// Enable request from client to activate stream management.
///
/// The client sends this after resource binding to enable SM features.
/// Optional attributes:
/// - `resume`: Request ability to resume the stream after disconnection
/// - `max`: Maximum resumption time in seconds the client can support
#[derive(Debug, Clone, Default)]
pub struct SmEnable {
    /// Whether the client wants to be able to resume the stream
    pub resume: bool,
    /// Maximum resumption time in seconds (optional)
    pub max: Option<u32>,
}

impl SmEnable {
    /// Create a new enable request.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an enable request with resumption support.
    pub fn with_resume(max_seconds: Option<u32>) -> Self {
        Self {
            resume: true,
            max: max_seconds,
        }
    }

    /// Parse an enable element from XML.
    pub fn parse(xml: &str) -> Option<Self> {
        if !xml.contains("<enable") || !xml.contains(SM_NS) {
            return None;
        }

        let resume = xml.contains("resume='true'") || xml.contains("resume=\"true\"");
        let max = extract_attr(xml, "max").and_then(|s| s.parse().ok());

        Some(Self { resume, max })
    }
}

/// Enabled response from server confirming stream management is active.
///
/// Sent by the server in response to `<enable/>`.
#[derive(Debug, Clone)]
pub struct SmEnabled {
    /// Unique identifier for this stream (for resumption)
    pub id: String,
    /// Whether stream resumption is available
    pub resume: bool,
    /// Maximum time in seconds the server will allow resumption
    pub max: Option<u32>,
    /// Server location hint for resumption (optional)
    pub location: Option<String>,
}

impl SmEnabled {
    /// Create a new enabled response.
    pub fn new(id: String) -> Self {
        Self {
            id,
            resume: false,
            max: None,
            location: None,
        }
    }

    /// Create an enabled response with resumption support.
    pub fn with_resume(id: String, max_seconds: u32) -> Self {
        Self {
            id,
            resume: true,
            max: Some(max_seconds),
            location: None,
        }
    }

    /// Serialize to XML string.
    pub fn to_xml(&self) -> String {
        let mut attrs = format!("id='{}'", self.id);
        if self.resume {
            attrs.push_str(" resume='true'");
        }
        if let Some(max) = self.max {
            attrs.push_str(&format!(" max='{}'", max));
        }
        if let Some(ref loc) = self.location {
            attrs.push_str(&format!(" location='{}'", loc));
        }
        format!("<enabled xmlns='{}' {}/>", SM_NS, attrs)
    }
}

/// Resume request from client to restore a previous stream.
///
/// The client sends this instead of resource binding when reconnecting.
#[derive(Debug, Clone)]
pub struct SmResume {
    /// The stream ID from the original `<enabled/>` response
    pub previd: String,
    /// The last handled stanza count from the client's perspective
    pub h: u32,
}

impl SmResume {
    /// Parse a resume element from XML.
    pub fn parse(xml: &str) -> Option<Self> {
        if !xml.contains("<resume") || !xml.contains(SM_NS) {
            return None;
        }

        let previd = extract_attr(xml, "previd")?;
        let h = extract_attr(xml, "h").and_then(|s| s.parse().ok())?;

        Some(Self { previd, h })
    }
}

/// Resumed response from server confirming stream was restored.
#[derive(Debug, Clone)]
pub struct SmResumed {
    /// The stream ID that was resumed
    pub previd: String,
    /// The server's last handled stanza count
    pub h: u32,
}

impl SmResumed {
    /// Create a new resumed response.
    pub fn new(previd: String, h: u32) -> Self {
        Self { previd, h }
    }

    /// Serialize to XML string.
    pub fn to_xml(&self) -> String {
        format!(
            "<resumed xmlns='{}' previd='{}' h='{}'/>",
            SM_NS, self.previd, self.h
        )
    }
}

/// Failed response indicating a stream management operation failed.
#[derive(Debug, Clone)]
pub struct SmFailed {
    /// Error condition (e.g., "item-not-found" for unknown stream ID)
    pub condition: Option<String>,
    /// The handled count at time of failure (for resume failures)
    pub h: Option<u32>,
}

impl SmFailed {
    /// Create a simple failed response.
    pub fn new() -> Self {
        Self {
            condition: None,
            h: None,
        }
    }

    /// Create a failed response with an error condition.
    pub fn with_condition(condition: &str) -> Self {
        Self {
            condition: Some(condition.to_string()),
            h: None,
        }
    }

    /// Create a failed response for resume failure with handled count.
    pub fn resume_failed(condition: &str, h: u32) -> Self {
        Self {
            condition: Some(condition.to_string()),
            h: Some(h),
        }
    }

    /// Serialize to XML string.
    pub fn to_xml(&self) -> String {
        let h_attr = self.h.map(|h| format!(" h='{}'", h)).unwrap_or_default();

        if let Some(ref cond) = self.condition {
            format!(
                "<failed xmlns='{}'{}><{} xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/></failed>",
                SM_NS, h_attr, cond
            )
        } else {
            format!("<failed xmlns='{}'{}/>", SM_NS, h_attr)
        }
    }
}

impl Default for SmFailed {
    fn default() -> Self {
        Self::new()
    }
}

/// Acknowledgment request from either party.
///
/// When received, the other party should respond with `<a/>`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SmRequest;

impl SmRequest {
    /// Check if XML is an ack request.
    pub fn is_request(xml: &str) -> bool {
        (xml.contains("<r") && xml.contains(SM_NS)) || xml.trim() == "<r/>" || xml.contains("<r xmlns=")
    }

    /// Serialize to XML string.
    pub fn to_xml() -> String {
        format!("<r xmlns='{}'/>", SM_NS)
    }
}

/// Acknowledgment response containing the count of handled stanzas.
///
/// The `h` attribute indicates the sequence number of the last handled stanza.
#[derive(Debug, Clone, Copy)]
pub struct SmAck {
    /// The sequence number of the last handled inbound stanza
    pub h: u32,
}

impl SmAck {
    /// Create a new acknowledgment with the given count.
    pub fn new(h: u32) -> Self {
        Self { h }
    }

    /// Parse an ack element from XML.
    pub fn parse(xml: &str) -> Option<Self> {
        if !xml.contains("<a") || !xml.contains(SM_NS) {
            return None;
        }

        let h = extract_attr(xml, "h").and_then(|s| s.parse().ok())?;
        Some(Self { h })
    }

    /// Serialize to XML string.
    pub fn to_xml(&self) -> String {
        format!("<a xmlns='{}' h='{}'/>", SM_NS, self.h)
    }
}

/// Stream management state for a connection.
///
/// Tracks the counters and state needed for XEP-0198 operation.
#[derive(Debug, Clone)]
pub struct StreamManagementState {
    /// Whether stream management is enabled
    pub enabled: bool,
    /// Unique stream ID (for resumption)
    pub stream_id: Option<String>,
    /// Whether resumption is enabled
    pub resumable: bool,
    /// Count of stanzas received from the client (inbound)
    pub inbound_count: u32,
    /// Count of stanzas sent to the client (outbound)
    pub outbound_count: u32,
    /// Last acknowledged outbound stanza count (from client's <a/>)
    pub last_acked: u32,
    /// Maximum resumption timeout in seconds
    pub max_resume_time: Option<u32>,
}

impl Default for StreamManagementState {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamManagementState {
    /// Create a new disabled stream management state.
    pub fn new() -> Self {
        Self {
            enabled: false,
            stream_id: None,
            resumable: false,
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            max_resume_time: None,
        }
    }

    /// Enable stream management.
    pub fn enable(&mut self, stream_id: String, resumable: bool, max_time: Option<u32>) {
        self.enabled = true;
        self.stream_id = Some(stream_id);
        self.resumable = resumable;
        self.max_resume_time = max_time;
    }

    /// Increment the inbound stanza count (stanzas received from client).
    pub fn increment_inbound(&mut self) {
        self.inbound_count = self.inbound_count.wrapping_add(1);
    }

    /// Increment the outbound stanza count (stanzas sent to client).
    pub fn increment_outbound(&mut self) {
        self.outbound_count = self.outbound_count.wrapping_add(1);
    }

    /// Update the last acknowledged count from a client ack.
    pub fn acknowledge(&mut self, h: u32) {
        self.last_acked = h;
    }

    /// Get the current inbound count for sending in an <a/> response.
    pub fn get_inbound_count(&self) -> u32 {
        self.inbound_count
    }

    /// Get the number of unacknowledged outbound stanzas.
    pub fn unacked_count(&self) -> u32 {
        self.outbound_count.wrapping_sub(self.last_acked)
    }

    /// Check if we should request an ack from the client.
    ///
    /// Returns true if there are many unacked stanzas.
    pub fn should_request_ack(&self, threshold: u32) -> bool {
        self.enabled && self.unacked_count() >= threshold
    }
}

/// Extract an attribute value from XML.
fn extract_attr(xml: &str, name: &str) -> Option<String> {
    // Try both single and double quotes
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

/// Parsed stream management stanza variants.
#[derive(Debug, Clone)]
pub enum SmStanza {
    /// Enable stream management request
    Enable(SmEnable),
    /// Stream management enabled response
    Enabled(SmEnabled),
    /// Resume stream request
    Resume(SmResume),
    /// Stream resumed response
    Resumed(SmResumed),
    /// Stream management failed
    Failed(SmFailed),
    /// Request acknowledgment
    Request,
    /// Acknowledgment with handled count
    Ack(SmAck),
}

impl SmStanza {
    /// Try to parse a stream management stanza from XML.
    pub fn parse(xml: &str) -> Option<Self> {
        if !xml.contains(SM_NS) && !xml.trim().starts_with("<r/>") && !xml.trim().starts_with("<a ") {
            return None;
        }

        if xml.contains("<enable") {
            SmEnable::parse(xml).map(SmStanza::Enable)
        } else if xml.contains("<enabled") {
            // Server response, not typically parsed by server
            None
        } else if xml.contains("<resume") {
            SmResume::parse(xml).map(SmStanza::Resume)
        } else if xml.contains("<resumed") {
            // Server response, not typically parsed by server
            None
        } else if xml.contains("<failed") {
            // Server response, not typically parsed by server
            None
        } else if SmRequest::is_request(xml) {
            Some(SmStanza::Request)
        } else if xml.contains("<a ") || xml.contains("<a>") {
            SmAck::parse(xml).map(SmStanza::Ack)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sm_enable_parse() {
        let xml = "<enable xmlns='urn:xmpp:sm:3'/>";
        let enable = SmEnable::parse(xml).unwrap();
        assert!(!enable.resume);
        assert!(enable.max.is_none());

        let xml = "<enable xmlns='urn:xmpp:sm:3' resume='true' max='300'/>";
        let enable = SmEnable::parse(xml).unwrap();
        assert!(enable.resume);
        assert_eq!(enable.max, Some(300));
    }

    #[test]
    fn test_sm_enabled_to_xml() {
        let enabled = SmEnabled::new("stream-123".to_string());
        let xml = enabled.to_xml();
        assert!(xml.contains("xmlns='urn:xmpp:sm:3'"));
        assert!(xml.contains("id='stream-123'"));

        let enabled = SmEnabled::with_resume("stream-456".to_string(), 300);
        let xml = enabled.to_xml();
        assert!(xml.contains("resume='true'"));
        assert!(xml.contains("max='300'"));
    }

    #[test]
    fn test_sm_request() {
        assert!(SmRequest::is_request("<r xmlns='urn:xmpp:sm:3'/>"));
        assert!(SmRequest::is_request("<r/>"));
        assert!(!SmRequest::is_request("<message/>"));
    }

    #[test]
    fn test_sm_ack_parse_and_serialize() {
        let xml = "<a xmlns='urn:xmpp:sm:3' h='5'/>";
        let ack = SmAck::parse(xml).unwrap();
        assert_eq!(ack.h, 5);

        let serialized = ack.to_xml();
        assert!(serialized.contains("h='5'"));
    }

    #[test]
    fn test_sm_failed() {
        let failed = SmFailed::with_condition("item-not-found");
        let xml = failed.to_xml();
        assert!(xml.contains("<item-not-found"));

        let failed = SmFailed::resume_failed("item-not-found", 10);
        let xml = failed.to_xml();
        assert!(xml.contains("h='10'"));
    }

    #[test]
    fn test_sm_state_counting() {
        let mut state = StreamManagementState::new();
        state.enable("test-id".to_string(), false, None);

        assert_eq!(state.inbound_count, 0);
        state.increment_inbound();
        state.increment_inbound();
        assert_eq!(state.inbound_count, 2);

        state.increment_outbound();
        state.increment_outbound();
        state.increment_outbound();
        assert_eq!(state.outbound_count, 3);
        assert_eq!(state.unacked_count(), 3);

        state.acknowledge(2);
        assert_eq!(state.unacked_count(), 1);
    }

    #[test]
    fn test_sm_stanza_parse() {
        // Enable
        let enable = SmStanza::parse("<enable xmlns='urn:xmpp:sm:3' resume='true'/>");
        assert!(matches!(enable, Some(SmStanza::Enable(_))));

        // Request
        let request = SmStanza::parse("<r xmlns='urn:xmpp:sm:3'/>");
        assert!(matches!(request, Some(SmStanza::Request)));

        // Ack
        let ack = SmStanza::parse("<a xmlns='urn:xmpp:sm:3' h='10'/>");
        assert!(matches!(ack, Some(SmStanza::Ack(_))));

        // Non-SM stanza
        let other = SmStanza::parse("<message/>");
        assert!(other.is_none());
    }

    #[test]
    fn test_sm_resume_parse() {
        let xml = "<resume xmlns='urn:xmpp:sm:3' previd='stream-123' h='5'/>";
        let resume = SmResume::parse(xml).unwrap();
        assert_eq!(resume.previd, "stream-123");
        assert_eq!(resume.h, 5);
    }
}
