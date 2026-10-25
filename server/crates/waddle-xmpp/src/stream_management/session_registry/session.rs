use crate::ownership::NodeIdentity;
use crate::stream_management::SmIngressFrameReceipt;
use crate::stream_management::DEFAULT_MAX_UNACKED_QUEUE_SIZE;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use jid::FullJid;
use xmpp_parsers::presence::Show;

use super::super::sequence::sequence_gt;
use super::DEFAULT_SESSION_TIMEOUT_SECS;
use crate::Stanza;

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
    /// Ingress obligations confirmed only after this ordered replay prefix is written.
    pub ingress_receipts: Vec<SmIngressFrameReceipt>,
    /// XEP-0198 outbound sequence number assigned to this stanza.
    pub sequence: u32,
    /// Immutable wire XML captured by the production serializer. This is a
    /// retransmission log, so retaining `String` deliberately preserves the
    /// exact replay bytes across detach; reparsing and reserializing a typed
    /// value here could change XML spelling. New registry appends accept typed
    /// stanzas and serialize once; existing entries are never normalized.
    /// See the replay byte contract documented by [`super::super::ReplayStanza`].
    pub stanza_xml: String,
    /// Server-side receipt time of the original stanza. Used by the
    /// Q6 SM-expiry promotion path for the XEP-0203 `<delay/>` stamp.
    pub original_receipt_at: DateTime<Utc>,
}

/// Immutable authority captured with an off-map queue. Fresh synthetic queues
/// share the first acquired fence across clones, so an old clone cannot admit
/// itself again after another copy has completed its source.
#[derive(Debug, Clone)]
pub enum PromotionCaptureCustody {
    /// Existing stream captured under this exact owner and claim epoch.
    Claimed(super::super::persistence::SmClaimFence),
    /// A claimed resume source entering expiry recovery. Its canonical row
    /// must survive until confirmation; retries may never recreate it.
    ExistingSource(super::super::persistence::SmClaimFence),
    /// A newly created source which has never held a claim.
    NeverClaimed(std::sync::Arc<FreshPromotionAdmission>),
    /// A reserved resume attempt may have obtained a new exact fence. Only
    /// that operation can resolve this clone-shared result, never a cache lookup.
    Acquiring(std::sync::Arc<FreshPromotionAdmission>),
    /// Raw wire/storage snapshots do not themselves prove ownership.
    Unproven,
}

impl PromotionCaptureCustody {
    pub fn claimed(fence: super::super::persistence::SmClaimFence) -> Self {
        Self::Claimed(fence)
    }

    pub(super) fn existing_source(fence: super::super::persistence::SmClaimFence) -> Self {
        Self::ExistingSource(fence)
    }

    pub(super) fn requires_existing_source(&self) -> bool {
        matches!(self, Self::ExistingSource(_))
    }

    pub fn never_claimed() -> Self {
        Self::NeverClaimed(std::sync::Arc::new(FreshPromotionAdmission {
            fence: std::sync::OnceLock::new(),
            first_owner: std::sync::OnceLock::new(),
            predecessor_fence: None,
        }))
    }

    pub(super) fn acquiring(
        owner: NodeIdentity,
        predecessor_fence: Option<super::super::persistence::SmClaimFence>,
    ) -> Self {
        Self::Acquiring(std::sync::Arc::new(FreshPromotionAdmission {
            fence: std::sync::OnceLock::new(),
            first_owner: std::sync::OnceLock::from(owner),
            predecessor_fence,
        }))
    }

    pub(super) fn same_acquisition(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Acquiring(left), Self::Acquiring(right)) => std::sync::Arc::ptr_eq(left, right),
            _ => false,
        }
    }

    pub(super) fn retains_predecessor_fence(
        &self,
        cached: Option<&super::super::persistence::SmClaimFence>,
    ) -> bool {
        matches!(self, Self::Acquiring(admission) if admission.predecessor_fence.as_ref() == cached)
    }

    pub fn unproven() -> Self {
        Self::Unproven
    }

    /// The immutable claim that was captured with this queue, if established.
    pub fn captured_fence(&self) -> Option<&super::super::persistence::SmClaimFence> {
        self.fence()
    }

    pub(super) fn fence(&self) -> Option<&super::super::persistence::SmClaimFence> {
        match self {
            Self::Claimed(fence) | Self::ExistingSource(fence) => Some(fence),
            Self::NeverClaimed(admission) | Self::Acquiring(admission) => admission.fence.get(),
            Self::Unproven => None,
        }
    }

    pub(super) fn bind_first_claim(&self, fence: super::super::persistence::SmClaimFence) {
        if let Self::NeverClaimed(admission) | Self::Acquiring(admission) = self {
            // The stream shard serializes fresh admission; a pre-existing
            // value deliberately wins over a later incarnation.
            let _ = admission.fence.set(fence);
        }
    }

    pub(super) fn begin_first_claim(&self, owner: &NodeIdentity) -> bool {
        match self {
            Self::NeverClaimed(admission) => admission.first_owner.set(owner.clone()).is_ok(),
            Self::Claimed(_) | Self::ExistingSource(_) | Self::Acquiring(_) | Self::Unproven => {
                false
            }
        }
    }

    pub(super) fn first_claim_owner(&self) -> Option<&NodeIdentity> {
        match self {
            Self::NeverClaimed(admission) | Self::Acquiring(admission) => {
                admission.first_owner.get()
            }
            Self::Claimed(_) | Self::ExistingSource(_) | Self::Unproven => None,
        }
    }
}

/// Clone-shared admission state. An ambiguous first acquisition is reconciled
/// read-only: retrying admission after an absent claim could resurrect a source
/// already completed by a successor.
#[derive(Debug)]
pub struct FreshPromotionAdmission {
    fence: std::sync::OnceLock<super::super::persistence::SmClaimFence>,
    first_owner: std::sync::OnceLock<NodeIdentity>,
    predecessor_fence: Option<super::super::persistence::SmClaimFence>,
}

/// A detached stream management session.
///
/// Contains all the state needed to resume a stream after disconnection.
#[derive(Debug, Clone)]
pub struct DetachedSession {
    /// Authority bound to this capture, independent of mutable registry caches.
    pub promotion_custody: PromotionCaptureCustody,
    /// The unique stream ID
    pub stream_id: String,
    /// Authenticated user identifier.
    pub user_id: String,
    /// The full JID of the session owner
    pub jid: FullJid,
    /// Per-connection occupancy ownership generation carried through SM
    /// detach/resume so the resumed connection keeps room ownership.
    pub occupancy_session: waddle_xmpp_core::OccupancySessionGeneration,
    /// Server's inbound stanza count at detach time
    pub inbound_count: u32,
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
        max_time == 0 || self.detached_at.elapsed() > Duration::from_secs(max_time as u64)
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
        stanza: &Stanza,
        original_receipt_at: DateTime<Utc>,
    ) {
        self.outbound_count = self.outbound_count.wrapping_add(1);
        if self.unacked_stanzas.len() >= DEFAULT_MAX_UNACKED_QUEUE_SIZE {
            let evicted = self.unacked_stanzas.remove(0);
            self.note_detached_eviction(evicted.sequence);
            self.mark_replay_gap_through(evicted.sequence);
        }
        self.unacked_stanzas.push(DetachedUnackedStanza {
            ingress_receipts: Vec::new(),
            sequence: self.outbound_count,
            stanza_xml: stanza_to_replay_xml(stanza),
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
    /// are no-ops; callers must skip the durable write for them so retries
    /// cannot change counters, replay gaps, or the original detach deadline.
    pub fn record_detached_outbound_at(
        &mut self,
        sequence: u32,
        stanza: &Stanza,
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
            ingress_receipts: Vec::new(),
            sequence,
            stanza_xml: stanza_to_replay_xml(stanza),
            original_receipt_at,
        });
        self.unacked_stanzas
            .sort_by_key(|entry| entry.sequence.wrapping_sub(self.last_acked));
        while self.unacked_stanzas.len() > DEFAULT_MAX_UNACKED_QUEUE_SIZE {
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

/// Serialize only at the replay log boundary; previously captured entries stay byte-exact.
fn stanza_to_replay_xml(stanza: &Stanza) -> String {
    let mut buffer = Vec::new();
    stanza
        .to_element()
        .write_to(&mut buffer)
        .expect("serializing typed stanza should not fail");
    String::from_utf8(buffer).expect("serialized typed stanza is UTF-8")
}
