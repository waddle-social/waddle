use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use jid::FullJid;
use xmpp_parsers::presence::Show;

use super::super::{sequence::sequence_gt, ShadowOrdinal};
use super::DEFAULT_SESSION_TIMEOUT_SECS;

/// One unacknowledged stanza retained on a detached SM session.
///
/// Carries the XEP-0198 outbound sequence + the serialized stanza
/// XML as the queue did before, plus the **server-side receipt time**
/// of the original stanza (NOT the detach time). The Q6 SM-expiry
/// promotion path consumes `original_receipt_at` when it stamps the
/// XEP-0203 `<delay/>` on a flushed offline replay so the recipient
/// sees the failed delivery's true timestamp per XEP-0203 §4.1 +
/// XEP-0198 §5 line 364.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachedUnackedStanza {
    /// XEP-0198 outbound sequence number assigned to this stanza.
    pub sequence: u32,
    /// Serialized stanza XML (re-parsed on demand by the promotion
    /// path; kept as `String` here so the queue doesn't pin the
    /// `xmpp_parsers::Element` representation in memory across
    /// detach windows).
    pub stanza_xml: String,
    /// Server-side receipt time of the original stanza. Used by the
    /// Q6 SM-expiry promotion path for the XEP-0203 `<delay/>` stamp.
    pub original_receipt_at: DateTime<Utc>,
}

/// A detached stream management session.
///
/// Contains all the state needed to resume a stream after disconnection.
#[derive(Debug, Clone)]
pub struct DetachedSession {
    /// The unique stream ID
    pub stream_id: String,
    /// Authenticated user identifier.
    pub user_id: String,
    /// The full JID of the session owner
    pub jid: FullJid,
    /// Server's inbound stanza count at detach time
    pub inbound_count: u32,
    /// Durable non-wrapping shadow ordinal frontier at detach time.
    pub shadow_ordinal: ShadowOrdinal,
    /// Server's outbound stanza count at detach time
    pub outbound_count: u32,
    /// Last acknowledged outbound stanza count
    pub last_acked: u32,
    /// Highest evicted outbound sequence not yet covered by a client ack.
    ///
    /// When set, XEP-0198 resumption can only succeed for a client `h`
    /// that is at or beyond this sequence; older `h` values still need a
    /// stanza this bounded replay queue no longer retains.
    pub replay_gap_through: Option<u32>,
    /// Unacknowledged stanzas (sequence + xml + receipt time).
    /// See [`DetachedUnackedStanza`] for field semantics.
    pub unacked_stanzas: Vec<DetachedUnackedStanza>,
    /// Maximum resumption time in seconds
    pub max_resume_time: Option<u32>,
    /// When the session was detached
    pub detached_at: Instant,
    /// XEP-0280 Message Carbons opt-in at detach time.
    ///
    /// XEP-0198 §5 defines `<resumed/>` as continuing the same stream, so any
    /// per-stream add-ons the client previously enabled (here: carbons) must
    /// survive resumption without requiring the client to re-negotiate them.
    pub carbons_enabled: bool,
    /// RFC 6121 roster-interest state at detach time.
    ///
    /// XEP-0198 resumption continues the same stream, so an already
    /// interested resource remains interested after a successful resume.
    pub roster_interested: bool,
    /// XEP-0191 blocklist-interest state at detach time.
    ///
    /// XEP-0198 resumption continues the same stream, so a resource that
    /// requested the blocklist remains eligible for block/unblock pushes while
    /// detached and after resume.
    pub blocklist_interested: bool,
    /// Whether the resource had sent available presence at detach time.
    ///
    /// Presence side effects required by RFC 6121 still apply to detached
    /// XEP-0198 streams that were available when the transport dropped.
    pub presence_available: bool,
    /// Last advertised show value while available.
    pub presence_show: Option<Show>,
    /// Last advertised status text while available.
    pub presence_status: Option<String>,
    /// Last advertised priority while available.
    pub presence_priority: i8,
    /// The resource's presence extension payloads (XEP-0115 caps,
    /// XEP-0319 idle, anything else) as last broadcast while available,
    /// relayed verbatim by probe/subscription delivery for detached
    /// resources (issue #1103). The durable persisted shape
    /// ([`PersistedSession`](super::super::persistence::PersistedSession))
    /// now carries these too (issue #1206), so a session rehydrated after
    /// a process restart or cross-node resume reports its full presence,
    /// payloads included — not bare show/status/priority.
    pub presence_payloads: Vec<minidom::Element>,
    /// Whether the once-per-session pending-subscribe flush (RFC 6121
    /// §3.1.3, issue #1104) was already consumed before detach. Resume
    /// restores this onto the fresh connection so the claim is only
    /// re-consumed when the detached session had actually consumed it —
    /// NOT inferred from `presence_available`, which goes stale when
    /// the session flips unavailable after its initial available.
    pub pending_subscribes_flushed: bool,
}

impl DetachedSession {
    /// Check if the session has expired.
    pub fn is_expired(&self) -> bool {
        let max_time = self
            .max_resume_time
            .unwrap_or(DEFAULT_SESSION_TIMEOUT_SECS as u32);
        self.detached_at.elapsed() > Duration::from_secs(max_time as u64)
    }

    /// Get remaining time until expiration.
    pub fn remaining_time(&self) -> Duration {
        let max_time = Duration::from_secs(
            self.max_resume_time
                .unwrap_or(DEFAULT_SESSION_TIMEOUT_SECS as u32) as u64,
        );
        max_time.saturating_sub(self.detached_at.elapsed())
    }

    /// Get the number of stanzas that would need to be resent.
    ///
    /// `client_h` is what the client reports as last received.
    pub fn stanzas_to_resend_count(&self, client_h: u32) -> usize {
        self.unacked_stanzas
            .iter()
            .filter(|entry| sequence_gt(entry.sequence, client_h))
            .count()
    }

    /// Get the XML payloads that must be resent to a client reporting `h`.
    pub fn stanzas_to_resend(&self, client_h: u32) -> Vec<String> {
        self.unacked_stanzas
            .iter()
            .filter(|entry| sequence_gt(entry.sequence, client_h))
            .map(|entry| entry.stanza_xml.clone())
            .collect()
    }

    /// Whether a resume `h` claims more stanzas handled than this
    /// stream ever sent (XEP-0198 §4 handled-count-too-high). Judged
    /// as an EXACT mod-2^32 window measured from `last_acked`,
    /// matching the live ack path's
    /// `StreamManagementState::ack_exceeds_outbound`: a valid `h` sits
    /// inside `[last_acked, outbound_count]`, so an `h` a few stanzas
    /// behind a freshly wrapped `outbound_count` is valid while the
    /// half-space comparison's blind spot at exactly distance 2^31
    /// (`h == outbound_count + 0x8000_0000`) is rejected. The
    /// regressed half-space also measures "outside the window";
    /// resume callers run [`Self::can_resume_from`] FIRST so a
    /// mod-behind `h` is a failed resume, not a stream error.
    pub fn handled_count_exceeds_outbound(&self, client_h: u32) -> bool {
        client_h.wrapping_sub(self.last_acked) > self.outbound_count.wrapping_sub(self.last_acked)
    }

    /// Whether this detached session can satisfy XEP-0198 replay for `client_h`.
    ///
    /// Two lower bounds, both mod 2^32: `client_h` must not sit below a
    /// queue-eviction gap, and it must not regress behind `last_acked` —
    /// stanzas at or below the acked watermark were purged from the
    /// replay queue when the ack landed, so a resume claiming less than
    /// the client already confirmed cannot be replayed (and its `h`
    /// would numerically range-delete every pending row; round-2
    /// concurrency review on #1099).
    pub fn can_resume_from(&self, client_h: u32) -> bool {
        if sequence_gt(self.last_acked, client_h) {
            return false;
        }
        self.replay_gap_through
            .is_none_or(|gap| !sequence_gt(gap, client_h))
    }

    /// Record an outbound stanza while this stream is detached.
    /// `original_receipt_at` is the server-side receipt time of the
    /// stanza (NOT the detach time) — consumed by the Q6 SM-expiry
    /// promotion path for the XEP-0203 `<delay/>` stamp.
    pub fn record_detached_outbound(
        &mut self,
        stanza_xml: String,
        original_receipt_at: DateTime<Utc>,
    ) {
        self.outbound_count = self.outbound_count.wrapping_add(1);
        if self.unacked_stanzas.len() >= crate::stream_management::DEFAULT_MAX_UNACKED_QUEUE_SIZE {
            let evicted = self.unacked_stanzas.remove(0);
            self.note_detached_eviction(evicted.sequence);
            self.mark_replay_gap_through(evicted.sequence);
        }
        self.unacked_stanzas.push(DetachedUnackedStanza {
            sequence: self.outbound_count,
            stanza_xml,
            original_receipt_at,
        });
    }

    /// Record an explicitly sequenced outbound stanza while detached.
    ///
    /// Entries at or behind `last_acked` are rejected before mutating the
    /// session: acknowledgement has already purged them, so admitting one
    /// would either restore obsolete state or evict a still-valid entry.
    ///
    /// Returns whether the session mutated. Stale and duplicate sequences
    /// are no-ops; callers that persist snapshots must skip the durable
    /// write for them, because persistence restamps `detached_at` and a
    /// no-op retry must not extend the session's resume window.
    pub fn record_detached_outbound_at(
        &mut self,
        sequence: u32,
        stanza_xml: String,
        original_receipt_at: DateTime<Utc>,
    ) -> bool {
        if !sequence_gt(sequence, self.last_acked) {
            return false;
        }
        if self
            .unacked_stanzas
            .iter()
            .any(|entry| entry.sequence == sequence)
        {
            return false;
        }
        if sequence_gt(sequence, self.outbound_count) {
            self.outbound_count = sequence;
        }
        self.unacked_stanzas.push(DetachedUnackedStanza {
            sequence,
            stanza_xml,
            original_receipt_at,
        });
        self.unacked_stanzas
            .sort_by_key(|entry| entry.sequence.wrapping_sub(self.last_acked));
        while self.unacked_stanzas.len() > crate::stream_management::DEFAULT_MAX_UNACKED_QUEUE_SIZE
        {
            let evicted = self.unacked_stanzas.remove(0);
            self.note_detached_eviction(evicted.sequence);
            self.mark_replay_gap_through(evicted.sequence);
        }
        true
    }

    /// Surface a detached-queue eviction that was previously silent (issue
    /// #1219): bump the dedicated counter and warn. A resume with an older
    /// `h` for this session must now fail rather than replay an incomplete
    /// window. Detached recording volume is far lower than the live burst
    /// path (it comes from presence relay / Q6 recording, not MAM
    /// catch-up), so this is not coalesced.
    fn note_detached_eviction(&self, evicted_sequence: u32) {
        crate::telemetry::reliability::increment_sm_detached_unacked_evicted();
        tracing::warn!(
            stream_id = %self.stream_id,
            evicted_sequence,
            "detached SM session unacked queue full; evicted oldest stanza — \
             older resume h values will be rejected"
        );
    }

    fn mark_replay_gap_through(&mut self, sequence: u32) {
        if self
            .replay_gap_through
            .is_none_or(|current| sequence_gt(sequence, current))
        {
            self.replay_gap_through = Some(sequence);
        }
    }
}
