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
    pub presence_available: bool,
    pub presence_show: Option<xmpp_parsers::presence::Show>,
    pub presence_status: Option<String>,
    pub presence_priority: i8,
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
    pub fn record_outbound(&mut self, stanza_xml: String) {
        self.record_outbound_with_receipt_at(stanza_xml, chrono::Utc::now());
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
    ) {
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
            presence_available: snapshot.presence_available,
            presence_show: snapshot.presence_show,
            presence_status: snapshot.presence_status,
            presence_priority: snapshot.presence_priority,
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

        // The retained queue no longer contains the evicted stanza; the
        // replay-gap marker added below is what prevents a successful resume
        // for clients that still need it.
        let resend = state.get_stanzas_to_resend(0);
        assert_eq!(resend.len(), 3, "evicted seq=1 must be absent from replay");
        assert!(!resend.iter().any(|xml| xml.contains("id='1'")));
    }

    #[test]
    fn test_evicted_unacked_stanza_blocks_resume_until_client_h_covers_gap() {
        let mut state = StreamManagementState::with_config(3, 5);
        state.enable("tiny-cap".to_string(), true, Some(300));

        state.record_outbound("<message id='1'/>".to_string());
        state.record_outbound("<message id='2'/>".to_string());
        state.record_outbound("<message id='3'/>".to_string());
        state.record_outbound("<message id='4'/>".to_string());

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
                presence_available: true,
                presence_show: Some(xmpp_parsers::presence::Show::Chat),
                presence_status: Some("ready".to_string()),
                presence_priority: 7,
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

        let detached_on = state
            .to_detached_session(DetachedSessionSnapshot {
                user_id: "user@example.com".to_string(),
                jid,
                carbons_enabled: true,
                roster_interested: false,
                presence_available: false,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
            })
            .expect("resumable state must produce detached session");
        assert!(
            detached_on.carbons_enabled,
            "carbons_enabled=true must round-trip so resume preserves opt-in"
        );
    }
}
