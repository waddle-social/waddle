use std::collections::BTreeSet;
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
    pending: BTreeSet<u32>,
    completed: BTreeSet<u32>,
}

impl SmInboundCompletionTracker {
    pub fn reserve(&mut self, sm_state: &StreamManagementState) -> OrderedRelayInboundSequence {
        let current = sm_state.get_inbound_count();
        let next = self
            .next_reserved
            .filter(|next| *next != current)
            .unwrap_or_else(|| current.wrapping_add(1));
        self.next_reserved = Some(next.wrapping_add(1));
        self.pending.insert(next);
        OrderedRelayInboundSequence(next)
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
            if !self.completed.remove(&next) {
                break;
            }
            sm_state.increment_inbound();
        }
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn reset(&mut self) {
        self.next_reserved = None;
        self.pending.clear();
        self.completed.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_sm_state() -> StreamManagementState {
        let mut sm_state = StreamManagementState::new();
        sm_state.enable("stream-1".to_string(), true, Some(300));
        sm_state
    }

    #[test]
    fn completion_tracker_advances_inbound_count_only_when_contiguous() {
        let mut sm_state = enabled_sm_state();
        let mut tracker = SmInboundCompletionTracker::default();

        let first = tracker.reserve(&sm_state);
        let second = tracker.reserve(&sm_state);

        tracker.complete(second, &mut sm_state);
        assert_eq!(sm_state.get_inbound_count(), 0);
        assert!(tracker.has_pending());

        tracker.complete(first, &mut sm_state);
        assert_eq!(sm_state.get_inbound_count(), 2);
        assert!(!tracker.has_pending());
    }

    #[test]
    fn completion_tracker_resets_pending_slots_without_mutating_sm_count() {
        let sm_state = enabled_sm_state();
        let mut tracker = SmInboundCompletionTracker::default();

        let reserved = tracker.reserve(&sm_state);
        assert_eq!(reserved, OrderedRelayInboundSequence(1));
        assert!(tracker.has_pending());

        tracker.reset();
        assert!(!tracker.has_pending());
    }
}
