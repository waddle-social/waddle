use std::time::Instant;

use tracing::warn;

use crate::prometheus;

use super::unacked_queue::sequence_gt;
use super::{
    DetachedSession, UnackedPushResult, UnackedQueue, DEFAULT_ACK_REQUEST_THRESHOLD,
    DEFAULT_MAX_UNACKED_QUEUE_SIZE,
};

/// Per-stream state that must survive XEP-0198 detachment.
#[derive(Debug, Clone, PartialEq)]
pub struct DetachedSessionSnapshot {
    pub user_id: String,
    pub jid: jid::FullJid,
    pub carbons_enabled: bool,
    pub roster_interested: bool,
    pub blocklist_interested: bool,
    pub presence_available: bool,
    pub presence_show: Option<xmpp_parsers::presence::Show>,
    pub presence_status: Option<String>,
    pub presence_priority: i8,
    /// Presence extension payloads (XEP-0115 caps, XEP-0319 idle, ...)
    /// last broadcast by the resource, carried into the detached session
    /// so probe/subscription delivery can relay them verbatim while the
    /// stream awaits resume (issue #1103).
    pub presence_payloads: Vec<minidom::Element>,
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
    /// `outbound_count` at the moment we most recently emitted an
    /// `<r/>` ack request. Drives the per-N cadence in
    /// [`Self::record_outbound_with_receipt_at`]: the next request
    /// fires once `outbound_count - last_request_outbound_count >=
    /// ack_threshold`. Without this gate the SM unacked queue grows
    /// monotonically, hits the 1000-cap, starts evicting the oldest
    /// stanzas, and every future resume from the stream is rejected
    /// because `replay_gap_through` permanently outruns the client's
    /// last-acked `h`.
    last_request_outbound_count: u32,
    /// Highest evicted outbound sequence not yet covered by a client ack.
    replay_gap_through: Option<u32>,
    /// Maximum resumption timeout in seconds
    pub max_resume_time: Option<u32>,
    /// Queue of unacknowledged outbound stanzas
    unacked_queue: UnackedQueue,
    /// Ack request threshold (request ack after this many unacked stanzas)
    ack_threshold: u32,
    /// When this SM state was created
    created_at: Instant,
}

/// Side-channel signal returned from [`StreamManagementState::record_outbound`]
/// telling the caller whether to follow the just-written stanza with an
/// `<r/>` to elicit an SM `<a h='N'/>` ack from the client.
///
/// `#[must_use]` so a refactor that drops the return value gets caught at
/// compile time — silently swallowing the request would re-introduce the
/// monotonically-growing unacked-queue bug.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[must_use = "the request_ack signal drives the SM <r/> cadence; ignoring it \
              lets the unacked queue grow unbounded"]
pub struct RecordOutboundResult {
    /// `true` once the count of stanzas since the last `<r/>` reached
    /// `ack_threshold`. Callers MUST emit an `<r/>` on the same
    /// wire after the stanza so the client knows to send `<a/>`.
    pub request_ack: bool,
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
            last_request_outbound_count: 0,
            replay_gap_through: None,
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
            last_request_outbound_count: 0,
            replay_gap_through: None,
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
    /// means a subsequent `<resume/>` from a client whose `h` is older
    /// than the retained window must fail instead of returning a
    /// misleading `<resumed/>` with missing stanzas.
    pub fn record_outbound(&mut self, stanza_xml: String) -> RecordOutboundResult {
        self.record_outbound_with_receipt_at(stanza_xml, chrono::Utc::now())
    }

    /// Record an outbound stanza with an explicit `original_receipt_at`.
    ///
    /// Used by the `pending_delivery` flush path so the SM unacked
    /// queue preserves the row's original receipt time. Without this,
    /// a flush replay → client disconnect pre-ack → SM expiry sequence
    /// would have Q6 promotion re-create the `pending_delivery` row
    /// with `original_receipt_at = flush time` (wrong), making the
    /// eventual XEP-0203 `<delay/>` stamp the flush time instead of
    /// the original failed-delivery time.
    /// (Greptile/Copilot/Qodo P1 review on PR #361.)
    pub fn record_outbound_with_receipt_at(
        &mut self,
        stanza_xml: String,
        original_receipt_at: chrono::DateTime<chrono::Utc>,
    ) -> RecordOutboundResult {
        self.outbound_count = self.outbound_count.wrapping_add(1);
        match self.unacked_queue.push_with_receipt_at(
            self.outbound_count,
            stanza_xml,
            original_receipt_at,
        ) {
            UnackedPushResult::Accepted => {}
            UnackedPushResult::Evicted(evicted) => {
                self.mark_replay_gap_through(evicted.sequence);
                prometheus::increment_sm_unacked_evicted();
                warn!(
                    stream_id = self.stream_id.as_deref().unwrap_or("<unset>"),
                    evicted_sequence = evicted.sequence,
                    queue_len = self.unacked_queue.len(),
                    "SM unacked queue full; evicted oldest stanza — older resume h values will be rejected"
                );
            }
        }
        self.ack_request_cadence()
    }

    /// Cadence: once `ack_threshold` stanzas have flowed since the
    /// last `<r/>` (or the stream was enabled / resumed), tell the
    /// caller to follow this stanza with an `<r/>`. The wasm
    /// client only sends `<a h='N'/>` in response to an explicit
    /// request, so without this signal the unacked queue grows
    /// unbounded and eventually starts evicting — breaking SM
    /// resume forever for that stream.
    fn ack_request_cadence(&mut self) -> RecordOutboundResult {
        let request_ack = self.enabled
            && self
                .outbound_count
                .wrapping_sub(self.last_request_outbound_count)
                >= self.ack_threshold;
        if request_ack {
            self.last_request_outbound_count = self.outbound_count;
        }
        RecordOutboundResult { request_ack }
    }

    /// Update the last acknowledged count from a client ack.
    ///
    /// This also removes acknowledged stanzas from the queue.
    pub fn acknowledge(&mut self, h: u32) {
        self.last_acked = h;
        self.unacked_queue.acknowledge(h);
        if self
            .replay_gap_through
            .is_some_and(|gap| !sequence_gt(gap, h))
        {
            self.replay_gap_through = None;
        }
    }

    /// Whether a client `<a h='N'/>` claims more stanzas handled than
    /// this stream has sent (XEP-0198 §4 handled-count-too-high).
    ///
    /// Judged mod 2^32: counters wrap, so "greater than" uses the same
    /// wrap-aware ordering as the unacked queue — an ack a few stanzas
    /// behind a freshly-wrapped `outbound_count` is valid.
    pub fn ack_exceeds_outbound(&self, h: u32) -> bool {
        sequence_gt(h, self.outbound_count)
    }

    /// Get the current inbound count for sending in an <a/> response.
    pub fn get_inbound_count(&self) -> u32 {
        self.inbound_count
    }

    /// Get the number of unacknowledged outbound stanzas.
    pub fn unacked_count(&self) -> u32 {
        self.outbound_count.wrapping_sub(self.last_acked)
    }

    /// Get stanzas that need to be resent after resumption.
    ///
    /// `client_h` is the last sequence number the client acknowledged receiving.
    /// Returns stanzas with sequence > client_h, each paired with its
    /// original receipt time for the XEP-0203 replay stamp.
    pub fn get_stanzas_to_resend(&self, client_h: u32) -> Vec<super::ReplayStanza> {
        self.unacked_queue.get_unacked_after(client_h)
    }

    /// Highest evicted outbound sequence that the client has not acked yet.
    pub fn replay_gap_through(&self) -> Option<u32> {
        self.replay_gap_through
    }

    /// Whether the retained queue can satisfy XEP-0198 replay for `client_h`.
    pub fn can_resume_from(&self, client_h: u32) -> bool {
        self.replay_gap_through
            .is_none_or(|gap| !sequence_gt(gap, client_h))
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

    fn mark_replay_gap_through(&mut self, sequence: u32) {
        if self
            .replay_gap_through
            .is_none_or(|current| sequence_gt(sequence, current))
        {
            self.replay_gap_through = Some(sequence);
        }
    }

    /// Create a detached session for storage in the registry.
    ///
    /// `carbons_enabled` is the actor's current XEP-0280 opt-in value.
    /// Carbons opt-in is per-stream, so XEP-0198 resumption must preserve
    /// it — storing it on the detached session is what makes that possible.
    pub fn to_detached_session(
        &self,
        snapshot: DetachedSessionSnapshot,
    ) -> Option<DetachedSession> {
        if !self.is_resumable() {
            return None;
        }

        Some(DetachedSession {
            stream_id: self.stream_id.clone()?,
            user_id: snapshot.user_id,
            jid: snapshot.jid,
            inbound_count: self.inbound_count,
            outbound_count: self.outbound_count,
            last_acked: self.last_acked,
            replay_gap_through: self.replay_gap_through,
            unacked_stanzas: self.unacked_queue.get_all_unacked(),
            max_resume_time: self.max_resume_time,
            detached_at: Instant::now(),
            carbons_enabled: snapshot.carbons_enabled,
            roster_interested: snapshot.roster_interested,
            blocklist_interested: snapshot.blocklist_interested,
            presence_available: snapshot.presence_available,
            presence_show: snapshot.presence_show,
            presence_status: snapshot.presence_status,
            presence_priority: snapshot.presence_priority,
            presence_payloads: snapshot.presence_payloads,
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
        // Resume just replayed every still-unacked stanza on the new
        // wire. The next `<r/>` cadence window starts fresh from the
        // current outbound_count so the very next post-resume stanza
        // doesn't immediately re-request — give it ack_threshold of
        // headroom like a freshly-enabled session would have.
        self.last_request_outbound_count = session.outbound_count;
        self.replay_gap_through = session.replay_gap_through;
        self.max_resume_time = session.max_resume_time;

        // Restore unacked queue
        self.unacked_queue.restore(&session.unacked_stanzas);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let _ = state.record_outbound("<message id='1'/>".to_string());
        let _ = state.record_outbound("<message id='2'/>".to_string());
        let _ = state.record_outbound("<message id='3'/>".to_string());

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

        let _metrics_guard = prometheus::metrics_test_lock().blocking_lock();

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

        let _ = state.record_outbound("<message id='1'/>".to_string());
        let _ = state.record_outbound("<message id='2'/>".to_string());
        let _ = state.record_outbound("<message id='3'/>".to_string());
        assert_eq!(state.queue_len(), 3);

        // 4th push evicts seq=1
        let _ = state.record_outbound("<message id='4'/>".to_string());
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

        // The retained queue no longer contains the evicted stanza; the
        // replay-gap marker added below is what prevents a successful resume
        // for clients that still need it.
        let resend = state.get_stanzas_to_resend(0);
        assert_eq!(resend.len(), 3, "evicted seq=1 must be absent from replay");
        assert!(!resend
            .iter()
            .any(|replay| replay.stanza_xml.contains("id='1'")));
    }

    #[test]
    fn test_evicted_unacked_stanza_blocks_resume_until_client_h_covers_gap() {
        let _metrics_guard = crate::prometheus::metrics_test_lock().blocking_lock();

        let mut state = StreamManagementState::with_config(3, 5);
        state.enable("tiny-cap".to_string(), true, Some(300));

        let _ = state.record_outbound("<message id='1'/>".to_string());
        let _ = state.record_outbound("<message id='2'/>".to_string());
        let _ = state.record_outbound("<message id='3'/>".to_string());
        let _ = state.record_outbound("<message id='4'/>".to_string());

        assert_eq!(state.replay_gap_through(), Some(1));
        assert!(
            !state.can_resume_from(0),
            "client h=0 still needs the evicted sequence 1, so resume would be incomplete"
        );
        assert!(
            state.can_resume_from(1),
            "client h=1 has handled the evicted sequence, so the retained replay window is complete"
        );

        state.acknowledge(1);
        assert_eq!(
            state.replay_gap_through(),
            None,
            "acknowledging through the evicted sequence closes the replay gap"
        );
        assert!(state.can_resume_from(1));
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
            .to_detached_session(DetachedSessionSnapshot {
                user_id: "user@example.com".to_string(),
                jid: jid.clone(),
                carbons_enabled: false,
                roster_interested: true,
                blocklist_interested: true,
                presence_available: true,
                presence_show: Some(xmpp_parsers::presence::Show::Chat),
                presence_status: Some("ready".to_string()),
                presence_priority: 7,
                presence_payloads: Vec::new(),
            })
            .expect("resumable state must produce detached session");
        assert!(
            !detached_off.carbons_enabled,
            "carbons_enabled=false must round-trip through DetachedSession"
        );
        assert!(
            detached_off.roster_interested,
            "roster_interested=true must round-trip through DetachedSession"
        );
        assert!(
            detached_off.blocklist_interested,
            "blocklist_interested=true must round-trip through DetachedSession"
        );

        let detached_on = state
            .to_detached_session(DetachedSessionSnapshot {
                user_id: "user@example.com".to_string(),
                jid,
                carbons_enabled: true,
                roster_interested: false,
                blocklist_interested: false,
                presence_available: false,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
                presence_payloads: Vec::new(),
            })
            .expect("resumable state must produce detached session");
        assert!(
            detached_on.carbons_enabled,
            "carbons_enabled=true must round-trip so resume preserves opt-in"
        );
    }

    /// XEP-0198 §4 servers SHOULD request `<a/>` periodically; without
    /// it the wasm client (which only acks in response to `<r/>`,
    /// `runtime/sm.rs`) lets the unacked queue grow unbounded until
    /// it hits the 1000-cap and starts evicting — which permanently
    /// breaks every future resume from the stream. The cadence below
    /// guarantees `<r/>` is requested on a tight schedule.
    #[test]
    fn record_outbound_requests_ack_at_threshold_and_only_once_per_window() {
        // ack_threshold=3 keeps the assertions short while still
        // proving the "request once per N, not on every push" rule.
        let mut state = StreamManagementState::with_config(1000, 3);
        state.enable("cadence".to_string(), true, Some(300));

        // Push 1, 2 — below threshold, no request yet.
        assert!(!state.record_outbound("<m id='1'/>".to_string()).request_ack);
        assert!(!state.record_outbound("<m id='2'/>".to_string()).request_ack);
        // Push 3 — threshold met, request fires.
        assert!(state.record_outbound("<m id='3'/>".to_string()).request_ack);
        // Push 4, 5 — request_ack must NOT keep firing on every
        // subsequent stanza; that would spam the client with one
        // `<r/>` per stanza.
        assert!(!state.record_outbound("<m id='4'/>".to_string()).request_ack);
        assert!(!state.record_outbound("<m id='5'/>".to_string()).request_ack);
        // Push 6 — three more since the last request, next request fires.
        assert!(state.record_outbound("<m id='6'/>".to_string()).request_ack);
    }

    #[test]
    fn record_outbound_does_not_request_ack_when_sm_disabled() {
        // Without SM enabled there's no `<r/>`/`<a/>` cycle at all —
        // the cadence signal must stay silent regardless of how many
        // outbound stanzas accumulate, otherwise the websocket layer
        // would write `<r/>` onto a stream the client isn't tracking.
        let mut state = StreamManagementState::with_config(1000, 1);
        // NOTE: `enable` deliberately NOT called.

        let result = state.record_outbound("<m id='1'/>".to_string());
        assert!(!result.request_ack);
    }

    #[test]
    fn restore_from_session_resets_request_window_so_no_immediate_re_request() {
        // After XEP-0198 resume the server has just replayed every
        // un-acked stanza on the new wire. The next post-resume
        // `<r/>` must wait for `ack_threshold` *new* stanzas — if it
        // fired on stanza #1 we'd be re-requesting against the same
        // pre-resume backlog the resume itself already covered.
        let detached = DetachedSession {
            stream_id: "previd".to_string(),
            user_id: "u@h".to_string(),
            jid: "u@h/r".parse().unwrap(),
            inbound_count: 10,
            outbound_count: 42,
            last_acked: 40,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
        };
        let mut state = StreamManagementState::with_config(1000, 3);
        state.restore_from_session(&detached);

        // Push 1, 2 — below the post-resume threshold.
        assert!(
            !state
                .record_outbound("<m id='post-1'/>".to_string())
                .request_ack
        );
        assert!(
            !state
                .record_outbound("<m id='post-2'/>".to_string())
                .request_ack
        );
        // Push 3 — threshold met against the *post-resume* baseline.
        assert!(
            state
                .record_outbound("<m id='post-3'/>".to_string())
                .request_ack
        );
    }
}
