use std::collections::VecDeque;

use minidom::Element;

use crate::error::{ClientError, ClientResult};
use crate::request::StanzaId;

pub const NS_SM: &str = "urn:xmpp:sm:3";

#[derive(Debug, Clone, PartialEq, Eq)]
struct QueuedOutboundStanza {
    h: u32,
    element: Element,
    message_stanza_id: Option<StanzaId>,
}

/// In-memory XEP-0198 resume snapshot carried across a reconnect attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmResumeState {
    previd: String,
    inbound_h: u32,
    outbound_h: u32,
    outbound_queue: VecDeque<QueuedOutboundStanza>,
}

impl SmResumeState {
    pub fn new(previd: impl Into<String>, inbound_h: u32, outbound_h: u32) -> ClientResult<Self> {
        let previd = previd.into();
        if previd.trim().is_empty() {
            return Err(ClientError::EmptyStanzaId);
        }

        Ok(Self {
            previd,
            inbound_h,
            outbound_h,
            outbound_queue: VecDeque::new(),
        })
    }

    fn from_outbound_queue(
        previd: impl Into<String>,
        inbound_h: u32,
        outbound_h: u32,
        outbound_queue: VecDeque<QueuedOutboundStanza>,
    ) -> ClientResult<Self> {
        let mut state = Self::new(previd, inbound_h, outbound_h)?;
        state.outbound_queue = outbound_queue;
        Ok(state)
    }

    pub fn previd(&self) -> &str {
        &self.previd
    }

    pub fn inbound_h(&self) -> u32 {
        self.inbound_h
    }

    pub fn outbound_h(&self) -> u32 {
        self.outbound_h
    }
}

/// XEP-0198 client-side stream management state.
///
/// **SM semantics:** Acking tracks transport-level responsibility. When the
/// peer reports `h`, every outbound stanza at or below that sequence number has
/// been handled by the peer. A resumable session must retain enough typed
/// outbound state to replay stanzas above the reported `h` without inventing new
/// message identity.
#[derive(Debug, Clone, Default)]
pub struct SmState {
    /// Stanzas sent since the session started (wrapping u32).
    pub outbound_count: u32,
    /// Stanzas received since the session started (wrapping u32).
    pub inbound_count: u32,
    /// Last `h` value acknowledged by the server.
    pub server_h: u32,
    /// Resumption token (`previd`) set after `<enabled/>` or `<resumed/>`.
    pub previd: Option<String>,
    /// Whether the outbound stanza counter has started for this session.
    ///
    /// XEP-0198 starts the sender's own counter immediately after sending
    /// `<enable/>`; the peer's `<enabled/>` response can arrive after one or
    /// more stanzas have already been sent.
    pub outbound_enabled: bool,
    /// Whether SM is currently enabled for this session.
    pub enabled: bool,
    outbound_queue: VecDeque<QueuedOutboundStanza>,
    replay_in_flight: VecDeque<Element>,
}

impl SmState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_resume_state(resume_state: &SmResumeState) -> Self {
        Self {
            outbound_count: resume_state.outbound_h(),
            inbound_count: resume_state.inbound_h(),
            previd: Some(resume_state.previd().to_string()),
            outbound_queue: resume_state.outbound_queue.clone(),
            ..Self::default()
        }
    }

    pub fn resume_state(&self) -> Option<SmResumeState> {
        self.previd.as_ref().and_then(|previd| {
            SmResumeState::from_outbound_queue(
                previd.clone(),
                self.inbound_count,
                self.outbound_count,
                self.outbound_queue.clone(),
            )
            .ok()
        })
    }

    /// Increment the outbound stanza counter by `count`.
    pub fn record_sent(&mut self, count: u32) {
        self.outbound_count = self.outbound_count.wrapping_add(count);
    }

    /// Record a newly sent outbound stanza unless it is a queued replay.
    pub fn record_sent_stanza(&mut self, element: &Element) {
        if self.suppress_replay_sent_record(element) {
            return;
        }

        self.record_sent(1);
        self.outbound_queue.push_back(QueuedOutboundStanza {
            h: self.outbound_count,
            message_stanza_id: message_delivery_stanza_id(element),
            element: element.clone(),
        });
    }

    /// Start a fresh outbound SM sequence after sending `<enable/>`.
    pub fn start_outbound(&mut self) {
        self.outbound_count = 0;
        self.server_h = 0;
        self.outbound_enabled = true;
        self.outbound_queue.clear();
        self.replay_in_flight.clear();
    }

    /// Stop all SM counters after `<failed/>` or stream termination.
    pub fn stop(&mut self) {
        self.outbound_enabled = false;
        self.enabled = false;
    }

    /// Increment the inbound stanza counter by `count`.
    pub fn record_received(&mut self, count: u32) {
        self.inbound_count = self.inbound_count.wrapping_add(count);
    }

    /// Update `server_h` from an `<a h='...'/>` ack.
    pub fn process_ack(&mut self, h: u32) -> Vec<StanzaId> {
        self.server_h = h;
        let mut acked = Vec::new();
        while self
            .outbound_queue
            .front()
            .is_some_and(|queued| queued.h <= h)
        {
            if let Some(queued) = self.outbound_queue.pop_front() {
                if let Some(stanza_id) = queued.message_stanza_id {
                    acked.push(stanza_id);
                }
            }
        }
        acked
    }

    /// Mark currently unhandled outbound stanzas for replay and return them.
    pub fn mark_unhandled_for_replay(&mut self) -> Vec<Element> {
        let replay: Vec<Element> = self
            .outbound_queue
            .iter()
            .map(|queued| queued.element.clone())
            .collect();
        self.replay_in_flight.extend(replay.iter().cloned());
        replay
    }

    fn suppress_replay_sent_record(&mut self, element: &Element) -> bool {
        if self
            .replay_in_flight
            .front()
            .is_some_and(|queued| queued == element)
        {
            self.replay_in_flight.pop_front();
            return true;
        }
        false
    }

    /// Build `<enable xmlns='urn:xmpp:sm:3' resume='true'/>`.
    pub fn build_enable(resume: bool) -> Element {
        Self::build_enable_with_max(resume, None)
    }

    /// Build `<enable/>` with an optional XEP-0198 resumption window request.
    pub fn build_enable_with_max(resume: bool, max: Option<u32>) -> Element {
        let mut b = Element::builder("enable", NS_SM);
        if resume {
            b = b.attr("resume", "true");
        }
        if let Some(max) = max {
            b = b.attr("max", max.to_string());
        }
        b.build()
    }

    /// Build `<resume xmlns='urn:xmpp:sm:3' previd='ID' h='N'/>`.
    pub fn build_resume(previd: &str, h: u32) -> Element {
        Element::builder("resume", NS_SM)
            .attr("previd", previd)
            .attr("h", h.to_string())
            .build()
    }

    /// Build `<r xmlns='urn:xmpp:sm:3'/>` to request an ack.
    pub fn build_request_ack() -> Element {
        Element::builder("r", NS_SM).build()
    }

    /// Build `<a xmlns='urn:xmpp:sm:3' h='N'/>` to acknowledge `h` stanzas.
    pub fn build_ack(h: u32) -> Element {
        Element::builder("a", NS_SM)
            .attr("h", h.to_string())
            .build()
    }

    /// Extract the resumption `id` from an `<enabled/>` element, if present.
    pub fn parse_enabled(element: &Element) -> Option<String> {
        if element.name() == "enabled" && element.ns() == NS_SM {
            if !matches!(element.attr("resume"), Some("true") | Some("1")) {
                return None;
            }
            element.attr("id").map(|s| s.to_string())
        } else {
            None
        }
    }

    /// Extract the `h` value from an `<a h='...'/>` ack element.
    pub fn parse_ack_h(element: &Element) -> Option<u32> {
        if element.name() == "a" && element.ns() == NS_SM {
            element.attr("h")?.parse().ok()
        } else {
            None
        }
    }

    /// Return `true` if `element` is an `<r/>` ack request from the server.
    pub fn is_request_ack(element: &Element) -> bool {
        element.name() == "r" && element.ns() == NS_SM
    }
}

fn message_delivery_stanza_id(element: &Element) -> Option<StanzaId> {
    if element.name() != "message" {
        return None;
    }

    element.attr("id").and_then(|id| StanzaId::new(id).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serialize(element: &Element) -> String {
        let mut buf = Vec::new();
        element.write_to(&mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn build_enable_with_resume_sets_attribute() {
        let el = SmState::build_enable(true);
        assert_eq!(el.name(), "enable");
        assert_eq!(el.ns(), NS_SM);
        assert_eq!(el.attr("resume"), Some("true"));
        let xml = serialize(&el);
        assert!(xml.contains("resume=\"true\""), "xml: {xml}");
    }

    #[test]
    fn build_enable_without_resume_omits_attribute() {
        let el = SmState::build_enable(false);
        assert_eq!(el.name(), "enable");
        assert_eq!(el.ns(), NS_SM);
        assert!(el.attr("resume").is_none());
    }

    #[test]
    fn build_ack_serialises_h_attribute() {
        let el = SmState::build_ack(42);
        assert_eq!(el.name(), "a");
        assert_eq!(el.ns(), NS_SM);
        assert_eq!(el.attr("h"), Some("42"));
        let xml = serialize(&el);
        assert!(xml.contains("h=\"42\""), "xml: {xml}");
    }

    #[test]
    fn build_request_ack_produces_r_element() {
        let el = SmState::build_request_ack();
        assert_eq!(el.name(), "r");
        assert_eq!(el.ns(), NS_SM);
    }

    #[test]
    fn parse_enabled_extracts_previd() {
        let el = Element::builder("enabled", NS_SM)
            .attr("id", "abc123")
            .attr("resume", "true")
            .build();
        assert_eq!(SmState::parse_enabled(&el), Some("abc123".to_string()));
    }

    #[test]
    fn parse_enabled_requires_resumable_response() {
        let el = Element::builder("enabled", NS_SM)
            .attr("id", "abc123")
            .build();
        assert_eq!(SmState::parse_enabled(&el), None);

        let el = Element::builder("enabled", NS_SM)
            .attr("id", "abc123")
            .attr("resume", "false")
            .build();
        assert_eq!(SmState::parse_enabled(&el), None);
    }

    #[test]
    fn parse_enabled_returns_none_when_no_id() {
        let el = Element::builder("enabled", NS_SM).build();
        assert_eq!(SmState::parse_enabled(&el), None);
    }

    #[test]
    fn parse_enabled_returns_none_for_wrong_element() {
        let el = Element::builder("enable", NS_SM).attr("id", "abc").build();
        assert_eq!(SmState::parse_enabled(&el), None);

        let el2 = Element::builder("enabled", "urn:ietf:params:xml:ns:xmpp-bind")
            .attr("id", "abc")
            .build();
        assert_eq!(SmState::parse_enabled(&el2), None);
    }

    #[test]
    fn parse_ack_h_extracts_value() {
        let el = Element::builder("a", NS_SM).attr("h", "7").build();
        assert_eq!(SmState::parse_ack_h(&el), Some(7));
    }

    #[test]
    fn parse_ack_h_returns_none_for_wrong_element() {
        let el = Element::builder("b", NS_SM).attr("h", "7").build();
        assert_eq!(SmState::parse_ack_h(&el), None);

        let el2 = Element::builder("a", "jabber:client")
            .attr("h", "7")
            .build();
        assert_eq!(SmState::parse_ack_h(&el2), None);
    }

    #[test]
    fn parse_ack_h_returns_none_for_bad_parse() {
        let el = Element::builder("a", NS_SM).attr("h", "notanumber").build();
        assert_eq!(SmState::parse_ack_h(&el), None);
    }

    #[test]
    fn is_request_ack_matches_r_element() {
        let el = Element::builder("r", NS_SM).build();
        assert!(SmState::is_request_ack(&el));
    }

    #[test]
    fn is_request_ack_rejects_other_elements() {
        let el = Element::builder("a", NS_SM).build();
        assert!(!SmState::is_request_ack(&el));

        let el2 = Element::builder("r", "jabber:client").build();
        assert!(!SmState::is_request_ack(&el2));
    }

    #[test]
    fn record_sent_wraps_at_u32_max() {
        let mut state = SmState::new();
        state.outbound_count = u32::MAX;
        state.record_sent(1);
        assert_eq!(state.outbound_count, 0);
    }

    #[test]
    fn record_received_wraps_at_u32_max() {
        let mut state = SmState::new();
        state.inbound_count = u32::MAX;
        state.record_received(1);
        assert_eq!(state.inbound_count, 0);
    }

    #[test]
    fn record_sent_increments_correctly() {
        let mut state = SmState::new();
        state.record_sent(3);
        assert_eq!(state.outbound_count, 3);
        state.record_sent(2);
        assert_eq!(state.outbound_count, 5);
    }

    #[test]
    fn process_ack_updates_server_h() {
        let mut state = SmState::new();
        state.process_ack(10);
        assert_eq!(state.server_h, 10);
        state.process_ack(15);
        assert_eq!(state.server_h, 15);
    }
}
