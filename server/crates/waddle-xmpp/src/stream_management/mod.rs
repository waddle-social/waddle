//! XEP-0198 Stream Management Implementation
//!
//! This module implements Stream Management as defined in XEP-0198,
//! providing reliability features for XMPP streams including:
//!
//! - Stanza acknowledgments (tracking which stanzas have been received)
//! - Stream resumption (reconnecting without losing messages)
//! - Unacknowledged stanza queuing (for resending after resume)
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
//!
//! ## Architecture
//!
//! - `StreamManagementState` - Per-connection SM state (counters, queue)
//! - `SmSessionRegistry` - Server-wide registry for detached resumable sessions
//! - `UnackedQueue` - Queue of unacknowledged outbound stanzas

mod session_registry;
mod unacked_queue;

pub use session_registry::{
    DetachedSession, InMemorySmSessionRegistry, SmRegistryError, SmSessionRegistry,
};
pub use unacked_queue::{UnackedPushResult, UnackedQueue, UnackedStanza};

use std::str::FromStr;
use std::time::Instant;

use minidom::Element;
use tracing::warn;

use crate::prometheus;

/// XEP-0198 Stream Management namespace (version 3)
pub const SM_NS: &str = "urn:xmpp:sm:3";

/// Namespace for XMPP stanza error conditions, used as the `<failed/>` child.
const STANZA_ERROR_NS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";

/// Parse an `xs:boolean` attribute value per W3C XML Schema Part 2 §3.2.2.
///
/// Both lexical forms are accepted for true (`"true"`, `"1"`) and false
/// (`"false"`, `"0"`). The XMPP ecosystem uses both: `prosody` and `ejabberd`
/// emit `"true"`, while `stanza.js` (our WebSocket client) emits the
/// canonical `"1"` / `"0"`. Missing or unrecognised values are treated as
/// `false`, matching the XMPP convention for optional boolean attributes.
fn parse_xs_boolean(value: &str) -> bool {
    matches!(value, "true" | "1")
}

/// Serialize a minidom `Element` to an XML string.
///
/// Writing to `Vec<u8>` is infallible (minidom only fails on I/O errors,
/// which `Vec<u8>` cannot produce) and minidom is documented to emit valid
/// UTF-8, so the two `expect`s are unreachable in practice.
fn element_to_xml(element: Element) -> String {
    let mut buf = Vec::new();
    element
        .write_to(&mut buf)
        .expect("minidom Element::write_to(Vec<u8>) cannot fail");
    String::from_utf8(buf).expect("minidom emits valid UTF-8")
}

/// Default maximum unacked queue size (stanzas)
pub const DEFAULT_MAX_UNACKED_QUEUE_SIZE: usize = 1000;

/// Default ack request threshold (request ack after this many unacked stanzas)
pub const DEFAULT_ACK_REQUEST_THRESHOLD: u32 = 5;

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

    /// Parse an `<enable/>` element from XML.
    pub fn parse(xml: &str) -> Option<Self> {
        let element = Element::from_str(xml).ok()?;
        if element.name() != "enable" || element.ns() != SM_NS {
            return None;
        }
        Some(Self::from_element(&element))
    }

    fn from_element(element: &Element) -> Self {
        let resume = element
            .attr("resume")
            .map(parse_xs_boolean)
            .unwrap_or(false);
        let max = element.attr("max").and_then(|s| s.parse().ok());
        Self { resume, max }
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

    /// Serialize to an `<enabled/>` XML string.
    pub fn to_xml(&self) -> String {
        let mut builder = Element::builder("enabled", SM_NS).attr("id", self.id.as_str());
        if self.resume {
            builder = builder.attr("resume", "true");
        }
        if let Some(max) = self.max {
            builder = builder.attr("max", max.to_string());
        }
        if let Some(location) = self.location.as_deref() {
            builder = builder.attr("location", location);
        }
        element_to_xml(builder.build())
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
    /// Parse a `<resume/>` element from XML.
    pub fn parse(xml: &str) -> Option<Self> {
        let element = Element::from_str(xml).ok()?;
        if element.name() != "resume" || element.ns() != SM_NS {
            return None;
        }
        Self::from_element(&element)
    }

    fn from_element(element: &Element) -> Option<Self> {
        let previd = element.attr("previd")?.to_string();
        let h = element.attr("h").and_then(|s| s.parse().ok())?;
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

    /// Serialize to a `<resumed/>` XML string.
    pub fn to_xml(&self) -> String {
        element_to_xml(
            Element::builder("resumed", SM_NS)
                .attr("previd", self.previd.as_str())
                .attr("h", self.h.to_string())
                .build(),
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

    /// Serialize to a `<failed/>` XML string.
    pub fn to_xml(&self) -> String {
        let mut builder = Element::builder("failed", SM_NS);
        if let Some(h) = self.h {
            builder = builder.attr("h", h.to_string());
        }
        if let Some(condition) = self.condition.as_deref() {
            builder = builder.append(Element::builder(condition, STANZA_ERROR_NS).build());
        }
        element_to_xml(builder.build())
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
        Element::from_str(xml)
            .map(|el| el.name() == "r" && el.ns() == SM_NS)
            .unwrap_or(false)
    }

    /// Serialize to an `<r/>` XML string.
    pub fn to_xml() -> String {
        element_to_xml(Element::builder("r", SM_NS).build())
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

    /// Parse an `<a/>` ack element from XML.
    pub fn parse(xml: &str) -> Option<Self> {
        let element = Element::from_str(xml).ok()?;
        if element.name() != "a" || element.ns() != SM_NS {
            return None;
        }
        Self::from_element(&element)
    }

    fn from_element(element: &Element) -> Option<Self> {
        let h = element.attr("h").and_then(|s| s.parse().ok())?;
        Some(Self { h })
    }

    /// Serialize to an `<a/>` XML string.
    pub fn to_xml(&self) -> String {
        element_to_xml(
            Element::builder("a", SM_NS)
                .attr("h", self.h.to_string())
                .build(),
        )
    }
}

/// Stream management state for a connection.
///
/// Tracks the counters, state, and unacknowledged stanza queue
/// needed for XEP-0198 operation.
#[derive(Debug)]
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
    /// Queue of unacknowledged outbound stanzas
    unacked_queue: UnackedQueue,
    /// Ack request threshold (request ack after this many unacked stanzas)
    ack_threshold: u32,
    /// When this SM state was created
    created_at: Instant,
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
            unacked_queue: UnackedQueue::new(DEFAULT_MAX_UNACKED_QUEUE_SIZE),
            ack_threshold: DEFAULT_ACK_REQUEST_THRESHOLD,
            created_at: Instant::now(),
        }
    }

    /// Create with custom configuration.
    pub fn with_config(max_queue_size: usize, ack_threshold: u32) -> Self {
        Self {
            enabled: false,
            stream_id: None,
            resumable: false,
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            max_resume_time: None,
            unacked_queue: UnackedQueue::new(max_queue_size),
            ack_threshold,
            created_at: Instant::now(),
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

    /// Record an outbound stanza and add it to the unacked queue.
    ///
    /// This should be called after sending each stanza when SM is enabled.
    /// The stanza is stored for potential resending after stream resumption.
    ///
    /// When the unacked queue is at capacity the oldest stanza is evicted
    /// to make room; that is surfaced via a `warn!` + the
    /// `waddle_sm_unacked_evicted_total` metric. A non-zero eviction rate
    /// means a subsequent `<resumed/>` on this stream will silently lose
    /// the evicted stanzas — the sender half of "reconnects drop
    /// messages".
    pub fn record_outbound(&mut self, stanza_xml: String) {
        self.outbound_count = self.outbound_count.wrapping_add(1);
        match self.unacked_queue.push(self.outbound_count, stanza_xml) {
            UnackedPushResult::Accepted => {}
            UnackedPushResult::Evicted(evicted) => {
                prometheus::increment_sm_unacked_evicted();
                warn!(
                    stream_id = self.stream_id.as_deref().unwrap_or("<unset>"),
                    evicted_sequence = evicted.sequence,
                    queue_len = self.unacked_queue.len(),
                    "SM unacked queue full; evicted oldest stanza — a later resume will replay without it"
                );
            }
        }
    }

    /// Update the last acknowledged count from a client ack.
    ///
    /// This also removes acknowledged stanzas from the queue.
    pub fn acknowledge(&mut self, h: u32) {
        self.last_acked = h;
        self.unacked_queue.acknowledge(h);
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

    /// Check if we should request an ack using the configured threshold.
    pub fn should_request_ack_auto(&self) -> bool {
        self.should_request_ack(self.ack_threshold)
    }

    /// Get stanzas that need to be resent after resumption.
    ///
    /// `client_h` is the last sequence number the client acknowledged receiving.
    /// Returns stanzas with sequence > client_h.
    pub fn get_stanzas_to_resend(&self, client_h: u32) -> Vec<String> {
        self.unacked_queue.get_unacked_after(client_h)
    }

    /// Get the queue length (for diagnostics).
    pub fn queue_len(&self) -> usize {
        self.unacked_queue.len()
    }

    /// Check if the stream is resumable (enabled + resumable flag + has stream_id).
    pub fn is_resumable(&self) -> bool {
        self.enabled && self.resumable && self.stream_id.is_some()
    }

    /// Get the age of this SM state.
    pub fn age(&self) -> std::time::Duration {
        self.created_at.elapsed()
    }

    /// Create a detached session for storage in the registry.
    ///
    /// `carbons_enabled` is the actor's current XEP-0280 opt-in value.
    /// Carbons opt-in is per-stream, so XEP-0198 resumption must preserve
    /// it — storing it on the detached session is what makes that possible.
    pub fn to_detached_session(
        &self,
        user_id: String,
        jid: jid::FullJid,
        carbons_enabled: bool,
    ) -> Option<DetachedSession> {
        if !self.is_resumable() {
            return None;
        }

        Some(DetachedSession {
            stream_id: self.stream_id.clone()?,
            user_id,
            jid,
            inbound_count: self.inbound_count,
            outbound_count: self.outbound_count,
            last_acked: self.last_acked,
            unacked_stanzas: self.unacked_queue.get_all_unacked(),
            max_resume_time: self.max_resume_time,
            detached_at: Instant::now(),
            carbons_enabled,
        })
    }

    /// Restore state from a detached session.
    ///
    /// This is used when resuming a stream.
    pub fn restore_from_session(&mut self, session: &DetachedSession) {
        self.enabled = true;
        self.stream_id = Some(session.stream_id.clone());
        self.resumable = true;
        self.inbound_count = session.inbound_count;
        self.outbound_count = session.outbound_count;
        self.last_acked = session.last_acked;
        self.max_resume_time = session.max_resume_time;

        // Restore unacked queue
        self.unacked_queue.restore(&session.unacked_stanzas);
    }
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
    /// Cheap lexical prefilter used by hot paths (e.g. WebSocket frame
    /// routing) so we only pay full XML parsing for likely SM control nonzas.
    pub fn is_client_nonza_candidate(xml: &str) -> bool {
        let trimmed = xml.trim_start();
        (trimmed.starts_with("<enable")
            || trimmed.starts_with("<resume")
            || trimmed.starts_with("<r")
            || trimmed.starts_with("<a"))
            && trimmed.contains(SM_NS)
    }

    /// Try to parse a stream management nonza from XML.
    ///
    /// Only parses client-origin nonzas: `<enable/>`, `<resume/>`, `<r/>`,
    /// `<a/>`. Server-origin nonzas (`<enabled/>`, `<resumed/>`, `<failed/>`)
    /// return `None` — the server authors those itself.
    pub fn parse(xml: &str) -> Option<Self> {
        let element = Element::from_str(xml).ok()?;
        if element.ns() != SM_NS {
            return None;
        }
        match element.name() {
            "enable" => Some(SmStanza::Enable(SmEnable::from_element(&element))),
            "resume" => SmResume::from_element(&element).map(SmStanza::Resume),
            "a" => SmAck::from_element(&element).map(SmStanza::Ack),
            "r" => Some(SmStanza::Request),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse the given XML through minidom and return the element. Used by
    /// tests below to assert against serialized output without depending on
    /// the quoting/attribute-order choices the serializer happens to make.
    fn parse_element(xml: &str) -> Element {
        Element::from_str(xml).expect("test fixture must be valid XML")
    }

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

    /// Regression guard for the `resume="1"` parsing bug. Stanza.js (the
    /// WebSocket client library used by `chat/`) serializes `xs:boolean`
    /// attributes in canonical form — `1`/`0`, not `true`/`false`. The old
    /// string-match parser only recognised `resume='true'` / `resume="true"`,
    /// so every real browser client ended up with a non-resumable SM
    /// session and the entire XEP-0198 resume path was effectively disabled.
    #[test]
    fn test_sm_enable_parses_xs_boolean_canonical_forms() {
        // Stanza.js wire format (double-quoted xs:boolean "1"):
        let enable = SmEnable::parse(r#"<enable xmlns="urn:xmpp:sm:3" resume="1"/>"#).unwrap();
        assert!(
            enable.resume,
            "resume=\"1\" is xs:boolean true — must parse as resume request"
        );

        // Single-quoted variant:
        let enable = SmEnable::parse("<enable xmlns='urn:xmpp:sm:3' resume='1'/>").unwrap();
        assert!(enable.resume);

        // Canonical xs:boolean false (`0`) must remain false.
        let enable = SmEnable::parse(r#"<enable xmlns="urn:xmpp:sm:3" resume="0"/>"#).unwrap();
        assert!(!enable.resume);

        // Unrecognised values fall back to false (XMPP convention for
        // optional boolean attributes).
        let enable = SmEnable::parse(r#"<enable xmlns="urn:xmpp:sm:3" resume="yes"/>"#).unwrap();
        assert!(!enable.resume);
    }

    #[test]
    fn test_sm_enabled_to_xml() {
        let enabled = SmEnabled::new("stream-123".to_string());
        let element = parse_element(&enabled.to_xml());
        assert_eq!(element.name(), "enabled");
        assert_eq!(element.ns(), SM_NS);
        assert_eq!(element.attr("id"), Some("stream-123"));
        assert_eq!(element.attr("resume"), None);

        let enabled = SmEnabled::with_resume("stream-456".to_string(), 300);
        let element = parse_element(&enabled.to_xml());
        assert_eq!(element.attr("resume"), Some("true"));
        assert_eq!(element.attr("max"), Some("300"));
    }

    #[test]
    fn test_sm_request() {
        assert!(SmRequest::is_request("<r xmlns='urn:xmpp:sm:3'/>"));
        // Bare `<r/>` (no xmlns) is NOT an SM request — the old parser
        // accepted it, which mis-classified any `<r>` in another namespace
        // as stream management.
        assert!(!SmRequest::is_request("<r/>"));
        assert!(!SmRequest::is_request("<message/>"));
    }

    #[test]
    fn test_sm_ack_parse_and_serialize() {
        let xml = "<a xmlns='urn:xmpp:sm:3' h='5'/>";
        let ack = SmAck::parse(xml).unwrap();
        assert_eq!(ack.h, 5);

        let element = parse_element(&ack.to_xml());
        assert_eq!(element.name(), "a");
        assert_eq!(element.ns(), SM_NS);
        assert_eq!(element.attr("h"), Some("5"));
    }

    /// The old string-match parser used `xml.find("h=")`, which happily
    /// matched the `h=` substring inside attributes like `bah="99"`. Proper
    /// XML parsing rejects that ambiguity. Guard against regressing back to
    /// substring search.
    #[test]
    fn test_sm_ack_is_not_fooled_by_attribute_name_prefix_collision() {
        let xml = r#"<a xmlns="urn:xmpp:sm:3" bah="99" h="7"/>"#;
        let ack = SmAck::parse(xml).expect("should parse");
        assert_eq!(
            ack.h, 7,
            "must read the real `h` attribute, not a substring match of `bah`"
        );
    }

    #[test]
    fn test_sm_failed() {
        let failed = SmFailed::with_condition("item-not-found");
        let element = parse_element(&failed.to_xml());
        assert_eq!(element.name(), "failed");
        assert_eq!(element.ns(), SM_NS);
        let condition = element
            .children()
            .find(|child| child.name() == "item-not-found")
            .expect("condition child");
        assert_eq!(condition.ns(), STANZA_ERROR_NS);

        let failed = SmFailed::resume_failed("item-not-found", 10);
        let element = parse_element(&failed.to_xml());
        assert_eq!(element.attr("h"), Some("10"));
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
    fn test_sm_state_record_outbound() {
        let mut state = StreamManagementState::new();
        state.enable("test-id".to_string(), true, Some(300));

        state.record_outbound("<message id='1'/>".to_string());
        state.record_outbound("<message id='2'/>".to_string());
        state.record_outbound("<message id='3'/>".to_string());

        assert_eq!(state.outbound_count, 3);
        assert_eq!(state.queue_len(), 3);

        // Acknowledge first two
        state.acknowledge(2);
        assert_eq!(state.queue_len(), 1);

        // Get stanzas to resend after client says h=1 (needs 2 and 3)
        let resend = state.get_stanzas_to_resend(1);
        assert_eq!(resend.len(), 1); // Only 3 is left in queue after ack(2)
    }

    #[test]
    fn test_record_outbound_surfaces_eviction_to_metric() {
        // With a tiny queue cap, the 4th push must evict the 1st — and
        // that eviction must bump `waddle_sm_unacked_evicted_total` so
        // operators can distinguish "everything's fine" from "your
        // <resumed/> replays have holes".
        use crate::prometheus;

        // Baseline the counter since other tests run in the same process.
        let before_render = prometheus::render_metrics();
        let baseline = before_render
            .lines()
            .find(|line| line.starts_with("waddle_sm_unacked_evicted_total "))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);

        let mut state = StreamManagementState::with_config(3, 5);
        state.enable("tiny-cap".to_string(), true, Some(300));

        state.record_outbound("<message id='1'/>".to_string());
        state.record_outbound("<message id='2'/>".to_string());
        state.record_outbound("<message id='3'/>".to_string());
        assert_eq!(state.queue_len(), 3);

        // 4th push evicts seq=1
        state.record_outbound("<message id='4'/>".to_string());
        assert_eq!(state.queue_len(), 3);

        let after_render = prometheus::render_metrics();
        let after = after_render
            .lines()
            .find(|line| line.starts_with("waddle_sm_unacked_evicted_total "))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
            .expect("metric line must render");
        assert_eq!(
            after - baseline,
            1,
            "one eviction must have bumped the counter by one"
        );

        // The replay after resume must be missing the evicted stanza.
        let resend = state.get_stanzas_to_resend(0);
        assert_eq!(resend.len(), 3, "evicted seq=1 must be absent from replay");
        assert!(!resend.iter().any(|xml| xml.contains("id='1'")));
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
    fn test_sm_stanza_candidate_prefilter() {
        assert!(SmStanza::is_client_nonza_candidate(
            "<enable xmlns='urn:xmpp:sm:3'/>"
        ));
        assert!(SmStanza::is_client_nonza_candidate(
            "<resume xmlns='urn:xmpp:sm:3' previd='id' h='1'/>"
        ));
        assert!(SmStanza::is_client_nonza_candidate(
            "<r xmlns='urn:xmpp:sm:3'/>"
        ));
        assert!(SmStanza::is_client_nonza_candidate(
            "<a xmlns='urn:xmpp:sm:3' h='4'/>"
        ));

        assert!(!SmStanza::is_client_nonza_candidate(
            "<message xmlns='jabber:client'/>"
        ));
        assert!(!SmStanza::is_client_nonza_candidate("<r/>"));
    }

    #[test]
    fn test_sm_resume_parse() {
        let xml = "<resume xmlns='urn:xmpp:sm:3' previd='stream-123' h='5'/>";
        let resume = SmResume::parse(xml).unwrap();
        assert_eq!(resume.previd, "stream-123");
        assert_eq!(resume.h, 5);
    }

    #[test]
    fn test_sm_state_resumable() {
        let mut state = StreamManagementState::new();
        assert!(!state.is_resumable());

        state.enable("test-id".to_string(), false, None);
        assert!(!state.is_resumable()); // Not resumable flag

        state.enable("test-id".to_string(), true, Some(300));
        assert!(state.is_resumable());
    }

    /// XEP-0198 §5 stream resumption is meant to continue the exact same
    /// stream — the client doesn't expect to re-negotiate per-stream add-ons
    /// like XEP-0280 carbons after a successful `<resumed/>`. So the
    /// detached session the server stashes at disconnect MUST carry the
    /// carbons opt-in flag, and a resumed actor must be able to read it
    /// back. If this regresses, every SM-resume will silently disable
    /// carbons until the client re-enables them.
    #[test]
    fn test_to_detached_session_carries_carbons_flag() {
        let mut state = StreamManagementState::new();
        state.enable("stream-carb".to_string(), true, Some(300));
        let jid: jid::FullJid = "user@example.com/resource".parse().unwrap();

        let detached_off = state
            .to_detached_session("user@example.com".to_string(), jid.clone(), false)
            .expect("resumable state must produce detached session");
        assert!(
            !detached_off.carbons_enabled,
            "carbons_enabled=false must round-trip through DetachedSession"
        );

        let detached_on = state
            .to_detached_session("user@example.com".to_string(), jid, true)
            .expect("resumable state must produce detached session");
        assert!(
            detached_on.carbons_enabled,
            "carbons_enabled=true must round-trip so resume preserves opt-in"
        );
    }
}
