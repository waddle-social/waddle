// Compile the production modules directly. The transport and database crates
// are outside the input/output contract of these functions.
#[path = "../../../../crates/waddle-xmpp/src/stream_management/durable/counter.rs"]
pub mod counter;
#[path = "../../../../crates/waddle-xmpp/src/stream_management/durable/lifecycle.rs"]
pub mod lifecycle;

#[cfg(kani)]
mod proofs {
    use super::counter::{AckKind, HandledDecision, HandledError, OutboundWindow, SmOrdinal};
    use super::lifecycle::{
        transition, DurableSmEvent as Event, DurableSmState as State, TerminalReason,
        TransitionError,
    };

    const WIRE_PERIOD: u64 = 1 << 32;
    const HALF_PERIOD: u64 = WIRE_PERIOD / 2;

    fn window() -> (OutboundWindow, u64, u64, u64) {
        let acked: u64 = kani::any();
        let exposed: u64 = kani::any();
        let appended: u64 = kani::any();
        kani::assume(acked <= exposed && exposed <= appended && appended <= i64::MAX as u64);
        kani::assume(appended - acked < HALF_PERIOD);

        let window = OutboundWindow::new(
            SmOrdinal::new(acked).unwrap(),
            SmOrdinal::new(exposed).unwrap(),
            SmOrdinal::new(appended).unwrap(),
        )
        .unwrap();
        (window, acked, exposed, appended)
    }

    // Work in absolute ordinals. Pick the first ordinal at or beyond the
    // floor whose residue in a 32-bit cycle is h.
    fn lift_wire(acked: u64, h: u32) -> u64 {
        let in_cycle = (acked / WIRE_PERIOD) * WIRE_PERIOD + u64::from(h);
        if in_cycle < acked {
            in_cycle + WIRE_PERIOD
        } else {
            in_cycle
        }
    }

    #[kani::proof]
    fn acknowledgments_match_absolute_ordinal_contract() {
        let (window, acked, exposed, appended) = window();
        let h: u32 = kani::any();
        let resume: bool = kani::any();
        let kind = if resume {
            AckKind::Resume
        } else {
            AckKind::Live
        };
        let ordinal = lift_wire(acked, h);
        let result = window.acknowledge(h, kind);

        kani::cover!(acked == exposed && exposed == appended && ordinal == acked);
        kani::cover!(acked % WIRE_PERIOD > u64::from(h) && ordinal <= exposed);
        kani::cover!(ordinal == acked + HALF_PERIOD);
        kani::cover!(exposed < ordinal && ordinal <= appended);
        kani::cover!(resume && ordinal > acked + HALF_PERIOD);
        kani::cover!(!resume && ordinal > acked + HALF_PERIOD);
        kani::cover!(appended == i64::MAX as u64);

        let expected = if ordinal == acked {
            Ok(HandledDecision::Unchanged)
        } else if ordinal <= exposed {
            Ok(HandledDecision::Advanced {
                through: SmOrdinal::new(ordinal).unwrap(),
                released: u32::try_from(ordinal - acked).unwrap(),
            })
        } else if ordinal < acked + HALF_PERIOD {
            Err(HandledError::TooHigh {
                h,
                ceiling: (exposed % WIRE_PERIOD) as u32,
            })
        } else if ordinal == acked + HALF_PERIOD {
            Err(HandledError::Ambiguous { h })
        } else if resume {
            Err(HandledError::Behind {
                h,
                acked: (acked % WIRE_PERIOD) as u32,
            })
        } else {
            Ok(HandledDecision::IgnoredStale)
        };
        assert_eq!(result.map(|(_, decision)| decision), expected);

        if let Ok((next, _)) = result {
            let floor = if ordinal <= exposed { ordinal } else { acked };
            assert_eq!(next.acked().value(), floor);
            assert_eq!(next.exposure_ceiling().value(), exposed);
            assert_eq!(next.appended().value(), appended);
            assert_eq!(next.replay().after().value(), floor);
            assert_eq!(next.replay().through().value(), appended);
        }
    }

    #[kani::proof]
    fn replay_wire_sequence_identifies_exactly_one_retained_ordinal() {
        let (window, acked, _, appended) = window();
        let h: u32 = kani::any();
        let ordinal = lift_wire(acked, h);
        let replay = window.replay();
        let expected = (acked < ordinal && ordinal <= appended).then_some(ordinal);
        assert_eq!(replay.ordinal_for_wire(h).map(SmOrdinal::value), expected);
    }

    #[kani::proof]
    fn resume_and_expiry_obey_lifecycle_contract() {
        let states = [
            State::Enabling,
            State::Live,
            State::Detached,
            State::TransferHydrating,
            State::Terminal(TerminalReason::EnableAbandoned),
            State::Terminal(TerminalReason::Expired),
            State::Terminal(TerminalReason::Replaced),
            State::Terminal(TerminalReason::Closed),
        ];
        let events = [
            Event::EnableCommitted,
            Event::EnableAbandoned,
            Event::Detach,
            Event::ResumeClaimed,
            Event::ResumeCompleted,
            Event::OwnerLost,
            Event::Expire,
            Event::Replace,
            Event::Close,
        ];
        let state_index: usize = kani::any_where(|i| *i < states.len());
        let event_index: usize = kani::any_where(|i| *i < events.len());
        let state = states[state_index];
        let event = events[event_index];
        let next = transition(state, event);

        if let State::Terminal(reason) = state {
            assert_eq!(next, Err(TransitionError::Terminal(reason)));
        }

        match event {
            Event::ResumeClaimed => {
                let eligible = matches!(
                    state,
                    State::Live | State::Detached | State::TransferHydrating
                );
                assert_eq!(next.ok(), eligible.then_some(State::TransferHydrating));
            }
            Event::ResumeCompleted => {
                assert_eq!(
                    next.ok(),
                    (state == State::TransferHydrating).then_some(State::Live)
                );
            }
            Event::Expire => {
                assert_eq!(
                    next.ok(),
                    (state == State::Detached).then_some(State::Terminal(TerminalReason::Expired))
                );
            }
            _ => {}
        }

        if let Ok(next) = next {
            assert!(
                !next.exposes_frames()
                    || matches!(event, Event::EnableCommitted | Event::ResumeCompleted)
            );
            assert_eq!(next.exposes_frames(), next == State::Live);
        }
    }
}
