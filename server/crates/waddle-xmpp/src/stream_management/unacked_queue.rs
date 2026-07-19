//! Unacknowledged Stanza Queue for XEP-0198 Stream Management
//!
//! This module provides a queue for storing outbound stanzas that haven't been
//! acknowledged by the client yet. When a stream is resumed, these stanzas
//! can be resent to ensure reliable delivery.

use std::collections::VecDeque;
use std::time::Instant;

use super::persistence::SmUnackedStanzaPurpose;
use crate::Stanza;
use chrono::{DateTime, Utc};

/// An unacknowledged stanza waiting for client acknowledgment.
#[derive(Debug, Clone)]
pub struct UnackedStanza {
    /// The sequence number of this stanza (outbound count when sent)
    pub sequence: u32,
    /// Typed stanza retained until a transport or storage boundary serializes it.
    pub stanza: Stanza,
    /// When the stanza was sent (monotonic — used for age telemetry).
    pub sent_at: Instant,
    /// Server-side receipt time of the original stanza. For server-
    /// origin replays this is `Utc::now()` at push time; for replays
    /// of `pending_delivery` rows it's the row's `original_receipt_at`.
    /// Used by the Q6 SM-expiry promotion path for the XEP-0203
    /// `<delay/>` stamp on offline replays (issue #209 PR #361).
    pub original_receipt_at: DateTime<Utc>,
    /// Typed recovery disposition preserved through detach and replay.
    pub purpose: SmUnackedStanzaPurpose,
}

/// Outcome of a `push` onto the unacked queue.
///
/// `Evicted(stanza)` carries the stanza that had to be dropped to make
/// room for the new entry, so `StreamManagementState` can emit a
/// structured warning and bump the eviction metric. Silent eviction —
/// the previous behaviour — is what causes `<resumed/>` to succeed
/// while the caller's earliest messages are already gone, which is the
/// sender-side half of "reconnects drop messages".
#[derive(Debug)]
pub enum UnackedPushResult {
    /// Queue had room; the new stanza was appended without eviction.
    Accepted,
    /// Queue was at capacity; the oldest stanza was evicted to make
    /// room. The evicted entry is returned so the caller can log its
    /// sequence number and mark the replay window as truncated.
    Evicted(Box<UnackedStanza>),
}

impl UnackedStanza {
    /// Create a new unacked stanza, stamping `original_receipt_at`
    /// with `Utc::now()`. Use [`Self::with_receipt_at`] when the
    /// caller already knows the original receipt time (e.g. when
    /// replaying a `pending_delivery` row).
    pub fn new(sequence: u32, stanza: Stanza, purpose: SmUnackedStanzaPurpose) -> Self {
        Self::with_receipt_at(sequence, stanza, Utc::now(), purpose)
    }

    /// Create a new unacked stanza with an explicit receipt time.
    pub fn with_receipt_at(
        sequence: u32,
        stanza: Stanza,
        original_receipt_at: DateTime<Utc>,
        purpose: SmUnackedStanzaPurpose,
    ) -> Self {
        Self {
            sequence,
            stanza,
            sent_at: Instant::now(),
            original_receipt_at,
            purpose,
        }
    }

    /// Get the age of this stanza (time since it was sent).
    pub fn age(&self) -> std::time::Duration {
        self.sent_at.elapsed()
    }
}

/// Queue of unacknowledged outbound stanzas.
///
/// Maintains stanzas in FIFO order, removing them when acknowledged.
/// Has a maximum size to prevent unbounded memory growth.
#[derive(Debug)]
pub struct UnackedQueue {
    /// The queue of unacked stanzas
    stanzas: VecDeque<UnackedStanza>,
    /// Maximum number of stanzas to store
    max_size: usize,
}

impl UnackedQueue {
    /// Create a new unacked queue with the specified maximum size.
    pub fn new(max_size: usize) -> Self {
        Self {
            stanzas: VecDeque::with_capacity(max_size.min(1024)),
            max_size,
        }
    }

    /// Push a new stanza onto the queue.
    ///
    /// Returns `UnackedPushResult::Accepted` when the queue had
    /// capacity, or `UnackedPushResult::Evicted(stanza)` when the
    /// queue was full and the oldest entry had to be removed to make
    /// room. Callers MUST observe the evicted variant (log + metric +
    /// replay-gap marker) so later `<resume/>` requests with older `h`
    /// values can fail instead of replaying an incomplete window.
    #[must_use = "eviction must be observed so missing-after-resume drops are not silent"]
    pub fn push(
        &mut self,
        sequence: u32,
        stanza: Stanza,
        purpose: SmUnackedStanzaPurpose,
    ) -> UnackedPushResult {
        self.push_with_receipt_at(sequence, stanza, Utc::now(), purpose)
    }

    /// Push a stanza with an explicit `original_receipt_at`. Used by
    /// the `pending_delivery` flush path so the row's receipt time
    /// flows into a future XEP-0203 `<delay/>` on Q6 promotion.
    #[must_use = "eviction must be observed so missing-after-resume drops are not silent"]
    pub fn push_with_receipt_at(
        &mut self,
        sequence: u32,
        stanza: Stanza,
        original_receipt_at: DateTime<Utc>,
        purpose: SmUnackedStanzaPurpose,
    ) -> UnackedPushResult {
        let evicted = if self.stanzas.len() >= self.max_size {
            self.stanzas.pop_front()
        } else {
            None
        };

        self.stanzas.push_back(UnackedStanza::with_receipt_at(
            sequence,
            stanza,
            original_receipt_at,
            purpose,
        ));

        match evicted {
            Some(stanza) => UnackedPushResult::Evicted(Box::new(stanza)),
            None => UnackedPushResult::Accepted,
        }
    }

    /// Remove all stanzas with sequence <= h (they've been acknowledged).
    pub fn acknowledge(&mut self, h: u32) {
        // Remove stanzas from front while their sequence <= h
        while let Some(front) = self.stanzas.front() {
            if sequence_lte(front.sequence, h) {
                self.stanzas.pop_front();
            } else {
                break;
            }
        }
    }

    /// Get all stanzas with sequence > h that need to be resent.
    ///
    /// This is used during stream resumption when the client reports
    /// which stanza it last received. Each entry carries the original
    /// receipt time so the resume path can stamp the XEP-0203
    /// `<delay/>` with the true send time (issue #1178).
    pub fn get_unacked_after(&self, h: u32) -> Vec<super::ReplayStanza> {
        self.stanzas
            .iter()
            .filter(|s| sequence_gt(s.sequence, h))
            .map(|s| super::ReplayStanza {
                stanza: s.stanza.clone(),
                original_receipt_at: s.original_receipt_at,
                purpose: s.purpose,
            })
            .collect()
    }

    /// Get every queued stanza as a [`DetachedUnackedStanza`] suitable
    /// for storage on a detached SM session. Preserves the original
    /// receipt time so the Q6 SM-expiry promotion path can stamp the
    /// XEP-0203 `<delay/>` correctly on offline replays.
    pub fn get_all_unacked(&self) -> Vec<super::session_registry::DetachedUnackedStanza> {
        self.stanzas
            .iter()
            .map(|s| super::session_registry::DetachedUnackedStanza {
                sequence: s.sequence,
                stanza: s.stanza.clone(),
                original_receipt_at: s.original_receipt_at,
                purpose: s.purpose,
            })
            .collect()
    }

    /// Restore the queue from the stored detached-session form.
    /// Called during XEP-0198 stream resumption.
    pub fn restore(&mut self, stanzas: &[super::session_registry::DetachedUnackedStanza]) {
        self.stanzas.clear();
        for entry in stanzas {
            if self.stanzas.len() < self.max_size {
                self.stanzas.push_back(UnackedStanza::with_receipt_at(
                    entry.sequence,
                    entry.stanza.clone(),
                    entry.original_receipt_at,
                    entry.purpose,
                ));
            }
        }
    }

    /// Get the number of stanzas in the queue.
    pub fn len(&self) -> usize {
        self.stanzas.len()
    }

    /// Check if the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.stanzas.is_empty()
    }

    /// Clear the queue.
    pub fn clear(&mut self) {
        self.stanzas.clear();
    }

    /// Get the sequence number of the oldest stanza (if any).
    pub fn oldest_sequence(&self) -> Option<u32> {
        self.stanzas.front().map(|s| s.sequence)
    }

    /// Get the sequence number of the newest stanza (if any).
    pub fn newest_sequence(&self) -> Option<u32> {
        self.stanzas.back().map(|s| s.sequence)
    }
}

/// Check if sequence a <= b, handling wrap-around.
///
/// XEP-0198 specifies that sequence numbers wrap at 2^32.
/// Per RFC 1982-like comparison, we use a window of 2^31.
pub(super) fn sequence_lte(a: u32, b: u32) -> bool {
    // If a == b, it's equal
    if a == b {
        return true;
    }

    // Otherwise, a < b if (b - a) mod 2^32 < 2^31
    let diff = b.wrapping_sub(a);
    diff < 0x8000_0000
}

/// Check if sequence a > b, handling wrap-around.
pub(super) fn sequence_gt(a: u32, b: u32) -> bool {
    !sequence_lte(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: impl ToString) -> Stanza {
        let mut message = xmpp_parsers::message::Message::new(None::<jid::Jid>);
        message.id = Some(xmpp_parsers::message::Id(id.to_string()));
        Stanza::Message(message)
    }

    fn stanza_has_id(stanza: &Stanza, expected: &str) -> bool {
        matches!(
            stanza,
            Stanza::Message(message)
                if message.id.as_ref().is_some_and(|id| id.0 == expected)
        )
    }

    fn push_accepted(queue: &mut UnackedQueue, sequence: u32, id: &str) {
        match queue.push(sequence, message(id), SmUnackedStanzaPurpose::Application) {
            UnackedPushResult::Accepted => {}
            UnackedPushResult::Evicted(stanza) => {
                panic!("unexpected eviction of stanza sequence={}", stanza.sequence)
            }
        }
    }

    #[test]
    fn test_unacked_queue_basic() {
        let mut queue = UnackedQueue::new(10);
        assert!(queue.is_empty());

        push_accepted(&mut queue, 1, "msg1");
        push_accepted(&mut queue, 2, "msg2");
        push_accepted(&mut queue, 3, "msg3");

        assert_eq!(queue.len(), 3);
        assert_eq!(queue.oldest_sequence(), Some(1));
        assert_eq!(queue.newest_sequence(), Some(3));
    }

    #[test]
    fn test_unacked_queue_acknowledge() {
        let mut queue = UnackedQueue::new(10);

        push_accepted(&mut queue, 1, "msg1");
        push_accepted(&mut queue, 2, "msg2");
        push_accepted(&mut queue, 3, "msg3");
        push_accepted(&mut queue, 4, "msg4");

        // Acknowledge up to 2
        queue.acknowledge(2);
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.oldest_sequence(), Some(3));

        // Acknowledge up to 4
        queue.acknowledge(4);
        assert!(queue.is_empty());
    }

    #[test]
    fn test_unacked_queue_get_unacked_after() {
        let mut queue = UnackedQueue::new(10);

        push_accepted(&mut queue, 1, "msg1");
        push_accepted(&mut queue, 2, "msg2");
        push_accepted(&mut queue, 3, "msg3");
        push_accepted(&mut queue, 4, "msg4");

        // Get stanzas after h=2 (should be 3 and 4)
        let unacked = queue.get_unacked_after(2);
        assert_eq!(unacked.len(), 2);
        assert!(stanza_has_id(&unacked[0].stanza, "msg3"));
        assert!(stanza_has_id(&unacked[1].stanza, "msg4"));

        // Get stanzas after h=0 (all of them)
        let unacked = queue.get_unacked_after(0);
        assert_eq!(unacked.len(), 4);
    }

    #[test]
    fn test_unacked_queue_max_size_returns_evicted_stanza() {
        let mut queue = UnackedQueue::new(3);

        push_accepted(&mut queue, 1, "msg1");
        push_accepted(&mut queue, 2, "msg2");
        push_accepted(&mut queue, 3, "msg3");
        assert_eq!(queue.len(), 3);

        // Adding a 4th should remove the oldest and surface the evicted
        // stanza so `StreamManagementState::record_outbound` can warn
        // about it instead of dropping silently.
        let result = queue.push(4, message("msg4"), SmUnackedStanzaPurpose::Application);
        let evicted = match result {
            UnackedPushResult::Evicted(stanza) => stanza,
            UnackedPushResult::Accepted => panic!("queue at capacity must evict"),
        };
        assert_eq!(evicted.sequence, 1);
        assert!(stanza_has_id(&evicted.stanza, "msg1"));
        assert_eq!(queue.len(), 3);
        assert_eq!(queue.oldest_sequence(), Some(2));
    }

    #[test]
    fn test_unacked_queue_restore() {
        use super::super::session_registry::DetachedUnackedStanza;
        let mut queue = UnackedQueue::new(10);
        let now = Utc::now();
        let stanzas = vec![
            DetachedUnackedStanza {
                sequence: 5,
                stanza: message("msg5"),
                original_receipt_at: now,
                purpose: SmUnackedStanzaPurpose::Application,
            },
            DetachedUnackedStanza {
                sequence: 6,
                stanza: message("msg6"),
                original_receipt_at: now,
                purpose: SmUnackedStanzaPurpose::Application,
            },
            DetachedUnackedStanza {
                sequence: 7,
                stanza: message("msg7"),
                original_receipt_at: now,
                purpose: SmUnackedStanzaPurpose::Application,
            },
        ];
        queue.restore(&stanzas);
        assert_eq!(queue.len(), 3);
        assert_eq!(queue.oldest_sequence(), Some(5));
        assert_eq!(queue.newest_sequence(), Some(7));
    }

    #[test]
    fn replay_purpose_survives_partial_replay_detach_and_restore() {
        let now = Utc::now();
        let mut queue = UnackedQueue::new(10);
        let mut message = xmpp_parsers::message::Message::new(None::<jid::Jid>);
        message.id = Some(xmpp_parsers::message::Id("application".to_string()));
        let message = Stanza::Message(message);
        let barrier = xmpp_parsers::iq::Iq::Get {
            from: None,
            to: None,
            id: "resume-barrier".to_string(),
            payload: minidom::Element::builder("ping", crate::xep::xep0199::NS_PING).build(),
        };
        let barrier = Stanza::Iq(Box::new(barrier));
        assert!(matches!(
            queue.push_with_receipt_at(1, message, now, SmUnackedStanzaPurpose::Application,),
            UnackedPushResult::Accepted
        ));
        assert!(matches!(
            queue.push_with_receipt_at(2, barrier, now, SmUnackedStanzaPurpose::ResumeBarrier,),
            UnackedPushResult::Accepted
        ));

        let suffix = queue.get_unacked_after(1);
        assert_eq!(suffix.len(), 1);
        assert_eq!(suffix[0].purpose, SmUnackedStanzaPurpose::ResumeBarrier);

        let detached = queue.get_all_unacked();
        assert_eq!(detached[0].purpose, SmUnackedStanzaPurpose::Application);
        assert_eq!(detached[1].purpose, SmUnackedStanzaPurpose::ResumeBarrier);

        let mut restored = UnackedQueue::new(10);
        restored.restore(&detached);
        let restored_suffix = restored.get_unacked_after(1);
        assert_eq!(restored_suffix.len(), 1);
        assert_eq!(
            restored_suffix[0].purpose,
            SmUnackedStanzaPurpose::ResumeBarrier
        );
    }

    #[test]
    fn test_sequence_comparison() {
        // Normal cases
        assert!(sequence_lte(1, 2));
        assert!(sequence_lte(1, 1));
        assert!(!sequence_lte(2, 1));

        assert!(sequence_gt(2, 1));
        assert!(!sequence_gt(1, 1));
        assert!(!sequence_gt(1, 2));

        // Wrap-around cases
        assert!(sequence_lte(0xFFFF_FFFF, 0)); // Wrapped
        assert!(sequence_gt(0, 0xFFFF_FFFF)); // 0 is after max when wrapped
    }

    #[test]
    fn test_unacked_queue_clear() {
        let mut queue = UnackedQueue::new(10);

        push_accepted(&mut queue, 1, "<msg1/>");
        push_accepted(&mut queue, 2, "<msg2/>");
        assert!(!queue.is_empty());

        queue.clear();
        assert!(queue.is_empty());
    }
}
