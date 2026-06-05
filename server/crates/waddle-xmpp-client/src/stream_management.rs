use std::collections::VecDeque;

use chrono::{DateTime, SecondsFormat, Utc};
use minidom::Element;

use crate::error::{ClientError, ClientResult};
use crate::request::StanzaId;

pub const NS_SM: &str = "urn:xmpp:sm:3";
const NS_DELAY: &str = "urn:xmpp:delay";

#[derive(Debug, Clone, PartialEq, Eq)]
struct QueuedOutboundStanza {
    element: Element,
    message_stanza_id: Option<StanzaId>,
    sent_at: DateTime<Utc>,
}

/// In-memory XEP-0198 resume snapshot carried across a reconnect attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmResumeState {
    previd: String,
    inbound_h: u32,
    outbound_h: u32,
    max_resume_seconds: Option<u32>,
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
            max_resume_seconds: None,
            outbound_queue: VecDeque::new(),
        })
    }

    pub fn with_max_resume_seconds(mut self, max_resume_seconds: Option<u32>) -> Self {
        self.max_resume_seconds = max_resume_seconds;
        self
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

    pub fn from_unhandled_outbound_stanzas(
        previd: impl Into<String>,
        inbound_h: u32,
        outbound_h: u32,
        stanzas: impl IntoIterator<Item = Element>,
    ) -> ClientResult<Self> {
        let outbound_queue = stanzas
            .into_iter()
            .map(|element| QueuedOutboundStanza {
                message_stanza_id: message_delivery_stanza_id(&element),
                sent_at: existing_delay_stamp(&element).unwrap_or_else(Utc::now),
                element,
            })
            .collect();
        Self::from_outbound_queue(previd, inbound_h, outbound_h, outbound_queue)
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

    pub fn max_resume_seconds(&self) -> Option<u32> {
        self.max_resume_seconds
    }

    pub fn has_unhandled_outbound_stanzas(&self) -> bool {
        !self.outbound_queue.is_empty()
    }

    pub fn unhandled_outbound_stanzas(&self) -> impl Iterator<Item = &Element> {
        self.outbound_queue.iter().map(|queued| &queued.element)
    }

    pub fn unhandled_message_stanza_ids(&self) -> Vec<StanzaId> {
        self.outbound_queue
            .iter()
            .filter_map(|queued| queued.message_stanza_id.clone())
            .collect()
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
    /// Advertised server resumption window in seconds, when the server supplied one.
    pub max_resume_seconds: Option<u32>,
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
        let queue_len = u32::try_from(resume_state.outbound_queue.len()).unwrap_or(u32::MAX);
        // The queue contains only stanzas not yet handled by the server.
        Self {
            outbound_count: resume_state.outbound_h(),
            inbound_count: resume_state.inbound_h(),
            server_h: resume_state.outbound_h().wrapping_sub(queue_len),
            previd: Some(resume_state.previd().to_string()),
            max_resume_seconds: resume_state.max_resume_seconds(),
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
            .map(|state| state.with_max_resume_seconds(self.max_resume_seconds))
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
            message_stanza_id: message_delivery_stanza_id(element),
            element: element.clone(),
            sent_at: existing_delay_stamp(element).unwrap_or_else(Utc::now),
        });
    }

    /// Start a fresh outbound SM sequence after sending `<enable/>`.
    pub fn start_outbound(&mut self) {
        self.outbound_count = 0;
        self.server_h = 0;
        self.max_resume_seconds = None;
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
        let handled_since_last_ack = h.wrapping_sub(self.server_h);
        self.server_h = h;
        let mut acked = Vec::new();
        let to_drop = usize::try_from(handled_since_last_ack)
            .unwrap_or(usize::MAX)
            .min(self.outbound_queue.len());
        for _ in 0..to_drop {
            if let Some(queued) = self.outbound_queue.pop_front() {
                if let Some(stanza_id) = queued.message_stanza_id {
                    acked.push(stanza_id);
                }
            }
        }
        acked
    }

    pub fn handled_count_too_high(&self, h: u32) -> bool {
        h.wrapping_sub(self.server_h) > self.outbound_count.wrapping_sub(self.server_h)
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

    pub fn unhandled_stanzas_for_fallback_retry(&self) -> Vec<Element> {
        self.outbound_queue
            .iter()
            .map(QueuedOutboundStanza::element_for_fallback_retry)
            .collect()
    }

    pub fn unhandled_message_stanza_ids(&self) -> Vec<StanzaId> {
        self.outbound_queue
            .iter()
            .filter_map(|queued| queued.message_stanza_id.clone())
            .collect()
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
            b = b.attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true");
        }
        if let Some(max) = max {
            b = b.attr(
                minidom::rxml::xml_ncname!("max").to_owned(),
                max.to_string(),
            );
        }
        b.build()
    }

    /// Build `<resume xmlns='urn:xmpp:sm:3' previd='ID' h='N'/>`.
    pub fn build_resume(previd: &str, h: u32) -> Element {
        Element::builder("resume", NS_SM)
            .attr(minidom::rxml::xml_ncname!("previd").to_owned(), previd)
            .attr(minidom::rxml::xml_ncname!("h").to_owned(), h.to_string())
            .build()
    }

    /// Build `<r xmlns='urn:xmpp:sm:3'/>` to request an ack.
    pub fn build_request_ack() -> Element {
        Element::builder("r", NS_SM).build()
    }

    /// Build `<a xmlns='urn:xmpp:sm:3' h='N'/>` to acknowledge `h` stanzas.
    pub fn build_ack(h: u32) -> Element {
        Element::builder("a", NS_SM)
            .attr(minidom::rxml::xml_ncname!("h").to_owned(), h.to_string())
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

    /// Extract the advertised resumption window from a resumable `<enabled/>`.
    pub fn parse_enabled_max(element: &Element) -> Option<u32> {
        if Self::parse_enabled(element).is_some() {
            element.attr("max")?.parse().ok()
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

impl QueuedOutboundStanza {
    fn element_for_fallback_retry(&self) -> Element {
        if self.element.name() != "message" {
            return self.element.clone();
        }
        if self.element.get_child("delay", NS_DELAY).is_some() {
            return self.element.clone();
        }

        let stamp = self.sent_at.to_rfc3339_opts(SecondsFormat::Millis, true);
        let mut element = self.element.clone();
        element.append_child(
            Element::builder("delay", NS_DELAY)
                .attr(minidom::rxml::xml_ncname!("stamp").to_owned(), stamp)
                .build(),
        );
        element
    }
}

fn existing_delay_stamp(element: &Element) -> Option<DateTime<Utc>> {
    element
        .get_child("delay", NS_DELAY)?
        .attr("stamp")
        .and_then(|stamp| DateTime::parse_from_rfc3339(stamp).ok())
        .map(|stamp| stamp.with_timezone(&Utc))
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
        assert!(xml.contains("resume='true'"), "xml: {xml}");
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
        assert!(xml.contains("h='42'"), "xml: {xml}");
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
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "abc123")
            .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
            .attr(minidom::rxml::xml_ncname!("max").to_owned(), "300")
            .build();
        assert_eq!(SmState::parse_enabled(&el), Some("abc123".to_string()));
        assert_eq!(SmState::parse_enabled_max(&el), Some(300));
    }

    #[test]
    fn parse_enabled_requires_resumable_response() {
        let el = Element::builder("enabled", NS_SM)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "abc123")
            .attr(minidom::rxml::xml_ncname!("max").to_owned(), "300")
            .build();
        assert_eq!(SmState::parse_enabled(&el), None);
        assert_eq!(SmState::parse_enabled_max(&el), None);

        let el = Element::builder("enabled", NS_SM)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "abc123")
            .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "false")
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
        let el = Element::builder("enable", NS_SM)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "abc")
            .build();
        assert_eq!(SmState::parse_enabled(&el), None);

        let el2 = Element::builder("enabled", "urn:ietf:params:xml:ns:xmpp-bind")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "abc")
            .build();
        assert_eq!(SmState::parse_enabled(&el2), None);
    }

    #[test]
    fn parse_ack_h_extracts_value() {
        let el = Element::builder("a", NS_SM)
            .attr(minidom::rxml::xml_ncname!("h").to_owned(), "7")
            .build();
        assert_eq!(SmState::parse_ack_h(&el), Some(7));
    }

    #[test]
    fn parse_ack_h_returns_none_for_wrong_element() {
        let el = Element::builder("b", NS_SM)
            .attr(minidom::rxml::xml_ncname!("h").to_owned(), "7")
            .build();
        assert_eq!(SmState::parse_ack_h(&el), None);

        let el2 = Element::builder("a", "jabber:client")
            .attr(minidom::rxml::xml_ncname!("h").to_owned(), "7")
            .build();
        assert_eq!(SmState::parse_ack_h(&el2), None);
    }

    #[test]
    fn parse_ack_h_returns_none_for_bad_parse() {
        let el = Element::builder("a", NS_SM)
            .attr(minidom::rxml::xml_ncname!("h").to_owned(), "notanumber")
            .build();
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

    #[test]
    fn process_ack_trims_queue_across_u32_wrap() {
        let mut state = SmState::new();
        state.outbound_enabled = true;
        state.server_h = u32::MAX - 1;
        state.outbound_count = u32::MAX - 1;

        state.record_sent_stanza(
            &Element::builder("message", "jabber:client")
                .attr(
                    minidom::rxml::xml_ncname!("id").to_owned(),
                    "last-before-wrap",
                )
                .build(),
        );
        state.record_sent_stanza(
            &Element::builder("message", "jabber:client")
                .attr(
                    minidom::rxml::xml_ncname!("id").to_owned(),
                    "first-after-wrap",
                )
                .build(),
        );

        assert!(!state.handled_count_too_high(0));
        let acked = state.process_ack(0);

        assert_eq!(
            acked
                .iter()
                .map(|stanza_id| stanza_id.as_str())
                .collect::<Vec<_>>(),
            vec!["last-before-wrap", "first-after-wrap"]
        );
        assert!(state.outbound_queue.is_empty());
    }

    #[test]
    fn from_resume_state_restores_prior_server_ack_position() {
        let mut state = SmState::new();
        state.start_outbound();
        state.previd = Some("previous-stream".to_string());
        for id in 1..=10 {
            state.record_sent_stanza(
                &Element::builder("message", "jabber:client")
                    .attr(
                        minidom::rxml::xml_ncname!("id").to_owned(),
                        format!("msg-{id}"),
                    )
                    .build(),
            );
        }

        let acked = state.process_ack(8);
        assert_eq!(acked.len(), 8);

        let resume_state = state.resume_state().expect("resume state");
        let mut restored = SmState::from_resume_state(&resume_state);

        assert_eq!(restored.server_h, 8);
        assert!(!restored.handled_count_too_high(9));
        let acked_after_resume = restored.process_ack(9);
        assert_eq!(
            acked_after_resume
                .iter()
                .map(|stanza_id| stanza_id.as_str())
                .collect::<Vec<_>>(),
            vec!["msg-9"]
        );
        assert_eq!(
            restored
                .outbound_queue
                .iter()
                .filter_map(|queued| queued.message_stanza_id.as_ref())
                .map(|stanza_id| stanza_id.as_str())
                .collect::<Vec<_>>(),
            vec!["msg-10"]
        );
    }

    #[test]
    fn resume_state_reports_unhandled_outbound_queue_presence() {
        let mut state = SmState::new();
        state.start_outbound();
        state.previd = Some("previous-stream".to_string());
        state.record_sent_stanza(
            &Element::builder("message", "jabber:client")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "unacked")
                .build(),
        );

        let resume_state = state.resume_state().expect("resume state");
        assert!(resume_state.has_unhandled_outbound_stanzas());

        state.process_ack(1);
        let resume_state = state.resume_state().expect("resume state");
        assert!(!resume_state.has_unhandled_outbound_stanzas());
    }

    #[test]
    fn resume_state_carries_advertised_max_resume_window() {
        let mut state = SmState::new();
        state.previd = Some("previous-stream".to_string());
        state.max_resume_seconds = Some(300);

        let resume_state = state.resume_state().expect("resume state");
        assert_eq!(resume_state.max_resume_seconds(), Some(300));

        let restored = SmState::from_resume_state(&resume_state);
        assert_eq!(restored.max_resume_seconds, Some(300));
    }

    #[test]
    fn resume_state_can_restore_serialized_unhandled_stanzas() {
        let stanza = Element::builder("message", "jabber:client")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "unacked")
            .build();

        let resume_state = SmResumeState::from_unhandled_outbound_stanzas(
            "previous-stream",
            4,
            9,
            [stanza.clone()],
        )
        .expect("resume state");

        assert!(resume_state.has_unhandled_outbound_stanzas());
        assert_eq!(
            resume_state
                .unhandled_outbound_stanzas()
                .collect::<Vec<_>>(),
            vec![&stanza],
        );
    }

    #[test]
    fn fallback_retry_preserves_existing_delay_stamp() {
        let mut state = SmState::new();
        state.outbound_enabled = true;
        let message = Element::builder("message", "jabber:client")
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "already-delayed",
            )
            .append(
                Element::builder("delay", NS_DELAY)
                    .attr(
                        minidom::rxml::xml_ncname!("stamp").to_owned(),
                        "2024-01-15T10:00:00Z",
                    )
                    .build(),
            )
            .build();

        state.record_sent_stanza(&message);
        let retry = state
            .unhandled_stanzas_for_fallback_retry()
            .into_iter()
            .next()
            .expect("fallback retry");
        let delays = retry
            .children()
            .filter(|child| child.name() == "delay" && child.ns() == NS_DELAY)
            .collect::<Vec<_>>();

        assert_eq!(delays.len(), 1);
        assert_eq!(delays[0].attr("stamp"), Some("2024-01-15T10:00:00Z"));
    }
}
