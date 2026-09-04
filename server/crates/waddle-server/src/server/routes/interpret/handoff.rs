use std::collections::{BTreeMap, BTreeSet};
#[cfg(feature = "clustering")]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[cfg(feature = "clustering")]
use tokio::sync::mpsc;
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::ingress::IngressOrdinal;
use waddle_xmpp::ingress::NormalizedTarget;
use waddle_xmpp::ownership::{ClaimEpoch, NodeIdentity};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::{stream_management::StreamManagementState, Stanza};
use xmpp_parsers::message::Message;

const MAX_PARKED_SHADOW_SUBMISSIONS: usize = 256;

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
    shadow_parked: BTreeMap<u32, ParkedIngressShadowSubmission>,
    /// Payloads evicted from the bounded parking map. Their sequence remains
    /// until drain so the non-reusable shadow ordinal still forms a visible
    /// frontier gap rather than compacting away missing shadow work.
    shadow_discarded: BTreeSet<u32>,
    /// First stanza whose dispatch was cancelled before the server accepted
    /// XEP-0198 responsibility. The connection terminates after creating this
    /// hole; retaining it here prevents late or later completions from
    /// advancing `h` across the sender-owned stanza.
    first_unhandled: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ParkedIngressShadowSubmission {
    pub stream_id: SmSessionId,
    pub owner: NodeIdentity,
    pub claim_epoch: ClaimEpoch,
    pub principal: AuthenticatedPrincipalRef,
    pub target: NormalizedTarget,
    pub message: Message,
    pub capture: crate::ingress_shadow::IngressEffectCapture,
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

    pub fn park_shadow_submission(
        &mut self,
        sequence: OrderedRelayInboundSequence,
        submission: ParkedIngressShadowSubmission,
    ) {
        if !self.pending.contains(&sequence.0) {
            return;
        }
        if !self.shadow_parked.contains_key(&sequence.0)
            && self.shadow_parked.len() >= MAX_PARKED_SHADOW_SUBMISSIONS
        {
            let Some((discarded_sequence, discarded)) = self.shadow_parked.pop_first() else {
                return;
            };
            self.shadow_discarded.insert(discarded_sequence);
            crate::ingress_shadow::observe(
                crate::ingress_shadow::IngressShadowObservation::Dropped {
                    kind: crate::ingress_shadow::IngressShadowRequestKind::Submit,
                    stream_id: discarded.stream_id,
                    reason: crate::ingress_shadow::IngressShadowDropReason::ParkingFull,
                },
            );
        }
        self.shadow_parked.insert(sequence.0, submission);
    }

    pub fn complete(
        &mut self,
        sequence: OrderedRelayInboundSequence,
        sm_state: &mut StreamManagementState,
        mut on_drained: impl FnMut(crate::ingress_shadow::IngressShadowSubmission),
    ) {
        if !self.pending.remove(&sequence.0) {
            return;
        }
        self.completed.insert(sequence.0);
        loop {
            let next = sm_state.get_inbound_count().wrapping_add(1);
            if self.first_unhandled == Some(next) {
                break;
            }
            if !self.completed.remove(&next) {
                break;
            }
            sm_state.increment_inbound();
            let parked = self.shadow_parked.remove(&next);
            let discarded = self.shadow_discarded.remove(&next);
            if let Some(parked) = parked {
                let Some(next_shadow_ordinal) = sm_state.shadow_ordinal.next() else {
                    continue;
                };
                sm_state.shadow_ordinal = next_shadow_ordinal;
                let handled_ordinal =
                    IngressOrdinal::from_storage(next_shadow_ordinal.to_storage())
                        .expect("shadow ordinal increments from zero to a valid ingress ordinal");
                let capture = parked.capture.snapshot();
                on_drained(crate::ingress_shadow::IngressShadowSubmission {
                    stream_id: parked.stream_id,
                    owner: parked.owner,
                    claim_epoch: parked.claim_epoch,
                    handled_ordinal,
                    principal: parked.principal,
                    target: parked.target,
                    message: capture.sanitized_message.clone().unwrap_or(parked.message),
                    capture,
                    connection_generation: None,
                });
            } else if discarded {
                // The ordinal is deliberately consumed without enqueueing a
                // task. The next submitted ordinal is then stale, exposing
                // the dropped shadow input instead of hiding it.
                let Some(next_shadow_ordinal) = sm_state.shadow_ordinal.next() else {
                    continue;
                };
                sm_state.shadow_ordinal = next_shadow_ordinal;
            }
        }
    }

    pub fn abandon(&mut self, sequence: OrderedRelayInboundSequence) {
        if !self.pending.remove(&sequence.0) {
            return;
        }
        self.completed.remove(&sequence.0);
        self.shadow_parked.remove(&sequence.0);
        self.shadow_discarded.remove(&sequence.0);
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
        self.next_reserved = None;
        self.pending.clear();
        self.completed.clear();
        self.shadow_parked.clear();
        self.shadow_discarded.clear();
        self.first_unhandled = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::auth::{AuthContextId, AuthContextVersion, PrincipalAuthEpoch};

    fn enabled_sm_state() -> StreamManagementState {
        let mut sm_state = StreamManagementState::new();
        sm_state.enable("stream-1".to_string(), true, Some(300));
        sm_state
    }

    fn parked_submission() -> ParkedIngressShadowSubmission {
        ParkedIngressShadowSubmission {
            stream_id: SmSessionId::new("stream-1"),
            owner: NodeIdentity::new("node-a", "incarnation-a"),
            claim_epoch: ClaimEpoch(7),
            principal: AuthenticatedPrincipalRef::new(
                "romeo@example.com".parse().expect("bare jid"),
                AuthContextId::new(uuid::Uuid::new_v4()),
                AuthContextVersion::new(1),
                PrincipalAuthEpoch::new(1),
            ),
            target: NormalizedTarget::Absent,
            message: Message::new(None),
            capture: crate::ingress_shadow::IngressEffectCapture::new(None),
        }
    }

    #[test]
    fn completion_tracker_advances_inbound_count_only_when_contiguous() {
        let mut sm_state = enabled_sm_state();
        let mut tracker = SmInboundCompletionTracker::default();
        let mut submissions = Vec::new();

        let first = tracker.reserve(&sm_state);
        let second = tracker.reserve(&sm_state);
        tracker.park_shadow_submission(first, parked_submission());
        tracker.park_shadow_submission(second, parked_submission());

        tracker.complete(second, &mut sm_state, |submission| {
            submissions.push(submission);
        });
        assert_eq!(sm_state.get_inbound_count(), 0);
        assert!(tracker.has_pending());
        assert!(submissions.is_empty());

        tracker.complete(first, &mut sm_state, |submission| {
            submissions.push(submission);
        });
        assert_eq!(sm_state.get_inbound_count(), 2);
        assert!(!tracker.has_pending());
        assert_eq!(sm_state.shadow_ordinal.to_storage(), 2);
        assert_eq!(submissions.len(), 2);
        assert_eq!(submissions[0].handled_ordinal.to_storage(), 1);
        assert_eq!(submissions[1].handled_ordinal.to_storage(), 2);
    }

    #[test]
    fn completion_tracker_resets_pending_slots_without_mutating_sm_count() {
        let sm_state = enabled_sm_state();
        let mut tracker = SmInboundCompletionTracker::default();
        let mut submissions = Vec::new();

        let reserved = tracker.reserve(&sm_state);
        assert_eq!(reserved, OrderedRelayInboundSequence(1));
        assert!(tracker.has_pending());
        tracker.park_shadow_submission(reserved, parked_submission());

        tracker.reset();
        assert!(!tracker.has_pending());
        let mut sm_state = enabled_sm_state();
        tracker.complete(reserved, &mut sm_state, |submission| {
            submissions.push(submission);
        });
        assert!(submissions.is_empty());
        assert_eq!(sm_state.shadow_ordinal.to_storage(), 0);
    }

    #[test]
    fn abandoned_sequence_never_advances_handled_count_or_waits_for_completion() {
        let mut sm_state = enabled_sm_state();
        let mut tracker = SmInboundCompletionTracker::default();
        let mut submissions = Vec::new();

        let abandoned = tracker.reserve(&sm_state);
        tracker.park_shadow_submission(abandoned, parked_submission());
        tracker.abandon(abandoned);

        assert_eq!(sm_state.get_inbound_count(), 0);
        assert!(!tracker.has_pending());
        assert!(tracker.has_unhandled_hole());

        let later = tracker.reserve(&sm_state);
        tracker.complete(later, &mut sm_state, |submission| {
            submissions.push(submission);
        });
        tracker.complete(abandoned, &mut sm_state, |submission| {
            submissions.push(submission);
        });

        assert_eq!(sm_state.get_inbound_count(), 0);
        assert!(!tracker.has_pending());
        assert!(submissions.is_empty());
        assert_eq!(sm_state.shadow_ordinal.to_storage(), 0);
    }

    #[test]
    fn completion_before_abandoned_sequence_still_advances_to_the_hole() {
        let mut sm_state = enabled_sm_state();
        let mut tracker = SmInboundCompletionTracker::default();
        let mut submissions = Vec::new();

        let first = tracker.reserve(&sm_state);
        let abandoned = tracker.reserve(&sm_state);
        tracker.park_shadow_submission(first, parked_submission());
        tracker.park_shadow_submission(abandoned, parked_submission());
        tracker.abandon(abandoned);
        tracker.complete(first, &mut sm_state, |submission| {
            submissions.push(submission);
        });

        assert_eq!(sm_state.get_inbound_count(), 1);
        assert!(tracker.has_unhandled_hole());
        assert!(!tracker.has_pending());
        assert_eq!(submissions.len(), 1);
        assert_eq!(submissions[0].handled_ordinal.to_storage(), 1);
        assert_eq!(sm_state.shadow_ordinal.to_storage(), 1);
    }

    #[test]
    fn completing_after_abandon_never_allocates_shadow_submission() {
        let mut sm_state = enabled_sm_state();
        let mut tracker = SmInboundCompletionTracker::default();
        let mut submissions = Vec::new();

        let deferred = tracker.reserve(&sm_state);
        tracker.park_shadow_submission(deferred, parked_submission());
        tracker.abandon(deferred);
        tracker.complete(deferred, &mut sm_state, |submission| {
            submissions.push(submission);
        });

        assert!(submissions.is_empty());
        assert_eq!(sm_state.shadow_ordinal.to_storage(), 0);
    }

    #[test]
    fn completion_without_a_parked_shadow_submission_skips_shadow_allocation() {
        let mut sm_state = enabled_sm_state();
        let mut tracker = SmInboundCompletionTracker::default();
        let mut submissions = Vec::new();

        let first = tracker.reserve(&sm_state);
        tracker.complete(first, &mut sm_state, |submission| {
            submissions.push(submission);
        });

        assert_eq!(sm_state.get_inbound_count(), 1);
        assert!(submissions.is_empty());
        assert_eq!(sm_state.shadow_ordinal.to_storage(), 0);
    }

    #[test]
    fn parking_overflow_preserves_a_visible_shadow_ordinal_gap() {
        let mut sm_state = enabled_sm_state();
        let mut tracker = SmInboundCompletionTracker::default();
        let mut reserved = Vec::new();

        for _ in 0..=MAX_PARKED_SHADOW_SUBMISSIONS {
            let sequence = tracker.reserve(&sm_state);
            tracker.park_shadow_submission(sequence, parked_submission());
            reserved.push(sequence);
        }

        assert_eq!(tracker.shadow_parked.len(), MAX_PARKED_SHADOW_SUBMISSIONS);
        assert!(
            !tracker.shadow_parked.contains_key(&reserved[0].0),
            "the oldest parked input is discarded before an unbounded backlog forms"
        );
        assert!(tracker.shadow_parked.contains_key(&reserved[1].0));

        let mut submissions = Vec::new();
        tracker.complete(reserved[0], &mut sm_state, |submission| {
            submissions.push(submission);
        });
        tracker.complete(reserved[1], &mut sm_state, |submission| {
            submissions.push(submission);
        });

        assert_eq!(sm_state.shadow_ordinal.to_storage(), 2);
        assert_eq!(submissions.len(), 1);
        assert_eq!(submissions[0].handled_ordinal.to_storage(), 2);
    }

    #[test]
    fn shadow_ordinal_continues_after_detach_and_resume_restore() {
        let mut sm_state = enabled_sm_state();
        let mut tracker = SmInboundCompletionTracker::default();
        let mut first_submissions = Vec::new();

        let first = tracker.reserve(&sm_state);
        tracker.park_shadow_submission(first, parked_submission());
        tracker.complete(first, &mut sm_state, |submission| {
            first_submissions.push(submission);
        });
        assert_eq!(first_submissions[0].handled_ordinal.to_storage(), 1);

        let detached = sm_state
            .to_detached_session(waddle_xmpp::stream_management::DetachedSessionSnapshot {
                user_id: "alice@example.com".to_string(),
                jid: "alice@example.com/web".parse().expect("jid"),
                occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                carbons_enabled: false,
                roster_interested: false,
                blocklist_interested: false,
                presence_available: false,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
                presence_payloads: Vec::new(),
                pending_subscribes_flushed: false,
            })
            .expect("resumable snapshot");

        let mut resumed_state = StreamManagementState::new();
        resumed_state.restore_from_session(&detached);
        let mut resumed_tracker = SmInboundCompletionTracker::default();
        let mut resumed_submissions = Vec::new();
        let second = resumed_tracker.reserve(&resumed_state);
        let mut resumed_submission = parked_submission();
        resumed_submission.claim_epoch = ClaimEpoch(8);
        resumed_tracker.park_shadow_submission(second, resumed_submission);
        resumed_tracker.complete(second, &mut resumed_state, |submission| {
            resumed_submissions.push(submission);
        });

        assert_eq!(resumed_submissions.len(), 1);
        assert_eq!(resumed_submissions[0].handled_ordinal.to_storage(), 2);
        assert_eq!(resumed_state.shadow_ordinal.to_storage(), 2);
    }
}
