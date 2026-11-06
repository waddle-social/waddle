//! Test-only model of a contiguous outbound stream-management frontier.
//!
//! This model assumes that its caller has current authority and a known
//! committed exposure fence, and records the next exposure immediately before
//! one write attempt. It cannot prove the fence, authority, an actual write, or
//! durable recovery. Replay rows are represented only by their ordinals.

use std::collections::BTreeSet;

const HALF_SPACE: u32 = 1 << 31;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrontierModel {
    peer_acked: u64,
    exposed: u64,
    appended: u64,
    ingress: u64,
    transport_next: Option<u64>,
    in_flight_attempt: Option<u64>,
    uncertain_writes: BTreeSet<u64>,
    capacity: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum InitError {
    AmbiguousCapacity,
    OutOfOrder,
    OutstandingExceedsCapacity,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AppendError {
    OrdinalExhausted,
    Capacity,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AttemptError {
    NotRetained,
    OutOfOrder,
    WriteInFlight,
    UncertainWritePending,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AttemptOutcome {
    NewlyExposed,
    Replay,
}

#[derive(Clone, Copy, Debug)]
pub enum AckKind {
    Live,
    Resume,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AckOutcome {
    Advanced { through: u64 },
    Duplicate,
    StaleIgnored,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AckError {
    HandledTooHigh { h: u32, send_count: u32 },
    StaleResume { h: u32 },
    Antipodal { h: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameWriteResult {
    Accepted,
    Unknown,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FrameWriteError {
    NotExposed,
    NoMatchingAttempt,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ResumeReconcileError {
    AckFloorMismatch { expected: u64, actual: u64 },
}

#[derive(Debug, PartialEq, Eq)]
pub enum IngressError {
    NonContiguous { expected: u64, actual: u64 },
    OrdinalExhausted,
}

impl FrontierModel {
    pub fn new(capacity: u32) -> Result<Self, InitError> {
        Self::from_ordinals(0, 0, 0, capacity)
    }

    /// Model a supplied contiguous history on an already-live transport. Its
    /// next emission cursor starts after `exposed`; callers must cross the
    /// explicit resume boundary before replaying older unacknowledged rows.
    /// This constructor does not validate that the history, retained rows,
    /// fence, or owner exist outside the model.
    pub fn from_ordinals(
        peer_acked: u64,
        exposed: u64,
        appended: u64,
        capacity: u32,
    ) -> Result<Self, InitError> {
        Self::from_ordinals_and_ingress(peer_acked, exposed, appended, 0, capacity)
    }

    pub fn from_ordinals_and_ingress(
        peer_acked: u64,
        exposed: u64,
        appended: u64,
        ingress: u64,
        capacity: u32,
    ) -> Result<Self, InitError> {
        if capacity >= HALF_SPACE {
            return Err(InitError::AmbiguousCapacity);
        }
        if peer_acked > exposed || exposed > appended {
            return Err(InitError::OutOfOrder);
        }
        if appended - peer_acked > u64::from(capacity) {
            return Err(InitError::OutstandingExceedsCapacity);
        }
        Ok(Self {
            peer_acked,
            exposed,
            appended,
            ingress,
            transport_next: exposed.checked_add(1),
            in_flight_attempt: None,
            uncertain_writes: BTreeSet::new(),
            capacity,
        })
    }

    pub fn peer_acked(&self) -> u64 {
        self.peer_acked
    }

    pub fn exposed(&self) -> u64 {
        self.exposed
    }

    pub fn appended(&self) -> u64 {
        self.appended
    }

    pub fn ingress(&self) -> u64 {
        self.ingress
    }

    pub fn has_uncertain_writes(&self) -> bool {
        !self.uncertain_writes.is_empty()
    }

    pub fn uncertain_writes(&self) -> impl Iterator<Item = u64> + '_ {
        self.uncertain_writes.iter().copied()
    }

    pub fn transport_next(&self) -> Option<u64> {
        self.transport_next
    }

    pub fn in_flight_attempt(&self) -> Option<u64> {
        self.in_flight_attempt
    }

    /// Retain the next replay ordinal without asserting any exposure.
    pub fn append(&mut self) -> Result<u64, AppendError> {
        let next = self
            .appended
            .checked_add(1)
            .ok_or(AppendError::OrdinalExhausted)?;
        if next - self.peer_acked > u64::from(self.capacity) {
            return Err(AppendError::Capacity);
        }
        self.appended = next;
        Ok(next)
    }

    /// Caller assumes a current authority and known committed fence. At most
    /// one attempt is in flight, and it must be the exact next ordinal on this
    /// transport. Previously exposed rows can be attempted only after an
    /// explicit new-transport resume boundary; same-transport retries are not
    /// allowed.
    pub fn record_assumed_committed_attempt(
        &mut self,
        ordinal: u64,
    ) -> Result<AttemptOutcome, AttemptError> {
        if self.in_flight_attempt.is_some() {
            return Err(AttemptError::WriteInFlight);
        }
        if !self.uncertain_writes.is_empty() {
            return Err(AttemptError::UncertainWritePending);
        }
        if ordinal <= self.peer_acked || ordinal > self.appended {
            return Err(AttemptError::NotRetained);
        }
        if self.transport_next != Some(ordinal) {
            return Err(AttemptError::OutOfOrder);
        }
        let outcome = if ordinal <= self.exposed {
            AttemptOutcome::Replay
        } else if ordinal - self.exposed == 1 {
            AttemptOutcome::NewlyExposed
        } else {
            return Err(AttemptError::OutOfOrder);
        };
        if outcome == AttemptOutcome::NewlyExposed {
            self.exposed = ordinal;
        }
        self.in_flight_attempt = Some(ordinal);
        Ok(outcome)
    }

    /// Classify relative to the acknowledged floor in the wrapping half-space.
    /// A forward value beyond exposure cannot delete rows. A forward distance
    /// greater than half the space is treated as backward, but may instead be a
    /// far-ahead value. An older ACK outside the backward half-space can alias
    /// an exposed forward ordinal and advance the floor; this model cannot
    /// identify its actual ordinal from the wire value.
    pub fn record_frame_write_result(
        &mut self,
        ordinal: u64,
        result: FrameWriteResult,
    ) -> Result<(), FrameWriteError> {
        if self.in_flight_attempt != Some(ordinal) {
            return Err(FrameWriteError::NoMatchingAttempt);
        }
        if ordinal <= self.peer_acked || ordinal > self.exposed {
            return Err(FrameWriteError::NotExposed);
        }
        self.in_flight_attempt = None;
        match result {
            // An accepted retry cannot disambiguate whether an earlier
            // uncertain attempt reached the peer. ACK or resume reconciliation
            // is required before subsequent exposure is safe.
            FrameWriteResult::Accepted => {
                self.transport_next = ordinal.checked_add(1);
            }
            FrameWriteResult::Unknown => {
                self.uncertain_writes.insert(ordinal);
            }
        }
        Ok(())
    }

    /// A completed resume supplies the authoritative peer ACK floor and makes
    /// every retained row above it part of the new stream's replay suffix.
    /// This is the only path besides an ACK that clears uncertain old-stream
    /// write outcomes; an Accepted result on the same stream is insufficient.
    pub(super) fn reconcile_uncertain_after_resume(
        &mut self,
        acknowledged: u64,
    ) -> Result<(), ResumeReconcileError> {
        if acknowledged != self.peer_acked {
            return Err(ResumeReconcileError::AckFloorMismatch {
                expected: self.peer_acked,
                actual: acknowledged,
            });
        }
        self.uncertain_writes.clear();
        self.in_flight_attempt = None;
        self.transport_next = self.peer_acked.checked_add(1);
        Ok(())
    }

    pub fn record_contiguous_ingress(&mut self, ordinal: u64) -> Result<(), IngressError> {
        let expected = self
            .ingress
            .checked_add(1)
            .ok_or(IngressError::OrdinalExhausted)?;
        if ordinal != expected {
            return Err(IngressError::NonContiguous {
                expected,
                actual: ordinal,
            });
        }
        self.ingress = ordinal;
        Ok(())
    }

    pub fn acknowledge(&mut self, h: u32, kind: AckKind) -> Result<AckOutcome, AckError> {
        let distance = h.wrapping_sub(self.peer_acked as u32);
        if distance == 0 {
            return Ok(AckOutcome::Duplicate);
        }
        if distance == HALF_SPACE {
            return Err(AckError::Antipodal { h });
        }
        if distance > HALF_SPACE {
            return match kind {
                AckKind::Live => Ok(AckOutcome::StaleIgnored),
                AckKind::Resume => Err(AckError::StaleResume { h }),
            };
        }
        if u64::from(distance) > self.exposed - self.peer_acked {
            return Err(AckError::HandledTooHigh {
                h,
                send_count: self.exposed as u32,
            });
        }
        self.peer_acked += u64::from(distance);
        self.uncertain_writes
            .retain(|ordinal| *ordinal > self.peer_acked);
        if self
            .in_flight_attempt
            .is_some_and(|ordinal| ordinal <= self.peer_acked)
        {
            self.in_flight_attempt = None;
        }
        let next_after_ack = self.peer_acked.checked_add(1);
        if let Some(next_after_ack) = next_after_ack {
            if self
                .transport_next
                .is_none_or(|transport_next| transport_next < next_after_ack)
            {
                self.transport_next = Some(next_after_ack);
            }
        } else {
            self.transport_next = None;
        }
        Ok(AckOutcome::Advanced {
            through: self.peer_acked,
        })
    }

    /// All retained rows, including appended rows with no write attempt yet.
    pub fn replay(&self) -> impl DoubleEndedIterator<Item = u64> + '_ {
        (self.peer_acked..self.appended).map(|ordinal| ordinal + 1)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use proptest::collection::vec;
    use proptest::prelude::{any, prop_assert, prop_assert_eq, prop_assume, proptest};

    use super::{
        AckError, AckKind, AckOutcome, AppendError, AttemptError, AttemptOutcome, FrameWriteResult,
        FrontierModel, IngressError, InitError, HALF_SPACE,
    };

    #[test]
    fn appended_but_unattempted_rows_remain_in_replay_and_cannot_be_acked() {
        let mut model = FrontierModel::new(4).unwrap();
        assert_eq!(
            model.acknowledge(0, AckKind::Live),
            Ok(AckOutcome::Duplicate)
        );
        assert_eq!(model.append(), Ok(1));
        assert_eq!(model.append(), Ok(2));
        assert_eq!(model.append(), Ok(3));
        assert_eq!(model.replay().collect::<Vec<_>>(), vec![1, 2, 3]);

        let before = model.clone();
        assert_eq!(
            model.acknowledge(1, AckKind::Live),
            Err(AckError::HandledTooHigh {
                h: 1,
                send_count: 0,
            })
        );
        assert_eq!(model, before);
        assert_eq!(
            model.record_assumed_committed_attempt(2),
            Err(AttemptError::OutOfOrder)
        );
        assert_eq!(model, before);

        assert_eq!(
            model.record_assumed_committed_attempt(1),
            Ok(AttemptOutcome::NewlyExposed)
        );
        assert_eq!(
            model.record_assumed_committed_attempt(2),
            Err(AttemptError::WriteInFlight)
        );
        model
            .record_frame_write_result(1, FrameWriteResult::Accepted)
            .unwrap();
        let before = model.clone();
        assert_eq!(
            model.record_assumed_committed_attempt(1),
            Err(AttemptError::OutOfOrder)
        );
        assert_eq!(model, before);
        assert_eq!(
            model.acknowledge(1, AckKind::Live),
            Ok(AckOutcome::Advanced { through: 1 })
        );
        assert_eq!(model.replay().collect::<Vec<_>>(), vec![2, 3]);
        let before = model.clone();
        assert_eq!(
            model.acknowledge(2, AckKind::Resume),
            Err(AckError::HandledTooHigh {
                h: 2,
                send_count: 1,
            })
        );
        assert_eq!(model, before);
        assert_eq!(
            model.record_assumed_committed_attempt(1),
            Err(AttemptError::NotRetained)
        );
        assert_eq!(model, before);
    }

    #[test]
    fn counters_cross_u32_max_without_discarding_unattempted_rows() {
        let start = u64::from(u32::MAX) - 1;
        let mut model = FrontierModel::from_ordinals(start, start, start, 4).unwrap();
        assert_eq!(model.append(), Ok(u64::from(u32::MAX)));
        assert_eq!(model.append(), Ok(u64::from(u32::MAX) + 1));
        assert_eq!(model.append(), Ok(u64::from(u32::MAX) + 2));
        assert_eq!(
            model.record_assumed_committed_attempt(u64::from(u32::MAX)),
            Ok(AttemptOutcome::NewlyExposed)
        );
        model
            .record_frame_write_result(u64::from(u32::MAX), FrameWriteResult::Accepted)
            .unwrap();
        assert_eq!(
            model.record_assumed_committed_attempt(u64::from(u32::MAX) + 1),
            Ok(AttemptOutcome::NewlyExposed)
        );
        model
            .record_frame_write_result(u64::from(u32::MAX) + 1, FrameWriteResult::Accepted)
            .unwrap();
        assert_eq!(
            model.acknowledge(u32::MAX, AckKind::Live),
            Ok(AckOutcome::Advanced {
                through: u64::from(u32::MAX),
            })
        );
        assert_eq!(
            model.acknowledge(0, AckKind::Resume),
            Ok(AckOutcome::Advanced {
                through: u64::from(u32::MAX) + 1,
            })
        );
        assert_eq!(
            model.acknowledge(0, AckKind::Live),
            Ok(AckOutcome::Duplicate)
        );
        assert_eq!(
            model.replay().collect::<Vec<_>>(),
            vec![u64::from(u32::MAX) + 2]
        );
        let before = model.clone();
        assert_eq!(
            model.acknowledge(1, AckKind::Live),
            Err(AckError::HandledTooHigh {
                h: 1,
                send_count: 0,
            })
        );
        assert_eq!(model, before);
    }

    #[test]
    fn resume_ack_from_a_prior_counter_cycle_can_alias_forward() {
        let next_cycle = u64::from(u32::MAX) + 1;
        let old_ordinal = 0u64;
        let mut old_ack =
            FrontierModel::from_ordinals(next_cycle - 1, next_cycle, next_cycle, 1).unwrap();
        let mut current_ack = old_ack.clone();
        assert_eq!(old_ack.replay().collect::<Vec<_>>(), vec![next_cycle]);
        assert_eq!(old_ordinal as u32, next_cycle as u32);

        let old_result = old_ack.acknowledge(old_ordinal as u32, AckKind::Resume);
        let current_result = current_ack.acknowledge(next_cycle as u32, AckKind::Resume);
        assert_eq!(
            old_result,
            Ok(AckOutcome::Advanced {
                through: next_cycle
            })
        );
        assert_eq!(old_result, current_result);
        assert_eq!(old_ack, current_ack);
        assert_eq!(old_ack.replay().next(), None);
    }

    #[test]
    fn duplicate_backward_window_antipodal_and_high_acknowledgements_never_delete_rows() {
        let mut model = FrontierModel::from_ordinals(9, 11, 12, 4).unwrap();
        for kind in [AckKind::Live, AckKind::Resume] {
            let before = model.clone();
            assert_eq!(model.acknowledge(9, kind), Ok(AckOutcome::Duplicate));
            assert_eq!(model, before);
            assert_eq!(
                model.acknowledge(9u32.wrapping_add(HALF_SPACE), kind),
                Err(AckError::Antipodal {
                    h: 9u32.wrapping_add(HALF_SPACE),
                })
            );
            assert_eq!(model, before);
            assert_eq!(
                model.acknowledge(12, kind),
                Err(AckError::HandledTooHigh {
                    h: 12,
                    send_count: 11,
                })
            );
            assert_eq!(model, before);
            assert_eq!(
                model.acknowledge(9u32.wrapping_add(HALF_SPACE - 1), kind),
                Err(AckError::HandledTooHigh {
                    h: 9u32.wrapping_add(HALF_SPACE - 1),
                    send_count: 11,
                })
            );
            assert_eq!(model, before);
        }
        let before = model.clone();
        assert_eq!(
            model.acknowledge(8, AckKind::Live),
            Ok(AckOutcome::StaleIgnored)
        );
        assert_eq!(model, before);
        assert_eq!(
            model.acknowledge(8, AckKind::Resume),
            Err(AckError::StaleResume { h: 8 })
        );
        assert_eq!(model, before);
        assert_eq!(model.replay().collect::<Vec<_>>(), vec![10, 11, 12]);
    }

    #[test]
    fn capacity_half_space_and_u64_boundaries_refuse_before_mutation() {
        assert_eq!(
            FrontierModel::new(HALF_SPACE),
            Err(InitError::AmbiguousCapacity)
        );
        assert_eq!(
            FrontierModel::from_ordinals(2, 1, 3, 4),
            Err(InitError::OutOfOrder)
        );
        assert_eq!(
            FrontierModel::from_ordinals(1, 3, 2, 4),
            Err(InitError::OutOfOrder)
        );
        assert_eq!(
            FrontierModel::from_ordinals(0, 0, u64::from(HALF_SPACE), HALF_SPACE - 1),
            Err(InitError::OutstandingExceedsCapacity)
        );
        let mut model = FrontierModel::from_ordinals(
            0,
            u64::from(HALF_SPACE) - 1,
            u64::from(HALF_SPACE) - 1,
            HALF_SPACE - 1,
        )
        .unwrap();
        let before = model.clone();
        assert_eq!(model.append(), Err(AppendError::Capacity));
        assert_eq!(model, before);
        assert_eq!(model.replay().next(), Some(1));
        assert_eq!(model.replay().next_back(), Some(u64::from(HALF_SPACE) - 1));
        assert_eq!(
            model.acknowledge(HALF_SPACE - 1, AckKind::Live),
            Ok(AckOutcome::Advanced {
                through: u64::from(HALF_SPACE) - 1,
            })
        );
        assert_eq!(model.append(), Ok(u64::from(HALF_SPACE)));

        let mut zero_capacity = FrontierModel::new(0).unwrap();
        let before = zero_capacity.clone();
        assert_eq!(zero_capacity.append(), Err(AppendError::Capacity));
        assert_eq!(zero_capacity, before);

        let mut exhausted =
            FrontierModel::from_ordinals(u64::MAX - 1, u64::MAX, u64::MAX, 1).unwrap();
        assert_eq!(exhausted.replay().collect::<Vec<_>>(), vec![u64::MAX]);
        assert_eq!(
            exhausted.acknowledge(u32::MAX, AckKind::Live),
            Ok(AckOutcome::Advanced { through: u64::MAX })
        );
        let before = exhausted.clone();
        assert_eq!(exhausted.append(), Err(AppendError::OrdinalExhausted));
        assert_eq!(exhausted, before);
        assert_eq!(exhausted.replay().next(), None);
    }

    #[test]
    fn attempts_never_skip_or_expose_a_non_retained_row() {
        let mut model = FrontierModel::from_ordinals(100, 101, 103, 3).unwrap();
        for (ordinal, expected) in [
            (100, AttemptError::NotRetained),
            (103, AttemptError::OutOfOrder),
            (104, AttemptError::NotRetained),
        ] {
            let before = model.clone();
            assert_eq!(
                model.record_assumed_committed_attempt(ordinal),
                Err(expected)
            );
            assert_eq!(model, before);
        }
        assert_eq!(
            model.record_assumed_committed_attempt(101),
            Err(AttemptError::OutOfOrder),
            "an already-exposed row is not resent on the same transport"
        );
        model.reconcile_uncertain_after_resume(100).unwrap();
        assert_eq!(
            model.record_assumed_committed_attempt(101),
            Ok(AttemptOutcome::Replay)
        );
        model
            .record_frame_write_result(101, FrameWriteResult::Accepted)
            .unwrap();
        assert_eq!(
            model.record_assumed_committed_attempt(102),
            Ok(AttemptOutcome::NewlyExposed)
        );
        model
            .record_frame_write_result(102, FrameWriteResult::Accepted)
            .unwrap();
        assert_eq!(
            model.record_assumed_committed_attempt(103),
            Ok(AttemptOutcome::NewlyExposed)
        );
        model
            .record_frame_write_result(103, FrameWriteResult::Accepted)
            .unwrap();
        assert_eq!(model.replay().collect::<Vec<_>>(), vec![101, 102, 103]);
    }

    #[test]
    fn uncertain_write_keeps_replay_and_repair_until_ack_or_resume() {
        let mut model = FrontierModel::new(4).unwrap();
        assert_eq!(model.append(), Ok(1));
        assert_eq!(model.append(), Ok(2));
        assert_eq!(model.replay().collect::<Vec<_>>(), vec![1, 2]);

        assert_eq!(
            model.record_assumed_committed_attempt(1),
            Ok(AttemptOutcome::NewlyExposed)
        );
        assert_eq!(model.exposed(), 1);
        assert_eq!(
            model.record_assumed_committed_attempt(2),
            Err(AttemptError::WriteInFlight),
            "ordinal 2 cannot be in flight before ordinal 1 has a result"
        );
        assert_eq!(model.exposed(), 1);
        assert_eq!(
            model.record_assumed_committed_attempt(1),
            Err(AttemptError::WriteInFlight)
        );
        assert_eq!(model.replay().collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(
            model.acknowledge(2, AckKind::Live),
            Err(AckError::HandledTooHigh {
                h: 2,
                send_count: 1
            })
        );
        assert_eq!(model.replay().collect::<Vec<_>>(), vec![1, 2]);

        model
            .record_frame_write_result(1, FrameWriteResult::Unknown)
            .unwrap();
        assert_eq!(model.uncertain_writes().collect::<Vec<_>>(), vec![1]);
        assert_eq!(
            model.record_assumed_committed_attempt(2),
            Err(AttemptError::UncertainWritePending)
        );
        let before = model.clone();
        assert_eq!(
            model.record_frame_write_result(1, FrameWriteResult::Accepted),
            Err(super::FrameWriteError::NoMatchingAttempt)
        );
        assert_eq!(model, before);
        assert!(model.has_uncertain_writes());
        assert_eq!(model.exposed(), 1);

        assert_eq!(
            model.acknowledge(1, AckKind::Live),
            Ok(AckOutcome::Advanced { through: 1 })
        );
        assert!(!model.has_uncertain_writes());
        assert_eq!(
            model.record_assumed_committed_attempt(2),
            Ok(AttemptOutcome::NewlyExposed)
        );
        assert_eq!(
            model.record_frame_write_result(2, FrameWriteResult::Unknown),
            Ok(())
        );

        assert_eq!(
            model.acknowledge(2, AckKind::Live),
            Ok(AckOutcome::Advanced { through: 2 })
        );
        assert_eq!(model.peer_acked(), 2);
        assert!(!model.has_uncertain_writes());
        assert_eq!(model.replay().next(), None);
    }

    #[test]
    fn uncertain_prefix_blocks_later_exposure_and_false_acknowledgement() {
        let mut model = FrontierModel::new(4).unwrap();
        assert_eq!(model.append(), Ok(1));
        assert_eq!(model.append(), Ok(2));
        assert_eq!(
            model.record_assumed_committed_attempt(1),
            Ok(AttemptOutcome::NewlyExposed)
        );
        model
            .record_frame_write_result(1, FrameWriteResult::Unknown)
            .unwrap();

        let before = model.clone();
        assert_eq!(
            model.record_assumed_committed_attempt(2),
            Err(AttemptError::UncertainWritePending)
        );
        assert_eq!(model, before);
        assert_eq!(
            model.acknowledge(1, AckKind::Live),
            Ok(AckOutcome::Advanced { through: 1 })
        );
        assert_eq!(model.peer_acked(), 1);
        assert_eq!(model.replay().collect::<Vec<_>>(), vec![2]);

        let mut accepted_first = FrontierModel::new(4).unwrap();
        accepted_first.append().unwrap();
        accepted_first.append().unwrap();
        accepted_first.record_assumed_committed_attempt(1).unwrap();
        accepted_first
            .record_frame_write_result(1, FrameWriteResult::Accepted)
            .unwrap();
        let before = accepted_first.clone();
        assert_eq!(
            accepted_first.record_assumed_committed_attempt(1),
            Err(AttemptError::OutOfOrder),
            "an accepted row cannot be resent on the same transport"
        );
        assert_eq!(accepted_first, before);
        assert_eq!(
            accepted_first.acknowledge(2, AckKind::Live),
            Err(AckError::HandledTooHigh {
                h: 2,
                send_count: 1
            })
        );
        assert_eq!(accepted_first.replay().collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn resume_boundary_reconciles_unknown_writes_only_at_exact_ack_floor() {
        let mut model = FrontierModel::new(4).unwrap();
        model.append().unwrap();
        model.append().unwrap();
        model
            .record_assumed_committed_attempt(1)
            .expect("first exposure");
        model
            .record_frame_write_result(1, FrameWriteResult::Unknown)
            .unwrap();

        assert_eq!(
            model.reconcile_uncertain_after_resume(1),
            Err(super::ResumeReconcileError::AckFloorMismatch {
                expected: 0,
                actual: 1,
            })
        );
        assert!(model.has_uncertain_writes());
        assert_eq!(
            model.acknowledge(0, AckKind::Resume),
            Ok(AckOutcome::Duplicate)
        );
        assert!(model.has_uncertain_writes());
        model.reconcile_uncertain_after_resume(0).unwrap();
        assert!(!model.has_uncertain_writes());
        assert_eq!(model.replay().collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(
            model.record_assumed_committed_attempt(2),
            Err(AttemptError::OutOfOrder),
            "the new transport cannot skip the first replay row"
        );
        assert_eq!(
            model.record_assumed_committed_attempt(1),
            Ok(AttemptOutcome::Replay)
        );
        model
            .record_frame_write_result(1, FrameWriteResult::Accepted)
            .unwrap();
        assert_eq!(
            model.record_assumed_committed_attempt(2),
            Ok(AttemptOutcome::NewlyExposed)
        );

        let mut late_result = FrontierModel::new(2).unwrap();
        late_result.append().unwrap();
        late_result.append().unwrap();
        late_result.record_assumed_committed_attempt(1).unwrap();
        assert_eq!(
            late_result.acknowledge(1, AckKind::Live),
            Ok(AckOutcome::Advanced { through: 1 })
        );
        assert_eq!(late_result.in_flight_attempt(), None);
        let before = late_result.clone();
        assert_eq!(
            late_result.record_frame_write_result(1, FrameWriteResult::Unknown),
            Err(super::FrameWriteError::NoMatchingAttempt),
            "a late Unknown after the ACK cannot re-freeze the resolved row"
        );
        assert_eq!(late_result, before);
        assert!(!late_result.has_uncertain_writes());
        assert_eq!(
            late_result.record_assumed_committed_attempt(2),
            Ok(AttemptOutcome::NewlyExposed)
        );
    }

    #[test]
    fn contiguous_ingress_rejects_gaps_and_never_wraps() {
        let mut model = FrontierModel::new(1).unwrap();
        let before = model.clone();
        assert_eq!(
            model.record_contiguous_ingress(2),
            Err(IngressError::NonContiguous {
                expected: 1,
                actual: 2,
            })
        );
        assert_eq!(model, before);
        assert_eq!(model.record_contiguous_ingress(1), Ok(()));
        assert_eq!(model.ingress(), 1);

        let mut exhausted = FrontierModel::from_ordinals_and_ingress(0, 0, 0, u64::MAX, 1).unwrap();
        let before = exhausted.clone();
        assert_eq!(
            exhausted.record_contiguous_ingress(0),
            Err(IngressError::OrdinalExhausted)
        );
        assert_eq!(exhausted, before);
    }

    proptest! {
        #[test]
        fn mixed_operations_preserve_frontiers_and_replay(
            start in any::<u64>(),
            operations in vec((0u8..8, any::<u8>()), 0..150),
        ) {
            prop_assume!(start <= u64::MAX - 150);
            let mut model = FrontierModel::from_ordinals(start, start, start, 12).unwrap();
            let mut retained = VecDeque::new();
            for (operation, value) in operations {
                let before = model.clone();
                match operation {
                    0 | 1 => match model.append() {
                        Ok(ordinal) => retained.push_back(ordinal),
                        Err(AppendError::Capacity) => prop_assert_eq!(&model, &before),
                        Err(AppendError::OrdinalExhausted) => unreachable!(),
                    },
                    2 => {
                        if let Some(ordinal) = model.in_flight_attempt {
                            prop_assert_eq!(
                                model.record_frame_write_result(ordinal, FrameWriteResult::Accepted),
                                Ok(()),
                            );
                        } else if let Some(ordinal) = model.transport_next {
                            if ordinal <= model.appended {
                                prop_assert!(model.record_assumed_committed_attempt(ordinal).is_ok());
                            } else {
                                prop_assert_eq!(
                                    model.record_assumed_committed_attempt(ordinal),
                                    Err(AttemptError::NotRetained),
                                );
                                prop_assert_eq!(&model, &before);
                            }
                        }
                    }
                    3 => {
                        let available = model.exposed - model.peer_acked;
                        let distance = u64::from(value) % (available + 1);
                        let h = (model.peer_acked + distance) as u32;
                        let result = model.acknowledge(h, AckKind::Live).unwrap();
                        if distance == 0 {
                            prop_assert_eq!(result, AckOutcome::Duplicate);
                        } else {
                            prop_assert_eq!(result, AckOutcome::Advanced {
                                through: before.peer_acked + distance,
                            });
                            for _ in 0..distance {
                                retained.pop_front();
                            }
                        }
                    }
                    4 => {
                        let h = (model.exposed as u32).wrapping_add(1);
                        prop_assert_eq!(
                            model.acknowledge(h, AckKind::Live),
                            Err(AckError::HandledTooHigh { h, send_count: model.exposed as u32 }),
                        );
                        prop_assert_eq!(&model, &before);
                    }
                    5 => {
                        let h = (model.peer_acked as u32).wrapping_sub(1);
                        prop_assert_eq!(model.acknowledge(h, AckKind::Live), Ok(AckOutcome::StaleIgnored));
                        prop_assert_eq!(&model, &before);
                        prop_assert_eq!(model.acknowledge(h, AckKind::Resume), Err(AckError::StaleResume { h }));
                        prop_assert_eq!(&model, &before);
                    }
                    6 => {
                        let h = (model.peer_acked as u32).wrapping_add(HALF_SPACE);
                        prop_assert_eq!(model.acknowledge(h, AckKind::Resume), Err(AckError::Antipodal { h }));
                        prop_assert_eq!(&model, &before);
                    }
                    _ => {
                        let ordinal = model.appended + 1;
                        let expected = if model.in_flight_attempt.is_some() {
                            AttemptError::WriteInFlight
                        } else {
                            AttemptError::NotRetained
                        };
                        prop_assert_eq!(model.record_assumed_committed_attempt(ordinal), Err(expected));
                        prop_assert_eq!(&model, &before);
                    }
                }
                prop_assert!(before.peer_acked <= model.peer_acked);
                prop_assert!(before.exposed <= model.exposed);
                prop_assert!(before.appended <= model.appended);
                prop_assert!(model.peer_acked <= model.exposed && model.exposed <= model.appended);
                prop_assert!(model.appended - model.peer_acked <= u64::from(model.capacity));
                prop_assert!(model.appended - model.peer_acked < u64::from(HALF_SPACE));
                prop_assert!(model.uncertain_writes.len() <= 1);
                if let Some(in_flight) = model.in_flight_attempt {
                    prop_assert_eq!(model.transport_next, Some(in_flight));
                }
                prop_assert_eq!(model.replay().collect::<Vec<_>>(), retained.iter().copied().collect::<Vec<_>>());
            }
        }
    }
}
