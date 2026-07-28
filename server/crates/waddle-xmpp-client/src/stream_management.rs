use std::collections::VecDeque;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use minidom::Element;
use xmpp_parsers::{iq::Iq, message::Message, presence::Presence};

use crate::error::{ClientError, ClientResult};
use crate::request::StanzaId;
use crate::state::StreamId;

pub const NS_SM: &str = "urn:xmpp:sm:3";
const NS_CLIENT: &str = "jabber:client";
const NS_DELAY: &str = "urn:xmpp:delay";
const NS_STANZA_ERRORS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";
const ACK_RETRY_DELAYS_MS: [i64; 5] = [250, 500, 1_000, 2_000, 5_000];
const ACK_REQUEST_TIMEOUT_MS: i64 = 5_000;
pub const SM_PROGRESS_TIMEOUT_MS: i64 = 30_000;

/// A bounded outcome for the Rust-owned XEP-0198 acknowledgement clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmAcknowledgementClockAction {
    Retry { attempt: u8 },
    RequestTimedOut,
    ProgressTimedOut,
}

/// A validated inbound XEP-0198 control element.
///
/// Raw XML is accepted at the transport boundary only. The runtime receives
/// this typed form so that no acknowledgement, queue, or resumption mutation
/// can happen before the control's XEP-0198 shape has been checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmInboundControl {
    RequestAck,
    Ack {
        h: u32,
    },
    Enabled {
        previd: Option<StreamId>,
        max_resume_seconds: Option<u32>,
    },
    Resumed {
        h: u32,
        previd: StreamId,
    },
    Failed {
        h: Option<u32>,
    },
}

/// The received control element is not a valid XEP-0198 wire shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidSmInboundControl;

/// The structural error returned when durable XEP-0198 state does not contain
/// a countable client stanza.
///
/// XEP-0198 control elements are stream-level commands, not stanzas, and
/// therefore must never enter the replay queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidPersistedSmStanza;

impl std::fmt::Display for InvalidPersistedSmStanza {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("persisted SM entry is not a valid countable jabber:client stanza")
    }
}

impl std::error::Error for InvalidPersistedSmStanza {}

/// A countable client stanza retained without reserializing its extension
/// payloads.
///
/// The typed `xmpp_parsers` parse below validates the root stanza.  The
/// original element is retained so extension payload ordering and opaque
/// extension contents survive an XEP-0198 replay exactly as sent.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CountableReplayStanza {
    element: Element,
}

impl TryFrom<Element> for CountableReplayStanza {
    type Error = InvalidPersistedSmStanza;

    fn try_from(element: Element) -> Result<Self, Self::Error> {
        if element.ns() != NS_CLIENT {
            return Err(InvalidPersistedSmStanza);
        }

        let parsed = match element.name() {
            "message" => Message::try_from(element.clone()).map(|_| ()),
            "presence" => Presence::try_from(element.clone()).map(|_| ()),
            "iq" => Iq::try_from(element.clone()).map(|_| ()),
            _ => return Err(InvalidPersistedSmStanza),
        };
        parsed.map_err(|_| InvalidPersistedSmStanza)?;

        Ok(Self { element })
    }
}

/// A sender-owned outbound stanza retained for XEP-0198 resumption.
///
/// The core deliberately keeps the parsed stanza, its stable message identity,
/// and original send time typed. Hosts serialize XML only at their durable
/// storage boundary and parse it exactly once when restoring this entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnhandledOutboundEntry {
    stanza: CountableReplayStanza,
    message_stanza_id: Option<StanzaId>,
    sent_at: DateTime<Utc>,
}

impl UnhandledOutboundEntry {
    /// Construct a replay entry at the persistence boundary.
    ///
    /// The XML parser has already produced one [`Element`]; this converts it
    /// once into a validated countable stanza before it can reach SM replay.
    pub fn try_new(
        element: Element,
        sent_at: DateTime<Utc>,
    ) -> Result<Self, InvalidPersistedSmStanza> {
        let stanza = CountableReplayStanza::try_from(element)?;
        Ok(Self {
            message_stanza_id: message_delivery_stanza_id(&stanza.element),
            stanza,
            sent_at,
        })
    }

    /// Expose the retained XML only for the literal persistence I/O boundary.
    pub fn stanza_for_persistence(&self) -> &Element {
        &self.stanza.element
    }

    pub fn sent_at(&self) -> DateTime<Utc> {
        self.sent_at
    }

    pub fn message_stanza_id(&self) -> Option<&StanzaId> {
        self.message_stanza_id.as_ref()
    }
}

/// In-memory XEP-0198 resume snapshot carried across a reconnect attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmResumeState {
    previd: StreamId,
    inbound_h: u32,
    outbound_h: u32,
    max_resume_seconds: Option<u32>,
    outbound_queue: VecDeque<UnhandledOutboundEntry>,
}

impl SmResumeState {
    pub fn new(previd: StreamId, inbound_h: u32, outbound_h: u32) -> ClientResult<Self> {
        if previd.as_str().trim().is_empty() {
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
        previd: StreamId,
        inbound_h: u32,
        outbound_h: u32,
        outbound_queue: VecDeque<UnhandledOutboundEntry>,
    ) -> ClientResult<Self> {
        let mut state = Self::new(previd, inbound_h, outbound_h)?;
        state.outbound_queue = outbound_queue;
        Ok(state)
    }

    pub fn from_unhandled_outbound_entries(
        previd: StreamId,
        inbound_h: u32,
        outbound_h: u32,
        entries: impl IntoIterator<Item = UnhandledOutboundEntry>,
    ) -> ClientResult<Self> {
        let outbound_queue = entries.into_iter().collect();
        Self::from_outbound_queue(previd, inbound_h, outbound_h, outbound_queue)
    }

    pub fn previd(&self) -> &StreamId {
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

    pub fn unhandled_outbound_entries(&self) -> impl Iterator<Item = &UnhandledOutboundEntry> {
        self.outbound_queue.iter()
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
    pub previd: Option<StreamId>,
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
    /// True after we have requested an acknowledgement for the current
    /// unhandled outbound tail. A valid `<a/>`, including one whose `h`
    /// makes no progress, clears this edge so the next unhandled stanza can
    /// ask again. This is deliberately protocol state rather than a browser
    /// timer: every transport uses the same XEP-0198 acknowledgement edge.
    ack_request_outstanding: bool,
    ack_request_started_at: Option<DateTime<Utc>>,
    next_ack_retry_at: Option<DateTime<Utc>>,
    ack_retry_attempt: u8,
    ack_request_timed_out: bool,
    last_ack_progress_at: Option<DateTime<Utc>>,
    progress_timed_out: bool,
    outbound_queue: VecDeque<UnhandledOutboundEntry>,
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
            previd: Some(resume_state.previd().clone()),
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
    /// Record an application stanza and report whether it opened a new
    /// unacknowledged tail that needs an immediate XEP-0198 `<r/>` request.
    pub fn record_sent_stanza(&mut self, element: &Element) -> bool {
        self.record_sent_stanza_at(element, Utc::now())
    }

    /// Record at an explicit clock instant so every timer boundary is testable.
    pub fn record_sent_stanza_at(&mut self, element: &Element, now: DateTime<Utc>) -> bool {
        if self.suppress_replay_sent_record(element) {
            return false;
        }

        let Ok(entry) = UnhandledOutboundEntry::try_new(
            element.clone(),
            existing_delay_stamp(element).unwrap_or(now),
        ) else {
            return false;
        };

        self.record_sent(1);
        self.outbound_queue.push_back(entry);
        self.arm_acknowledgement_clock(now)
    }

    /// Arm the acknowledgement policy for an already sender-owned tail.
    ///
    /// A successful XEP-0198 `<resumed/>` retains the prior outbound counter
    /// and may leave entries to replay. This method starts the same clock as a
    /// newly-sent stanza without recording or counting another stanza.
    pub fn arm_acknowledgement_clock(&mut self, now: DateTime<Utc>) -> bool {
        if self.outbound_queue.is_empty() || self.ack_request_outstanding {
            return false;
        }

        self.ack_request_outstanding = true;
        self.ack_request_started_at = Some(now);
        self.next_ack_retry_at = Some(now + Duration::milliseconds(ACK_RETRY_DELAYS_MS[0]));
        self.ack_retry_attempt = 0;
        self.ack_request_timed_out = false;
        if self.last_ack_progress_at.is_none() {
            self.last_ack_progress_at = Some(now);
            self.progress_timed_out = false;
        }
        true
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
        self.cancel_acknowledgement_clock();
    }

    /// Stop all SM counters after `<failed/>` or stream termination.
    pub fn stop(&mut self) {
        self.outbound_enabled = false;
        self.enabled = false;
        self.cancel_acknowledgement_clock();
    }

    /// Increment the inbound stanza counter by `count`.
    pub fn record_received(&mut self, count: u32) {
        self.inbound_count = self.inbound_count.wrapping_add(count);
    }

    /// Update `server_h` from an `<a h='...'/>` ack.
    pub fn process_ack(&mut self, h: u32) -> Vec<StanzaId> {
        self.process_ack_at(h, Utc::now())
    }

    /// Process a valid acknowledgement at an explicit time.
    pub fn process_ack_at(&mut self, h: u32, now: DateTime<Utc>) -> Vec<StanzaId> {
        // XEP-0198 permits unrequested acknowledgements and a peer may send
        // the same handled count again. Either is proof that the outstanding
        // `<r/>` reached a live peer, so it clears the request edge even when
        // no queued stanza is newly acknowledged.
        self.ack_request_outstanding = false;
        self.ack_request_started_at = None;
        self.next_ack_retry_at = None;
        self.ack_retry_attempt = 0;
        self.ack_request_timed_out = false;
        let handled_since_last_ack = h.wrapping_sub(self.server_h);
        if handled_since_last_ack != 0 {
            self.last_ack_progress_at = Some(now);
            self.progress_timed_out = false;
        }
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
        // Once the peer has handled the whole sender-owned tail there is
        // nothing left whose acknowledgement can make progress. Keeping the
        // old progress deadline would turn an otherwise idle, fully-acked
        // stream into a spurious reconnect thirty seconds later.
        if self.outbound_queue.is_empty() {
            self.cancel_acknowledgement_clock();
        } else {
            // A valid acknowledgement proves the peer is live, even when its
            // handled count has not advanced. Start a fresh request cadence
            // for the remaining tail, but deliberately leave
            // `last_ack_progress_at` alone: repeated no-progress `<a/>`s
            // must not postpone the 30-second h-progress deadline.
            self.arm_acknowledgement_clock(now);
        }
        acked
    }

    /// Advance acknowledgement policy using a caller-supplied clock.
    ///
    /// The first request is emitted with the first unhandled stanza. Retries
    /// occur at 250ms, 500ms, 1s, 2s, and 5s after that request, then stay
    /// on the five-second plateau. Five seconds without any valid `<a/>`
    /// emits an outcome but does not stop retries. Independently, 30s without
    /// `h` progress requires a resumable reconnect.
    pub fn poll_acknowledgement_clock(
        &mut self,
        now: DateTime<Utc>,
    ) -> Vec<SmAcknowledgementClockAction> {
        let mut actions = Vec::new();
        if self.ack_request_outstanding {
            if self
                .next_ack_retry_at
                .is_some_and(|deadline| now >= deadline)
            {
                let attempt = self.ack_retry_attempt + 1;
                self.ack_retry_attempt = attempt;
                self.next_ack_retry_at = self.ack_request_started_at.map(|started| {
                    let delay = ACK_RETRY_DELAYS_MS
                        .get(usize::from(attempt))
                        .copied()
                        .unwrap_or(ACK_REQUEST_TIMEOUT_MS);
                    let scheduled = started + Duration::milliseconds(delay);
                    if scheduled > now {
                        scheduled
                    } else {
                        now + Duration::milliseconds(ACK_REQUEST_TIMEOUT_MS)
                    }
                });
                actions.push(SmAcknowledgementClockAction::Retry { attempt });
            }
            if self.ack_request_started_at.is_some_and(|started| {
                now >= started + Duration::milliseconds(ACK_REQUEST_TIMEOUT_MS)
            }) && !self.ack_request_timed_out
            {
                self.ack_request_timed_out = true;
                actions.push(SmAcknowledgementClockAction::RequestTimedOut);
            }
        }
        if self.last_ack_progress_at.is_some_and(|progress| {
            now >= progress + Duration::milliseconds(SM_PROGRESS_TIMEOUT_MS)
        }) && !self.progress_timed_out
        {
            self.progress_timed_out = true;
            actions.push(SmAcknowledgementClockAction::ProgressTimedOut);
        }
        actions
    }

    pub fn cancel_acknowledgement_clock(&mut self) {
        self.ack_request_outstanding = false;
        self.ack_request_started_at = None;
        self.next_ack_retry_at = None;
        self.ack_retry_attempt = 0;
        self.ack_request_timed_out = false;
        self.last_ack_progress_at = None;
        self.progress_timed_out = false;
    }

    /// Whether the host needs to keep driving the XEP-0198 acknowledgement
    /// clock.
    ///
    /// A valid `<a/>` with an unhandled tail begins a fresh retry cadence
    /// without advancing `h`; its original 30-second no-progress deadline
    /// remains authoritative. Conversely, a fully acknowledged tail cancels
    /// both pieces of state. Hosts must use this predicate rather than
    /// deriving liveness from the outbound queue.
    pub fn acknowledgement_clock_pending(&self) -> bool {
        self.ack_request_outstanding || self.last_ack_progress_at.is_some()
    }

    pub fn handled_count_too_high(&self, h: u32) -> bool {
        h.wrapping_sub(self.server_h) > self.outbound_count.wrapping_sub(self.server_h)
    }

    /// Mark currently unhandled outbound stanzas for replay and return them.
    pub fn mark_unhandled_for_replay(&mut self) -> Vec<Element> {
        let replay: Vec<Element> = self
            .outbound_queue
            .iter()
            .map(|queued| queued.stanza.element.clone())
            .collect();
        self.replay_in_flight.extend(replay.iter().cloned());
        replay
    }

    pub fn unhandled_stanzas_for_fallback_retry(&self) -> Vec<Element> {
        self.outbound_queue
            .iter()
            .map(UnhandledOutboundEntry::element_for_fallback_retry)
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
    pub fn build_resume(previd: &StreamId, h: u32) -> Element {
        Element::builder("resume", NS_SM)
            .attr(
                minidom::rxml::xml_ncname!("previd").to_owned(),
                previd.as_str(),
            )
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

    /// Parse an inbound XEP-0198 control after validating its schema shape.
    pub fn parse_inbound_control(
        element: &Element,
    ) -> Result<SmInboundControl, InvalidSmInboundControl> {
        if element.ns() != NS_SM {
            return Err(InvalidSmInboundControl);
        }

        match element.name() {
            "r" if is_empty_control(element) && has_only_attributes(element, &[]) => {
                Ok(SmInboundControl::RequestAck)
            }
            "a" if is_empty_control(element) && has_only_attributes(element, &["h"]) => {
                let h = parse_exact_u32(element.attr("h").ok_or(InvalidSmInboundControl)?)
                    .ok_or(InvalidSmInboundControl)?;
                Ok(SmInboundControl::Ack { h })
            }
            "enabled"
                if is_empty_control(element)
                    && has_only_attributes(element, &["id", "location", "max", "resume"]) =>
            {
                let resume = element
                    .attr("resume")
                    .map(parse_xsd_boolean)
                    .transpose()?
                    .unwrap_or(false);
                let max_resume_seconds = element.attr("max").map(parse_positive_u32).transpose()?;
                let previd = if resume {
                    let id = element.attr("id").ok_or(InvalidSmInboundControl)?;
                    if id.is_empty() {
                        return Err(InvalidSmInboundControl);
                    }
                    Some(StreamId::new(id))
                } else {
                    None
                };
                Ok(SmInboundControl::Enabled {
                    previd,
                    max_resume_seconds,
                })
            }
            "resumed"
                if is_empty_control(element) && has_only_attributes(element, &["h", "previd"]) =>
            {
                let h = parse_exact_u32(element.attr("h").ok_or(InvalidSmInboundControl)?)
                    .ok_or(InvalidSmInboundControl)?;
                let previd = element.attr("previd").ok_or(InvalidSmInboundControl)?;
                if previd.is_empty() {
                    return Err(InvalidSmInboundControl);
                }
                Ok(SmInboundControl::Resumed {
                    h,
                    previd: StreamId::new(previd),
                })
            }
            "failed"
                if has_only_attributes(element, &["h"])
                    && has_optional_stanza_error_group(element) =>
            {
                let h = match element.attr("h") {
                    Some(value) => Some(parse_exact_u32(value).ok_or(InvalidSmInboundControl)?),
                    None => None,
                };
                Ok(SmInboundControl::Failed { h })
            }
            _ => Err(InvalidSmInboundControl),
        }
    }
}

fn is_empty_control(element: &Element) -> bool {
    element.nodes().next().is_none()
}

fn has_only_attributes(element: &Element, allowed: &[&str]) -> bool {
    element.attrs().iter().all(|((namespace, name), _)| {
        namespace.is_empty() && allowed.iter().any(|allowed_name| *allowed_name == name)
    })
}

fn parse_exact_u32(value: &str) -> Option<u32> {
    let value = trim_xml_whitespace(value);
    let value = value.strip_prefix('+').unwrap_or(value);
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

fn parse_positive_u32(value: &str) -> Result<u32, InvalidSmInboundControl> {
    let value = parse_exact_u32(value).ok_or(InvalidSmInboundControl)?;
    (value != 0).then_some(value).ok_or(InvalidSmInboundControl)
}

fn parse_xsd_boolean(value: &str) -> Result<bool, InvalidSmInboundControl> {
    match trim_xml_whitespace(value) {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(InvalidSmInboundControl),
    }
}

/// Validate the optional `err:stanzaErrorGroup` accepted by XEP-0198
/// `<failed/>`.
///
/// XEP-0198's `<failed/>` payload is an optional RFC 6120
/// `err:stanzaErrorGroup`: either absent, or exactly one recognized stanza
/// condition. It does not admit application-defined children or `err:text`.
/// The latter is a separate extension in schemas that explicitly include it.
fn has_optional_stanza_error_group(element: &Element) -> bool {
    let mut nodes = element.nodes().filter(|node| !is_xml_whitespace_node(node));
    let Some(node) = nodes.next() else {
        return true;
    };

    let Some(condition) = node.as_element() else {
        return false;
    };
    if !is_stanza_error_condition(condition) {
        return false;
    }

    nodes.next().is_none()
}

fn is_stanza_error_condition(condition: &Element) -> bool {
    condition.ns() == NS_STANZA_ERRORS
        && condition.attrs().is_empty()
        && is_standard_stanza_error_condition(condition.name())
        && if is_text_stanza_error_condition(condition.name()) {
            condition.nodes().all(|node| node.as_text().is_some())
        } else {
            condition.nodes().next().is_none()
        }
}

fn trim_xml_whitespace(value: &str) -> &str {
    value.trim_matches(is_xml_whitespace)
}

fn is_xml_whitespace_node(node: &minidom::Node) -> bool {
    node.as_text()
        .is_some_and(|text| text.chars().all(is_xml_whitespace))
}

fn is_xml_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n' | '\r')
}

fn is_text_stanza_error_condition(name: &str) -> bool {
    matches!(name, "gone" | "redirect")
}

fn is_standard_stanza_error_condition(name: &str) -> bool {
    matches!(
        name,
        "bad-request"
            | "conflict"
            | "feature-not-implemented"
            | "forbidden"
            | "gone"
            | "internal-server-error"
            | "item-not-found"
            | "jid-malformed"
            | "not-acceptable"
            | "not-allowed"
            | "not-authorized"
            | "policy-violation"
            | "recipient-unavailable"
            | "redirect"
            | "registration-required"
            | "remote-server-not-found"
            | "remote-server-timeout"
            | "resource-constraint"
            | "service-unavailable"
            | "subscription-required"
            | "undefined-condition"
            | "unexpected-request"
    )
}

impl UnhandledOutboundEntry {
    fn element_for_fallback_retry(&self) -> Element {
        if !matches!(self.stanza.element.name(), "message" | "presence") {
            return self.stanza.element.clone();
        }

        let stamp = self.sent_at.to_rfc3339_opts(SecondsFormat::Millis, true);
        let mut element = self.stanza.element.clone();
        // XEP-0203 delay is a record of the original delivery time. A
        // persisted stanza can contain a stale, malformed, offset, or
        // duplicated delay, so always replace every delay child with one
        // builder-generated UTC value from the typed send timestamp.
        while element.remove_child("delay", NS_DELAY).is_some() {}
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
    use chrono::TimeZone;

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
    fn parse_inbound_control_accepts_exact_schema_shapes() {
        let el = Element::builder("enabled", NS_SM)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "abc123")
            .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
            .attr(minidom::rxml::xml_ncname!("max").to_owned(), "300")
            .build();
        assert_eq!(
            SmState::parse_inbound_control(&el),
            Ok(SmInboundControl::Enabled {
                previd: Some(StreamId::new("abc123")),
                max_resume_seconds: Some(300),
            })
        );
        assert_eq!(
            SmState::parse_inbound_control(&Element::builder("r", NS_SM).build()),
            Ok(SmInboundControl::RequestAck)
        );
        assert_eq!(
            SmState::parse_inbound_control(
                &Element::builder("a", NS_SM)
                    .attr(minidom::rxml::xml_ncname!("h").to_owned(), " +4294967295 ")
                    .build()
            ),
            Ok(SmInboundControl::Ack { h: u32::MAX })
        );
        assert_eq!(
            SmState::parse_inbound_control(
                &Element::builder("enabled", NS_SM)
                    .attr(minidom::rxml::xml_ncname!("resume").to_owned(), " true ")
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), "abc123")
                    .build()
            ),
            Ok(SmInboundControl::Enabled {
                previd: Some(StreamId::new("abc123")),
                max_resume_seconds: None,
            })
        );
    }

    #[test]
    fn parse_inbound_control_rejects_malformed_empty_controls_and_counters() {
        for element in [
            Element::builder("r", NS_SM)
                .attr(minidom::rxml::xml_ncname!("unexpected").to_owned(), "value")
                .build(),
            Element::builder("a", NS_SM).build(),
            Element::builder("a", NS_SM)
                .attr(minidom::rxml::xml_ncname!("h").to_owned(), "1 2")
                .build(),
            Element::builder("resumed", NS_SM)
                .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
                .attr(minidom::rxml::xml_ncname!("previd").to_owned(), "")
                .build(),
        ] {
            assert_eq!(
                SmState::parse_inbound_control(&element),
                Err(InvalidSmInboundControl)
            );
        }
        let mut whitespace_only_ack = Element::builder("a", NS_SM)
            .attr(minidom::rxml::xml_ncname!("h").to_owned(), "1")
            .build();
        whitespace_only_ack.append_text_node(" ");
        assert_eq!(
            SmState::parse_inbound_control(&whitespace_only_ack),
            Err(InvalidSmInboundControl)
        );
    }

    #[test]
    fn parse_inbound_control_requires_valid_enabled_boolean_and_resume_id() {
        for element in [
            Element::builder("enabled", NS_SM)
                .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "yes")
                .build(),
            Element::builder("enabled", NS_SM)
                .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
                .build(),
            Element::builder("enabled", NS_SM)
                .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "1")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "")
                .build(),
            Element::builder("enabled", NS_SM)
                .attr(minidom::rxml::xml_ncname!("max").to_owned(), "0")
                .build(),
            Element::builder("enabled", NS_SM)
                .attr(minidom::rxml::xml_ncname!("max").to_owned(), "4294967296")
                .build(),
        ] {
            assert_eq!(
                SmState::parse_inbound_control(&element),
                Err(InvalidSmInboundControl)
            );
        }
        for resume in ["false", "0"] {
            let element = Element::builder("enabled", NS_SM)
                .attr(minidom::rxml::xml_ncname!("resume").to_owned(), resume)
                .build();
            assert_eq!(
                SmState::parse_inbound_control(&element),
                Ok(SmInboundControl::Enabled {
                    previd: None,
                    max_resume_seconds: None,
                })
            );
        }
    }

    #[test]
    fn parse_inbound_control_accepts_xep0198_stanza_error_group() {
        let condition = Element::builder("item-not-found", NS_STANZA_ERRORS).build();
        let mut valid = Element::builder("failed", NS_SM).build();
        valid.append_text_node("\n  ");
        valid.append_child(condition);
        valid.append_text_node("\n");
        assert_eq!(
            SmState::parse_inbound_control(&valid),
            Ok(SmInboundControl::Failed { h: None })
        );

        let mut redirect = Element::builder("redirect", NS_STANZA_ERRORS).build();
        redirect.append_text_node("xmpp:replacement.example");
        assert_eq!(
            SmState::parse_inbound_control(
                &Element::builder("failed", NS_SM).append(redirect).build()
            ),
            Ok(SmInboundControl::Failed { h: None })
        );
        assert_eq!(
            SmState::parse_inbound_control(
                &Element::builder("failed", NS_SM)
                    .attr(minidom::rxml::xml_ncname!("h").to_owned(), " +1 ")
                    .build()
            ),
            Ok(SmInboundControl::Failed { h: Some(1) })
        );

        for invalid in [
            Element::builder("failed", NS_SM)
                .attr(minidom::rxml::xml_ncname!("h").to_owned(), "not-a-u32")
                .build(),
            Element::builder("failed", NS_SM)
                .attr(minidom::rxml::xml_ncname!("unexpected").to_owned(), "value")
                .build(),
            Element::builder("failed", NS_SM)
                .append(Element::builder("custom", NS_STANZA_ERRORS).build())
                .build(),
            Element::builder("failed", NS_SM)
                .append(Element::builder("item-not-found", NS_STANZA_ERRORS).build())
                .append(Element::builder("service-unavailable", NS_STANZA_ERRORS).build())
                .build(),
            Element::builder("failed", NS_SM)
                .append(Element::builder("item-not-found", NS_STANZA_ERRORS).build())
                .append(Element::builder("retry-after", "urn:waddle:diagnostics").build())
                .build(),
            Element::builder("failed", NS_SM)
                .append(
                    Element::builder("text", NS_STANZA_ERRORS)
                        .append("first")
                        .build(),
                )
                .append(Element::builder("item-not-found", NS_STANZA_ERRORS).build())
                .build(),
            Element::builder("failed", NS_SM)
                .append(Element::builder("item-not-found", NS_STANZA_ERRORS).build())
                .append(Element::builder("text", NS_STANZA_ERRORS).build())
                .build(),
            Element::builder("failed", NS_SM)
                .append(Element::builder("item-not-found", NS_STANZA_ERRORS).build())
                .append(Element::builder("retry-after", "urn:waddle:diagnostics").build())
                .append(
                    Element::builder("text", NS_STANZA_ERRORS)
                        .append("late")
                        .build(),
                )
                .build(),
            Element::builder("failed", NS_SM)
                .append(Element::builder("item-not-found", NS_STANZA_ERRORS).build())
                .append(
                    Element::builder("text", NS_STANZA_ERRORS)
                        .append("one")
                        .build(),
                )
                .append(
                    Element::builder("text", NS_STANZA_ERRORS)
                        .append("two")
                        .build(),
                )
                .build(),
            Element::builder("failed", NS_SM)
                .append(Element::builder("item-not-found", NS_STANZA_ERRORS).build())
                .append(Element::builder("retry-after", "urn:waddle:diagnostics").build())
                .append(Element::builder("retry-after", "urn:waddle:diagnostics").build())
                .build(),
            Element::builder("failed", NS_SM)
                .append(Element::builder("item-not-found", NS_STANZA_ERRORS).build())
                .append(Element::builder("application", "").build())
                .build(),
            Element::builder("failed", NS_SM)
                .append(Element::builder("not-an-error", "urn:waddle:diagnostics").build())
                .build(),
            Element::builder("failed", NS_SM)
                .append(Element::builder("item-not-found", "").build())
                .build(),
            Element::builder("failed", NS_SM)
                .append(
                    Element::builder("item-not-found", NS_STANZA_ERRORS)
                        .append(Element::builder("detail", "urn:waddle:diagnostics").build())
                        .build(),
                )
                .build(),
            Element::builder("failed", NS_SM)
                .append(Element::builder("item-not-found", NS_STANZA_ERRORS).build())
                .append(
                    Element::builder("text", NS_STANZA_ERRORS)
                        .attr(minidom::rxml::xml_ncname!("lang").to_owned(), "en")
                        .append("not xml:lang")
                        .build(),
                )
                .build(),
        ] {
            assert_eq!(
                SmState::parse_inbound_control(&invalid),
                Err(InvalidSmInboundControl)
            );
        }
    }

    #[test]
    fn no_progress_ack_rearms_the_existing_unhandled_tail() {
        let mut state = SmState::new();
        state.outbound_enabled = true;
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let message = Element::builder("message", "jabber:client")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "one")
            .build();

        assert!(state.record_sent_stanza_at(&message, now));
        assert!(!state.record_sent_stanza(&message));
        assert!(state
            .process_ack_at(0, now + Duration::milliseconds(1))
            .is_empty());

        assert!(state
            .poll_acknowledgement_clock(now + Duration::milliseconds(250))
            .is_empty());
        assert_eq!(
            state.poll_acknowledgement_clock(now + Duration::milliseconds(251)),
            vec![SmAcknowledgementClockAction::Retry { attempt: 1 }]
        );
    }

    #[test]
    fn acknowledgement_clock_stays_pending_for_no_progress_ack_only_until_terminal_reset() {
        let mut state = SmState::new();
        state.outbound_enabled = true;
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let message = Element::builder("message", "jabber:client").build();

        assert!(!state.acknowledgement_clock_pending());
        assert!(state.record_sent_stanza_at(&message, now));
        assert!(state.acknowledgement_clock_pending());

        // A valid no-progress `<a/>` restarts the `<r/>` retry cadence, but
        // the original h-progress deadline must continue to run.
        assert!(state
            .process_ack_at(0, now + Duration::milliseconds(1))
            .is_empty());
        assert!(state.acknowledgement_clock_pending());

        state.stop();
        assert!(!state.acknowledgement_clock_pending());
    }

    #[test]
    fn acknowledgement_clock_retries_then_times_out_without_an_ack() {
        let mut state = SmState::new();
        state.outbound_enabled = true;
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let message = Element::builder("message", "jabber:client").build();
        assert!(state.record_sent_stanza_at(&message, now));

        assert!(state
            .poll_acknowledgement_clock(now + Duration::milliseconds(249))
            .is_empty());
        assert_eq!(
            state.poll_acknowledgement_clock(now + Duration::milliseconds(250)),
            vec![SmAcknowledgementClockAction::Retry { attempt: 1 }]
        );
        assert_eq!(
            state.poll_acknowledgement_clock(now + Duration::milliseconds(500)),
            vec![SmAcknowledgementClockAction::Retry { attempt: 2 }]
        );
        assert_eq!(
            state.poll_acknowledgement_clock(now + Duration::milliseconds(1_000)),
            vec![SmAcknowledgementClockAction::Retry { attempt: 3 }]
        );
        assert_eq!(
            state.poll_acknowledgement_clock(now + Duration::milliseconds(2_000)),
            vec![SmAcknowledgementClockAction::Retry { attempt: 4 }]
        );
        assert_eq!(
            state.poll_acknowledgement_clock(now + Duration::milliseconds(5_000)),
            vec![
                SmAcknowledgementClockAction::Retry { attempt: 5 },
                SmAcknowledgementClockAction::RequestTimedOut,
            ]
        );
        assert_eq!(
            state.poll_acknowledgement_clock(now + Duration::milliseconds(10_000)),
            vec![SmAcknowledgementClockAction::Retry { attempt: 6 }]
        );
    }

    #[test]
    fn repeated_no_progress_acks_restart_retry_cadence_without_moving_deadline() {
        let mut state = SmState::new();
        state.outbound_enabled = true;
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let message = Element::builder("message", "jabber:client").build();
        assert!(state.record_sent_stanza_at(&message, now));
        assert!(state
            .process_ack_at(0, now + Duration::milliseconds(100))
            .is_empty());
        assert!(state
            .poll_acknowledgement_clock(now + Duration::milliseconds(349))
            .is_empty());
        assert_eq!(
            state.poll_acknowledgement_clock(now + Duration::milliseconds(350)),
            vec![SmAcknowledgementClockAction::Retry { attempt: 1 }]
        );

        assert!(state
            .process_ack_at(0, now + Duration::milliseconds(400))
            .is_empty());
        assert!(state
            .poll_acknowledgement_clock(now + Duration::milliseconds(649))
            .is_empty());
        assert_eq!(
            state.poll_acknowledgement_clock(now + Duration::milliseconds(650)),
            vec![SmAcknowledgementClockAction::Retry { attempt: 1 }]
        );
        assert!(!state
            .poll_acknowledgement_clock(now + Duration::milliseconds(29_999))
            .contains(&SmAcknowledgementClockAction::ProgressTimedOut));
        assert!(state
            .poll_acknowledgement_clock(now + Duration::milliseconds(30_000))
            .contains(&SmAcknowledgementClockAction::ProgressTimedOut));
    }

    #[test]
    fn full_ack_cancels_idle_progress_deadline_and_next_stanza_rearms_it() {
        let mut state = SmState::new();
        state.outbound_enabled = true;
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let message = Element::builder("message", "jabber:client").build();

        assert!(state.record_sent_stanza_at(&message, now));
        assert!(state
            .process_ack_at(1, now + Duration::seconds(1))
            .is_empty());
        assert!(state.outbound_queue.is_empty());
        assert!(state
            .poll_acknowledgement_clock(now + Duration::seconds(31))
            .is_empty());

        let next = now + Duration::seconds(32);
        assert!(state.record_sent_stanza_at(&message, next));
        assert_eq!(
            state.poll_acknowledgement_clock(next + Duration::milliseconds(250)),
            vec![SmAcknowledgementClockAction::Retry { attempt: 1 }]
        );
        assert!(state
            .poll_acknowledgement_clock(next + Duration::seconds(30))
            .contains(&SmAcknowledgementClockAction::ProgressTimedOut));
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
        state.previd = Some(StreamId::new("previous-stream"));
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
        state.previd = Some(StreamId::new("previous-stream"));
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
        state.previd = Some(StreamId::new("timed-out-stream"));
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
        state.previd = Some(StreamId::new("previous-stream"));
        state.max_resume_seconds = Some(300);

        let resume_state = state.resume_state().expect("resume state");
        assert_eq!(resume_state.max_resume_seconds(), Some(300));

        let restored = SmState::from_resume_state(&resume_state);
        assert_eq!(restored.max_resume_seconds, Some(300));
    }

    #[test]
    fn resume_state_round_trips_timestamped_unhandled_outbound_entries() {
        let stanza = Element::builder("message", "jabber:client")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "unacked")
            .build();
        let sent_at = "2026-07-26T12:34:56.789Z"
            .parse::<DateTime<Utc>>()
            .expect("timestamp");

        let resume_state = SmResumeState::from_unhandled_outbound_entries(
            StreamId::new("previous-stream"),
            4,
            9,
            [UnhandledOutboundEntry::try_new(stanza.clone(), sent_at).expect("countable stanza")],
        )
        .expect("resume state");

        assert!(resume_state.has_unhandled_outbound_stanzas());
        assert_eq!(
            resume_state
                .unhandled_outbound_entries()
                .map(UnhandledOutboundEntry::stanza_for_persistence)
                .collect::<Vec<_>>(),
            vec![&stanza],
        );
        assert_eq!(
            resume_state
                .unhandled_outbound_entries()
                .map(UnhandledOutboundEntry::sent_at)
                .collect::<Vec<_>>(),
            vec![sent_at],
        );
    }

    #[test]
    fn resumed_unacked_tail_clock_keeps_counters_and_uses_normal_deadlines() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let message = Element::builder("message", "jabber:client").build();
        let resume = SmResumeState::from_unhandled_outbound_entries(
            StreamId::new("previous-stream"),
            3,
            9,
            [UnhandledOutboundEntry::try_new(message, now).expect("countable stanza")],
        )
        .unwrap();
        let mut state = SmState::from_resume_state(&resume);

        assert!(state.arm_acknowledgement_clock(now));
        assert_eq!(state.outbound_count, 9);
        assert_eq!(
            state.poll_acknowledgement_clock(now + Duration::milliseconds(250)),
            vec![SmAcknowledgementClockAction::Retry { attempt: 1 }]
        );
        assert!(state
            .poll_acknowledgement_clock(now + Duration::seconds(5))
            .contains(&SmAcknowledgementClockAction::RequestTimedOut));
        assert!(state
            .poll_acknowledgement_clock(now + Duration::seconds(30))
            .contains(&SmAcknowledgementClockAction::ProgressTimedOut));
    }

    #[test]
    fn fallback_retry_normalizes_persisted_delay_to_one_utc_stamp() {
        let sent_at = Utc.with_ymd_and_hms(2025, 2, 3, 4, 5, 6).unwrap();
        let cases = [
            (
                vec![Some("2024-01-15T10:00:00Z")],
                "2024-01-15T10:00:00.000Z",
            ),
            (
                vec![Some("2024-01-15T10:00:00+01:00")],
                "2024-01-15T09:00:00.000Z",
            ),
            (vec![], "2025-02-03T04:05:06.000Z"),
            (vec![Some("not-a-timestamp")], "2025-02-03T04:05:06.000Z"),
            (
                vec![Some("2024-01-15T10:00:00Z"), None],
                "2024-01-15T10:00:00.000Z",
            ),
        ];

        for (delay_stamps, expected_stamp) in cases {
            let mut message = Element::builder("message", "jabber:client")
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), "persisted")
                .build();
            for stamp in delay_stamps {
                let mut delay = Element::builder("delay", NS_DELAY);
                if let Some(stamp) = stamp {
                    delay = delay.attr(minidom::rxml::xml_ncname!("stamp").to_owned(), stamp);
                }
                message.append_child(delay.build());
            }

            let mut state = SmState::new();
            state.outbound_enabled = true;
            state.record_sent_stanza_at(&message, sent_at);
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
            assert_eq!(delays[0].attr("stamp"), Some(expected_stamp));
        }
    }
}
