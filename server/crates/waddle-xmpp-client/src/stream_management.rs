use std::collections::VecDeque;

use chrono::{DateTime, SecondsFormat, Utc};
use minidom::Element;

use crate::error::{ClientError, ClientResult};
use crate::request::StanzaId;

pub const NS_SM: &str = "urn:xmpp:sm:3";
const NS_DELAY: &str = "urn:xmpp:delay";

const ACK_RETRY_DELAYS_MS: [u64; 5] = [250, 500, 1_000, 2_000, 5_000];
const ACK_RESPONSE_TIMEOUT_MS: u64 = 5_000;
const ACK_PROGRESS_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SentStanzaKind {
    NotCountable,
    New,
    Replay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckRequest {
    pub attempt: u32,
    pub unacked: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SentStanzaResult {
    pub kind: SentStanzaKind,
    pub request: Option<AckRequest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckObservation {
    pub progressed: bool,
    pub latency_ms: Option<u64>,
    pub unacked: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessAckResult {
    pub acked: Vec<StanzaId>,
    pub observation: AckObservation,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AckTimerPoll {
    pub request: Option<AckRequest>,
    pub request_timed_out: bool,
    pub progress_stalled_ms: Option<u64>,
}

#[derive(Debug, Clone, Default)]
struct AckCadence {
    request_sent_at_ms: Option<u64>,
    next_request_at_ms: Option<u64>,
    progress_started_at_ms: Option<u64>,
    retry_index: usize,
    request_attempt: u32,
}

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
    ack_cadence: AckCadence,
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
        let _ = self.record_sent_stanza_at(element, 0);
    }

    /// Record a transport-confirmed outbound element and update the
    /// client-to-server acknowledgement cadence.
    ///
    /// The caller supplies a monotonic millisecond timestamp. Keeping time
    /// numeric and injected makes this state identical on native and WASM and
    /// lets tests advance every timeout without sleeping.
    pub fn record_sent_stanza_at(&mut self, element: &Element, now_ms: u64) -> SentStanzaResult {
        if !self.outbound_enabled || !matches!(element.name(), "iq" | "message" | "presence") {
            return SentStanzaResult {
                kind: SentStanzaKind::NotCountable,
                request: None,
            };
        }

        let kind = if self.suppress_replay_sent_record(element) {
            SentStanzaKind::Replay
        } else {
            self.record_sent(1);
            self.outbound_queue.push_back(QueuedOutboundStanza {
                message_stanza_id: message_delivery_stanza_id(element),
                element: element.clone(),
                sent_at: existing_delay_stamp(element).unwrap_or_else(Utc::now),
            });
            SentStanzaKind::New
        };

        let request = self.arm_after_outbound_at(now_ms);
        SentStanzaResult { kind, request }
    }

    /// Start a fresh inbound SM sequence after receiving `<enabled/>`.
    ///
    /// XEP-0198 §5: the received-stanza counter is "set to zero and
    /// started after receiving either `<enable/>` or `<enabled/>`".
    /// Without this reset, a failed resume followed by a fresh
    /// `<enable/>` carries the previous session's inbound count into
    /// the new session's `h`, compounding per reconnect cycle until
    /// the server rejects `<resume/>` with handled-count-too-high
    /// (issue #1181).
    pub fn start_inbound(&mut self) {
        self.inbound_count = 0;
    }

    /// Start a fresh outbound SM sequence after sending `<enable/>`.
    pub fn start_outbound(&mut self) {
        self.outbound_count = 0;
        self.server_h = 0;
        self.max_resume_seconds = None;
        self.outbound_enabled = true;
        self.outbound_queue.clear();
        self.replay_in_flight.clear();
        self.ack_cadence = AckCadence::default();
    }

    /// Stop all SM counters after `<failed/>` or stream termination.
    pub fn stop(&mut self) {
        self.outbound_enabled = false;
        self.enabled = false;
        self.ack_cadence = AckCadence::default();
    }

    /// Increment the inbound stanza counter by `count`.
    pub fn record_received(&mut self, count: u32) {
        self.inbound_count = self.inbound_count.wrapping_add(count);
    }

    /// Update `server_h` from an `<a h='...'/>` ack.
    pub fn process_ack(&mut self, h: u32) -> Vec<StanzaId> {
        self.process_ack_at(h, 0).acked
    }

    /// Process a valid server acknowledgement at `now_ms`.
    ///
    /// Every valid `<a/>`, including a duplicate `h`, releases the current
    /// request. If work remains, the next request is scheduled instead of
    /// emitted synchronously so a non-progress response cannot create an
    /// `<r/>`/`<a/>` ping-pong loop.
    pub fn process_ack_at(&mut self, h: u32, now_ms: u64) -> ProcessAckResult {
        let previous_h = self.server_h;
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

        let latency_ms = self
            .ack_cadence
            .request_sent_at_ms
            .take()
            .map(|sent_at| now_ms.saturating_sub(sent_at));
        let progressed = h != previous_h;
        let unacked = self.unacked_count();
        if unacked == 0 {
            self.ack_cadence = AckCadence::default();
        } else {
            if progressed {
                self.ack_cadence.retry_index = 0;
                self.ack_cadence.request_attempt = 0;
                self.ack_cadence.progress_started_at_ms = Some(now_ms);
            } else if self.ack_cadence.progress_started_at_ms.is_none() {
                self.ack_cadence.progress_started_at_ms = Some(now_ms);
            }
            let retry_index = self
                .ack_cadence
                .retry_index
                .min(ACK_RETRY_DELAYS_MS.len() - 1);
            self.ack_cadence.next_request_at_ms =
                Some(now_ms.saturating_add(ACK_RETRY_DELAYS_MS[retry_index]));
            self.ack_cadence.retry_index =
                (self.ack_cadence.retry_index + 1).min(ACK_RETRY_DELAYS_MS.len() - 1);
        }

        ProcessAckResult {
            acked,
            observation: AckObservation {
                progressed,
                latency_ms,
                unacked,
            },
        }
    }

    /// Request an acknowledgement immediately when SM has outstanding work
    /// and no request is awaiting a response. Used by the pagehide handoff;
    /// regular sends and timers use the same state transition.
    pub fn request_ack_now_at(&mut self, now_ms: u64) -> Option<AckRequest> {
        if !self.enabled
            || !self.outbound_enabled
            || self.outbound_queue.is_empty()
            || self.ack_cadence.request_sent_at_ms.is_some()
        {
            return None;
        }
        self.ack_cadence.next_request_at_ms = None;
        Some(self.mark_ack_request_sent_at(now_ms))
    }

    /// Poll response/retry/progress deadlines without performing I/O.
    pub fn poll_ack_timer_at(&mut self, now_ms: u64) -> AckTimerPoll {
        if !self.enabled || !self.outbound_enabled || self.outbound_queue.is_empty() {
            self.ack_cadence = AckCadence::default();
            return AckTimerPoll::default();
        }

        if let Some(started_at) = self.ack_cadence.progress_started_at_ms {
            let stalled_ms = now_ms.saturating_sub(started_at);
            if stalled_ms >= ACK_PROGRESS_TIMEOUT_MS {
                return AckTimerPoll {
                    progress_stalled_ms: Some(stalled_ms),
                    ..AckTimerPoll::default()
                };
            }
        }

        if let Some(sent_at) = self.ack_cadence.request_sent_at_ms {
            if now_ms.saturating_sub(sent_at) >= ACK_RESPONSE_TIMEOUT_MS {
                self.ack_cadence.request_sent_at_ms = None;
                return AckTimerPoll {
                    request: Some(self.mark_ack_request_sent_at(now_ms)),
                    request_timed_out: true,
                    progress_stalled_ms: None,
                };
            }
            return AckTimerPoll::default();
        }

        if self
            .ack_cadence
            .next_request_at_ms
            .is_some_and(|deadline| now_ms >= deadline)
        {
            self.ack_cadence.next_request_at_ms = None;
            return AckTimerPoll {
                request: Some(self.mark_ack_request_sent_at(now_ms)),
                ..AckTimerPoll::default()
            };
        }

        AckTimerPoll::default()
    }

    /// Milliseconds until the next SM timer action, if any.
    pub fn next_ack_wakeup_in_ms(&self, now_ms: u64) -> Option<u64> {
        if !self.enabled || !self.outbound_enabled || self.outbound_queue.is_empty() {
            return None;
        }

        let progress_deadline = self
            .ack_cadence
            .progress_started_at_ms
            .map(|started_at| started_at.saturating_add(ACK_PROGRESS_TIMEOUT_MS));
        let response_deadline = self
            .ack_cadence
            .request_sent_at_ms
            .map(|sent_at| sent_at.saturating_add(ACK_RESPONSE_TIMEOUT_MS));
        [
            progress_deadline,
            response_deadline,
            self.ack_cadence.next_request_at_ms,
        ]
        .into_iter()
        .flatten()
        .min()
        .map(|deadline| deadline.saturating_sub(now_ms))
    }

    pub fn handled_count_too_high(&self, h: u32) -> bool {
        h.wrapping_sub(self.server_h) > self.outbound_count.wrapping_sub(self.server_h)
    }

    pub fn unacked_count(&self) -> u32 {
        self.outbound_count.wrapping_sub(self.server_h)
    }

    /// Prepare the surviving outbound queue for XEP-0198 retransmission after
    /// applying a successful `<resumed h='…'/>` response.
    ///
    /// Treating `<resumed/>` exactly like a normal `<a/>` first is important:
    /// it trims handled stanzas and establishes the no-progress deadline.
    /// Unlike an ordinary ack, however, the protocol requires the remaining
    /// queue to be retransmitted immediately. Clear the delayed retry/request
    /// latch so the first replay's transport-confirmed `MessageSent` arms an
    /// immediate `<r/>` without resetting counters or recounting the stanza.
    pub fn begin_replay_transition_at(&mut self, now_ms: u64) {
        if self.outbound_queue.is_empty() {
            self.ack_cadence = AckCadence::default();
            return;
        }

        let progress_started_at_ms = self.ack_cadence.progress_started_at_ms.unwrap_or(now_ms);
        self.ack_cadence.request_sent_at_ms = None;
        self.ack_cadence.next_request_at_ms = None;
        self.ack_cadence.progress_started_at_ms = Some(progress_started_at_ms);
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

    fn arm_after_outbound_at(&mut self, now_ms: u64) -> Option<AckRequest> {
        if !self.enabled || self.outbound_queue.is_empty() {
            return None;
        }
        if self.ack_cadence.progress_started_at_ms.is_none() {
            self.ack_cadence.progress_started_at_ms = Some(now_ms);
        }
        if self.ack_cadence.request_sent_at_ms.is_some()
            || self.ack_cadence.next_request_at_ms.is_some()
        {
            return None;
        }
        Some(self.mark_ack_request_sent_at(now_ms))
    }

    fn mark_ack_request_sent_at(&mut self, now_ms: u64) -> AckRequest {
        self.ack_cadence.request_sent_at_ms = Some(now_ms);
        self.ack_cadence.request_attempt = self.ack_cadence.request_attempt.saturating_add(1);
        AckRequest {
            attempt: self.ack_cadence.request_attempt,
            unacked: self.unacked_count(),
        }
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
    fn unchanged_server_h_replays_a_timed_out_outbound_stanza() {
        let mut state = SmState::new();
        state.start_outbound();
        state.previd = Some("timed-out-stream".to_string());
        let timed_out = Element::builder("message", "jabber:client")
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "timed-out-message",
            )
            .build();
        state.record_sent_stanza(&timed_out);

        // The server ended the transport without advancing h, so processing
        // its last acknowledgement must leave the stanza sender-owned.
        assert!(state.process_ack(0).is_empty());
        let resume_state = state.resume_state().expect("resume state");
        let mut resumed = SmState::from_resume_state(&resume_state);

        assert_eq!(resumed.mark_unhandled_for_replay(), vec![timed_out]);
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

    fn enabled_state() -> SmState {
        let mut state = SmState::new();
        state.start_outbound();
        state.enabled = true;
        state
    }

    fn message(id: &str) -> Element {
        Element::builder("message", "jabber:client")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
            .build()
    }

    #[test]
    fn first_countable_stanza_requests_ack_and_burst_coalesces() {
        let mut state = enabled_state();

        let first = state.record_sent_stanza_at(&message("one"), 100);
        let second = state.record_sent_stanza_at(&message("two"), 101);

        assert_eq!(first.kind, SentStanzaKind::New);
        assert_eq!(
            first.request,
            Some(AckRequest {
                attempt: 1,
                unacked: 1,
            })
        );
        assert_eq!(second.kind, SentStanzaKind::New);
        assert_eq!(second.request, None);
        assert_eq!(state.unacked_count(), 2);
    }

    #[test]
    fn valid_non_progress_ack_releases_request_and_schedules_retry() {
        let mut state = enabled_state();
        state.record_sent_stanza_at(&message("one"), 100);

        let processed = state.process_ack_at(0, 120);

        assert!(!processed.observation.progressed);
        assert_eq!(processed.observation.latency_ms, Some(20));
        assert_eq!(state.next_ack_wakeup_in_ms(120), Some(250));
        assert_eq!(state.poll_ack_timer_at(369), AckTimerPoll::default());
        assert_eq!(
            state.poll_ack_timer_at(370).request,
            Some(AckRequest {
                attempt: 2,
                unacked: 1,
            })
        );
    }

    #[test]
    fn progress_resets_retry_backoff_while_work_remains() {
        let mut state = enabled_state();
        state.record_sent_stanza_at(&message("one"), 0);
        state.record_sent_stanza_at(&message("two"), 1);
        state.process_ack_at(0, 10);
        let retry = state.poll_ack_timer_at(260);
        assert_eq!(retry.request.map(|request| request.attempt), Some(2));

        let progressed = state.process_ack_at(1, 300);

        assert!(progressed.observation.progressed);
        assert_eq!(progressed.observation.unacked, 1);
        assert_eq!(state.next_ack_wakeup_in_ms(300), Some(250));
        assert_eq!(
            state.poll_ack_timer_at(550).request,
            Some(AckRequest {
                attempt: 1,
                unacked: 1,
            })
        );
    }

    #[test]
    fn non_progress_retry_schedule_reaches_and_repeats_five_second_cap() {
        let mut state = enabled_state();
        state.record_sent_stanza_at(&message("one"), 0);
        let mut now_ms = 0;

        for (expected_delay_ms, expected_attempt) in [
            (250, 2),
            (500, 3),
            (1_000, 4),
            (2_000, 5),
            (5_000, 6),
            (5_000, 7),
        ] {
            let observation = state.process_ack_at(0, now_ms);
            assert!(!observation.observation.progressed);
            assert_eq!(state.next_ack_wakeup_in_ms(now_ms), Some(expected_delay_ms));
            assert_eq!(
                state.poll_ack_timer_at(now_ms + expected_delay_ms - 1),
                AckTimerPoll::default()
            );
            now_ms += expected_delay_ms;
            assert_eq!(
                state.poll_ack_timer_at(now_ms).request,
                Some(AckRequest {
                    attempt: expected_attempt,
                    unacked: 1,
                })
            );
        }
    }

    #[test]
    fn missing_ack_response_times_out_and_reissues_request() {
        let mut state = enabled_state();
        state.record_sent_stanza_at(&message("one"), 1_000);

        assert_eq!(state.next_ack_wakeup_in_ms(1_000), Some(5_000));
        let poll = state.poll_ack_timer_at(6_000);

        assert!(poll.request_timed_out);
        assert_eq!(
            poll.request,
            Some(AckRequest {
                attempt: 2,
                unacked: 1,
            })
        );
    }

    #[test]
    fn thirty_seconds_without_h_progress_requests_unclean_reconnect() {
        let mut state = enabled_state();
        state.record_sent_stanza_at(&message("one"), 5_000);

        let poll = state.poll_ack_timer_at(35_000);

        assert_eq!(poll.progress_stalled_ms, Some(30_000));
        assert_eq!(poll.request, None);
    }

    #[test]
    fn replay_arms_ack_request_without_incrementing_counter() {
        let replay = message("replay");
        let resume =
            SmResumeState::from_unhandled_outbound_stanzas("previous", 0, 1, [replay.clone()])
                .expect("resume state");
        let mut state = SmState::from_resume_state(&resume);
        state.enabled = true;
        state.outbound_enabled = true;
        assert_eq!(state.mark_unhandled_for_replay(), vec![replay.clone()]);

        let sent = state.record_sent_stanza_at(&replay, 50);

        assert_eq!(sent.kind, SentStanzaKind::Replay);
        assert_eq!(sent.request.map(|request| request.unacked), Some(1));
        assert_eq!(state.outbound_count, 1);
    }

    #[test]
    fn resumed_replay_transition_supersedes_delayed_retry_but_keeps_progress_deadline() {
        let replay = message("replay");
        let resume =
            SmResumeState::from_unhandled_outbound_stanzas("previous", 0, 1, [replay.clone()])
                .expect("resume state");
        let mut state = SmState::from_resume_state(&resume);
        state.enabled = true;
        state.outbound_enabled = true;

        state.process_ack_at(0, 10_000);
        assert_eq!(state.next_ack_wakeup_in_ms(10_000), Some(250));

        state.begin_replay_transition_at(10_000);
        assert_eq!(state.mark_unhandled_for_replay(), vec![replay.clone()]);
        let sent = state.record_sent_stanza_at(&replay, 10_001);

        assert_eq!(sent.kind, SentStanzaKind::Replay);
        assert_eq!(
            sent.request,
            Some(AckRequest {
                attempt: 1,
                unacked: 1,
            })
        );
        assert_eq!(state.outbound_count, 1);
        assert_eq!(
            state.poll_ack_timer_at(40_000).progress_stalled_ms,
            Some(30_000)
        );
    }

    #[test]
    fn resumed_replay_transition_cancels_cadence_when_ack_handles_everything() {
        let resume =
            SmResumeState::from_unhandled_outbound_stanzas("previous", 0, 1, [message("one")])
                .expect("resume state");
        let mut state = SmState::from_resume_state(&resume);
        state.enabled = true;
        state.outbound_enabled = true;

        let processed = state.process_ack_at(1, 500);
        assert_eq!(processed.observation.unacked, 0);
        state.begin_replay_transition_at(500);

        assert!(state.mark_unhandled_for_replay().is_empty());
        assert_eq!(state.next_ack_wakeup_in_ms(500), None);
        assert_eq!(state.poll_ack_timer_at(u64::MAX), AckTimerPoll::default());
    }

    #[test]
    fn sm_disabled_and_ack_request_elements_never_request_recursively() {
        let mut state = SmState::new();
        state.start_outbound();
        let disabled = state.record_sent_stanza_at(&message("one"), 0);
        assert_eq!(disabled.kind, SentStanzaKind::New);
        assert_eq!(disabled.request, None);

        state.enabled = true;
        let before = state.outbound_count;
        let request = state.record_sent_stanza_at(&SmState::build_request_ack(), 1);
        assert_eq!(request.kind, SentStanzaKind::NotCountable);
        assert_eq!(request.request, None);
        assert_eq!(state.outbound_count, before);
    }
}
