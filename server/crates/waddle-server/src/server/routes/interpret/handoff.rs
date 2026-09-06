use std::collections::{BTreeMap, BTreeSet};
#[cfg(feature = "clustering")]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[cfg(feature = "clustering")]
use tokio::sync::mpsc;
use waddle_xmpp::{stream_management::StreamManagementState, Stanza};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OrderedRelayInboundSequence(pub u32);

#[derive(Debug)]
pub struct OrderedRelayHandoffCompletion {
    pub inbound_sequence: OrderedRelayInboundSequence,
    pub replies: Vec<Stanza>,
}

#[derive(Debug, Clone)]
#[cfg(feature = "clustering")]
pub struct OrderedRelayHandoffHandle {
    inbound_sequence: OrderedRelayInboundSequence,
    tx: mpsc::UnboundedSender<OrderedRelayHandoffCompletion>,
    deferred: Arc<AtomicBool>,
}

#[cfg(feature = "clustering")]
impl OrderedRelayHandoffHandle {
    pub fn new(
        inbound_sequence: OrderedRelayInboundSequence,
        tx: mpsc::UnboundedSender<OrderedRelayHandoffCompletion>,
    ) -> Self {
        Self {
            inbound_sequence,
            tx,
            deferred: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn mark_deferred(&self) -> bool {
        self.deferred
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn was_deferred(&self) -> bool {
        self.deferred.load(Ordering::Acquire)
    }

    pub fn complete(&self, replies: Vec<Stanza>) {
        let _ = self.tx.send(OrderedRelayHandoffCompletion {
            inbound_sequence: self.inbound_sequence,
            replies,
        });
    }
}

#[derive(Debug, Default)]
pub struct SmInboundCompletionTracker {
    next_reserved: Option<u32>,
    contiguous: u32,
    pending: BTreeSet<u32>,
    completed: BTreeSet<u32>,
    committed: BTreeMap<u32, u32>,
    checkpoint_dirty: bool,
    first_unhandled: Option<u32>,
}

impl SmInboundCompletionTracker {
    pub fn reserve(&mut self, sm_state: &StreamManagementState) -> OrderedRelayInboundSequence {
        let current = sm_state.get_inbound_count();
        self.contiguous = current;
        let next = self
            .next_reserved
            .filter(|next| *next != current)
            .unwrap_or_else(|| current.wrapping_add(1));
        self.next_reserved = Some(next.wrapping_add(1));
        self.pending.insert(next);
        OrderedRelayInboundSequence(next)
    }

    /// The count this commit can expose without acknowledging a pending hole.
    pub fn checkpoint_for(&self, sequence: OrderedRelayInboundSequence) -> u32 {
        let distance = sequence.0.wrapping_sub(self.contiguous);
        if self.first_unhandled.is_some()
            || self.pending.iter().any(|pending| {
                *pending != sequence.0 && pending.wrapping_sub(self.contiguous) < distance
            })
        {
            self.contiguous
        } else {
            sequence.0
        }
    }

    pub fn mark_committed(&mut self, sequence: OrderedRelayInboundSequence, checkpoint_h: u32) {
        if self.pending.contains(&sequence.0) {
            self.committed.insert(sequence.0, checkpoint_h);
        }
    }

    pub fn complete(
        &mut self,
        sequence: OrderedRelayInboundSequence,
        sm_state: &mut StreamManagementState,
    ) {
        if !self.pending.remove(&sequence.0) {
            return;
        }
        self.completed.insert(sequence.0);
        loop {
            let next = sm_state.get_inbound_count().wrapping_add(1);
            if self.first_unhandled == Some(next) || !self.completed.remove(&next) {
                break;
            }
            sm_state.increment_inbound();
            self.contiguous = next;
            if self
                .committed
                .remove(&next)
                .is_some_and(|checkpoint| checkpoint != next)
            {
                self.checkpoint_dirty = true;
            }
        }
    }

    pub fn checkpoint_dirty(&self) -> bool {
        self.checkpoint_dirty
    }
    pub fn checkpoint_flushed(&mut self) {
        self.checkpoint_dirty = false;
    }

    pub fn abandon(&mut self, sequence: OrderedRelayInboundSequence) {
        if !self.pending.remove(&sequence.0) {
            return;
        }
        self.completed.remove(&sequence.0);
        self.committed.remove(&sequence.0);
        if self.first_unhandled.is_none() {
            self.first_unhandled = Some(sequence.0);
        }
    }
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
    pub fn has_unhandled_hole(&self) -> bool {
        self.first_unhandled.is_some()
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> StreamManagementState {
        let mut sm = StreamManagementState::new();
        sm.enable("stream".to_owned(), true, Some(300));
        sm
    }
    #[test]
    fn deferred_iq_completion_dirties_checkpoint_after_committed_messages() {
        let mut sm = state();
        let mut tracker = SmInboundCompletionTracker::default();
        let iq = tracker.reserve(&sm);
        for _ in 0..2 {
            let message = tracker.reserve(&sm);
            let checkpoint = tracker.checkpoint_for(message);
            assert_eq!(checkpoint, 0);
            tracker.mark_committed(message, checkpoint);
            tracker.complete(message, &mut sm);
        }
        assert_eq!(sm.get_inbound_count(), 0);
        assert!(!tracker.checkpoint_dirty());
        tracker.complete(iq, &mut sm);
        assert_eq!(sm.get_inbound_count(), 3);
        assert!(tracker.checkpoint_dirty());
        tracker.checkpoint_flushed();
        assert!(!tracker.checkpoint_dirty());
    }
    #[test]
    fn own_commit_checkpoint_needs_no_flush() {
        let mut sm = state();
        let mut tracker = SmInboundCompletionTracker::default();
        let seq = tracker.reserve(&sm);
        assert_eq!(tracker.checkpoint_for(seq), 1);
        tracker.mark_committed(seq, 1);
        tracker.complete(seq, &mut sm);
        assert_eq!(sm.get_inbound_count(), 1);
        assert!(!tracker.checkpoint_dirty());
    }
    #[test]
    fn abandoned_sequence_preserves_hole_on_late_completion() {
        let mut sm = state();
        let mut tracker = SmInboundCompletionTracker::default();
        let first = tracker.reserve(&sm);
        let hole = tracker.reserve(&sm);
        let later = tracker.reserve(&sm);
        tracker.abandon(hole);
        tracker.complete(later, &mut sm);
        tracker.complete(first, &mut sm);
        tracker.complete(hole, &mut sm);
        assert_eq!(sm.get_inbound_count(), 1);
        assert!(tracker.has_unhandled_hole());
        assert!(!tracker.has_pending());
        tracker.reset();
        assert!(!tracker.has_unhandled_hole());
    }
}
