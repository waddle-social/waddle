use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use proptest::prelude::{any, prop_assert, prop_assert_eq, proptest};
use uuid::Uuid;

use super::{
    authorize_resume, check_owner_fence, transition, AckKind, AppendError, DurableSessionView,
    DurableSmEvent, DurableSmState, Exposure, ExposureBounds, FenceError, HandledDecision,
    HandledError, OutboundWindow, OwnerFence, PrincipalStanding, ResumeAttempt, ResumeDenial,
    SmOrdinal, SmResumeToken, TerminalReason, TransitionError, WindowError, MAX_OUTSTANDING_LIMIT,
};
use crate::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};
use crate::ownership::{
    BindingRef, ConnectionRef, NodeLeaseIdentity, SessionGeneration, SmSessionKey,
};

const STATES: [DurableSmState; 8] = [
    DurableSmState::Enabling,
    DurableSmState::Live,
    DurableSmState::Detached,
    DurableSmState::TransferHydrating,
    DurableSmState::Terminal(TerminalReason::EnableAbandoned),
    DurableSmState::Terminal(TerminalReason::Expired),
    DurableSmState::Terminal(TerminalReason::Replaced),
    DurableSmState::Terminal(TerminalReason::Closed),
];

const EVENTS: [DurableSmEvent; 9] = [
    DurableSmEvent::EnableCommitted,
    DurableSmEvent::EnableAbandoned,
    DurableSmEvent::Detach,
    DurableSmEvent::ResumeClaimed,
    DurableSmEvent::ResumeCompleted,
    DurableSmEvent::OwnerLost,
    DurableSmEvent::Expire,
    DurableSmEvent::Replace,
    DurableSmEvent::Close,
];

fn ordinal(value: u64) -> SmOrdinal {
    SmOrdinal::new(value).expect("test ordinal fits")
}

fn window(acked: u64, ceiling: u64, appended: u64) -> OutboundWindow {
    OutboundWindow::new(ordinal(acked), ordinal(ceiling), ordinal(appended)).expect("valid window")
}

fn principal(context: u128, epoch: u64, jid: &str) -> AuthenticatedPrincipalRef {
    AuthenticatedPrincipalRef::new(
        jid.parse().expect("bare jid"),
        AuthContextId::new(Uuid::from_u128(context)),
        AuthContextVersion::INITIAL,
        PrincipalAuthEpoch::new(epoch),
    )
}

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + seconds, 0)
        .single()
        .expect("valid time")
}

fn binding() -> BindingRef {
    BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::local_process()))
}

fn generation(value: u64) -> SessionGeneration {
    SessionGeneration::new(value).expect("valid generation")
}

#[test]
fn transition_table_matches_the_decision_record() {
    use DurableSmEvent as Event;
    use DurableSmState as State;

    let accepted = [
        (State::Enabling, Event::EnableCommitted, State::Live),
        (
            State::Enabling,
            Event::EnableAbandoned,
            State::Terminal(TerminalReason::EnableAbandoned),
        ),
        (State::Live, Event::Detach, State::Detached),
        (State::Live, Event::OwnerLost, State::Detached),
        (State::Live, Event::ResumeClaimed, State::TransferHydrating),
        (
            State::Live,
            Event::Replace,
            State::Terminal(TerminalReason::Replaced),
        ),
        (
            State::Live,
            Event::Close,
            State::Terminal(TerminalReason::Closed),
        ),
        (
            State::Detached,
            Event::ResumeClaimed,
            State::TransferHydrating,
        ),
        (
            State::Detached,
            Event::Expire,
            State::Terminal(TerminalReason::Expired),
        ),
        (
            State::Detached,
            Event::Replace,
            State::Terminal(TerminalReason::Replaced),
        ),
        (
            State::Detached,
            Event::Close,
            State::Terminal(TerminalReason::Closed),
        ),
        (
            State::TransferHydrating,
            Event::ResumeCompleted,
            State::Live,
        ),
        (State::TransferHydrating, Event::Detach, State::Detached),
        (State::TransferHydrating, Event::OwnerLost, State::Detached),
        (
            State::TransferHydrating,
            Event::ResumeClaimed,
            State::TransferHydrating,
        ),
        (
            State::TransferHydrating,
            Event::Replace,
            State::Terminal(TerminalReason::Replaced),
        ),
        (
            State::TransferHydrating,
            Event::Close,
            State::Terminal(TerminalReason::Closed),
        ),
    ];

    for state in STATES {
        for event in EVENTS {
            let expected = accepted
                .iter()
                .find(|(from, on, _)| *from == state && *on == event)
                .map(|(_, _, to)| *to);
            match (transition(state, event), expected, state) {
                (Ok(actual), Some(expected), _) => assert_eq!(actual, expected),
                (Err(TransitionError::Terminal(reason)), None, State::Terminal(stored)) => {
                    assert_eq!(reason, stored);
                }
                (
                    Err(TransitionError::NotApplicable {
                        state: from,
                        event: on,
                    }),
                    None,
                    _,
                ) => {
                    assert_eq!((from, on), (state, event));
                }
                (actual, expected, _) => {
                    panic!("{state:?} on {event:?}: got {actual:?}, expected {expected:?}")
                }
            }
        }
    }
}

#[test]
fn only_live_exposes_frames_and_only_owned_states_have_an_owner() {
    for state in STATES {
        assert_eq!(state.exposes_frames(), state == DurableSmState::Live);
        assert_eq!(
            state.has_owner(),
            matches!(
                state,
                DurableSmState::Live | DurableSmState::TransferHydrating
            )
        );
    }
}

#[test]
fn window_rejects_unordered_and_overwide_ordinals() {
    assert_eq!(
        OutboundWindow::new(ordinal(2), ordinal(1), ordinal(3)),
        Err(WindowError::Unordered)
    );
    assert_eq!(
        OutboundWindow::new(ordinal(1), ordinal(4), ordinal(3)),
        Err(WindowError::Unordered)
    );
    assert_eq!(
        OutboundWindow::new(
            ordinal(0),
            ordinal(0),
            ordinal(u64::from(MAX_OUTSTANDING_LIMIT) + 1)
        ),
        Err(WindowError::TooWide)
    );
    assert_eq!(SmOrdinal::new(i64::MAX as u64 + 1), None);
}

#[test]
fn handled_count_table() {
    // acked 10, exposed through 13, appended through 15.
    let stored = window(10, 13, 15);
    let cases = [
        (10, AckKind::Live, Ok(HandledDecision::Unchanged)),
        (
            11,
            AckKind::Live,
            Ok(HandledDecision::Advanced {
                through: ordinal(11),
                released: 1,
            }),
        ),
        (
            13,
            AckKind::Resume,
            Ok(HandledDecision::Advanced {
                through: ordinal(13),
                released: 3,
            }),
        ),
        // Appended but never exposed: the peer cannot have handled it.
        (
            14,
            AckKind::Live,
            Err(HandledError::TooHigh { h: 14, ceiling: 13 }),
        ),
        (
            14,
            AckKind::Resume,
            Err(HandledError::TooHigh { h: 14, ceiling: 13 }),
        ),
        (
            10 + (1 << 31),
            AckKind::Live,
            Err(HandledError::Ambiguous { h: 10 + (1 << 31) }),
        ),
        (9, AckKind::Live, Ok(HandledDecision::IgnoredStale)),
        (
            9,
            AckKind::Resume,
            Err(HandledError::Behind { h: 9, acked: 10 }),
        ),
    ];
    for (h, kind, expected) in cases {
        assert_eq!(stored.classify_handled(h, kind), expected, "h={h} {kind:?}");
    }
}

#[test]
fn handled_count_crosses_the_counter_wrap() {
    let floor = u64::from(u32::MAX) - 1;
    let stored = window(floor, floor + 4, floor + 4);
    // Wire values: floor = 0xFFFF_FFFE, then 0xFFFF_FFFF, 0, 1, 2.
    let (advanced, decision) = stored
        .acknowledge(1, AckKind::Live)
        .expect("forward over wrap");
    assert_eq!(
        decision,
        HandledDecision::Advanced {
            through: ordinal(floor + 3),
            released: 3
        }
    );
    assert_eq!(advanced.acked().wire(), 1);
    assert_eq!(
        stored.classify_handled(3, AckKind::Live),
        Err(HandledError::TooHigh { h: 3, ceiling: 2 })
    );
    assert_eq!(
        stored.classify_handled(u32::MAX - 2, AckKind::Live),
        Ok(HandledDecision::IgnoredStale)
    );
}

#[test]
fn append_respects_bounds_and_exposure() {
    let bounds = ExposureBounds::new(2, 10).expect("bounds");
    let (live, first) = OutboundWindow::EMPTY
        .append(Exposure::Exposed, bounds, 0, 4)
        .expect("first frame");
    assert_eq!((first, live.exposure_ceiling()), (ordinal(1), ordinal(1)));

    let (retained, second) = live
        .append(Exposure::Retained, bounds, 4, 4)
        .expect("second frame");
    assert_eq!(second, ordinal(2));
    assert_eq!(retained.exposure_ceiling(), ordinal(1));
    assert_eq!(
        retained.unexposed().ordinals().collect::<Vec<_>>(),
        vec![ordinal(2)]
    );

    assert_eq!(
        retained.append(Exposure::Exposed, bounds, 8, 1),
        Err(AppendError::OutstandingBound)
    );
    assert_eq!(
        live.append(Exposure::Exposed, bounds, 4, 7),
        Err(AppendError::ByteBound)
    );
    assert!(ExposureBounds::new(0, 1).is_err());
    assert!(ExposureBounds::new(MAX_OUTSTANDING_LIMIT + 1, 1).is_err());
    assert!(ExposureBounds::new(1, 0).is_err());
}

#[test]
fn replay_covers_unexposed_frames_and_maps_stored_sequences() {
    let floor = u64::from(u32::MAX);
    let stored = window(floor, floor + 1, floor + 3);
    let replay = stored.replay();
    assert_eq!(replay.len(), 3);
    assert_eq!(
        replay.ordinals().map(SmOrdinal::wire).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(replay.ordinal_for_wire(0), Some(ordinal(floor + 1)));
    assert_eq!(replay.ordinal_for_wire(2), Some(ordinal(floor + 3)));
    assert_eq!(replay.ordinal_for_wire(u32::MAX), None);
    assert_eq!(replay.ordinal_for_wire(3), None);
    assert_eq!(stored.expose_all().exposure_ceiling(), ordinal(floor + 3));
}

#[test]
fn owner_fence_requires_the_exact_generation_and_binding() {
    let session = SmSessionKey::fresh();
    let stored = OwnerFence {
        session,
        generation: generation(3),
        binding: binding(),
    };
    assert_eq!(check_owner_fence(&stored, &stored.clone()), Ok(()));
    assert_eq!(
        check_owner_fence(
            &stored,
            &OwnerFence {
                generation: generation(2),
                ..stored.clone()
            }
        ),
        Err(FenceError::StaleGeneration {
            stored: generation(3),
            presented: generation(2)
        })
    );
    assert_eq!(
        check_owner_fence(
            &stored,
            &OwnerFence {
                generation: generation(4),
                ..stored.clone()
            }
        ),
        Err(FenceError::UnknownGeneration {
            stored: generation(3),
            presented: generation(4)
        })
    );
    assert_eq!(
        check_owner_fence(
            &stored,
            &OwnerFence {
                binding: binding(),
                ..stored.clone()
            }
        ),
        Err(FenceError::BindingMismatch)
    );
    assert_eq!(
        check_owner_fence(
            &stored,
            &OwnerFence {
                session: SmSessionKey::fresh(),
                ..stored.clone()
            }
        ),
        Err(FenceError::OtherSession)
    );
}

#[test]
fn resume_authorization_table() {
    let stored = principal(1, 7, "alice@example.test");
    let view = |state, detached_at| DurableSessionView {
        state,
        generation: generation(1),
        principal: &stored,
        window: window(10, 13, 15),
        detached_at,
        max_resume: Duration::from_secs(300),
    };
    let same = stored.clone();
    let new_context = principal(2, 7, "alice@example.test");
    let other_epoch = principal(1, 8, "alice@example.test");
    let other_jid = principal(1, 7, "mallory@example.test");
    let attempt = |presented, stored_standing, h, now| ResumeAttempt {
        presented,
        stored_standing,
        h,
        now,
    };

    let granted = authorize_resume(
        &view(DurableSmState::Detached, Some(at(0))),
        &attempt(&same, PrincipalStanding::Current, 12, at(299)),
    )
    .expect("authorized resume");
    assert_eq!(granted.generation, generation(2));
    assert_eq!(granted.state, DurableSmState::TransferHydrating);
    assert_eq!(granted.window, window(12, 13, 15));
    assert_eq!(
        granted.replay.ordinals().collect::<Vec<_>>(),
        vec![ordinal(13), ordinal(14), ordinal(15)]
    );

    // A live session with no detach time is resumable: the generation, not
    // the old transport's close, fences its previous owner.
    for state in [DurableSmState::Live, DurableSmState::TransferHydrating] {
        assert!(authorize_resume(
            &view(state, None),
            &attempt(&new_context, PrincipalStanding::Current, 10, at(10_000)),
        )
        .is_ok());
    }

    let denials = [
        (
            view(DurableSmState::Terminal(TerminalReason::Closed), None),
            attempt(&same, PrincipalStanding::Current, 10, at(0)),
            ResumeDenial::Terminal(TerminalReason::Closed),
        ),
        (
            view(DurableSmState::Detached, Some(at(0))),
            attempt(&same, PrincipalStanding::Current, 10, at(300)),
            ResumeDenial::Expired,
        ),
        (
            view(DurableSmState::Detached, Some(at(0))),
            attempt(&same, PrincipalStanding::Missing, 10, at(1)),
            ResumeDenial::PrincipalUnavailable,
        ),
        (
            view(DurableSmState::Detached, Some(at(0))),
            attempt(&same, PrincipalStanding::Diverged, 10, at(1)),
            ResumeDenial::PrincipalDiverged,
        ),
        (
            view(DurableSmState::Detached, Some(at(0))),
            attempt(&other_epoch, PrincipalStanding::Current, 10, at(1)),
            ResumeDenial::PrincipalMismatch,
        ),
        (
            view(DurableSmState::Detached, Some(at(0))),
            attempt(&other_jid, PrincipalStanding::Current, 10, at(1)),
            ResumeDenial::PrincipalMismatch,
        ),
        (
            view(DurableSmState::Detached, Some(at(0))),
            attempt(&same, PrincipalStanding::Current, 14, at(1)),
            ResumeDenial::Handled(HandledError::TooHigh { h: 14, ceiling: 13 }),
        ),
        (
            view(DurableSmState::Detached, Some(at(0))),
            attempt(&same, PrincipalStanding::Current, 9, at(1)),
            ResumeDenial::Handled(HandledError::Behind { h: 9, acked: 10 }),
        ),
    ];
    for (view, attempt, expected) in denials {
        assert_eq!(authorize_resume(&view, &attempt), Err(expected));
    }
}

#[test]
fn resume_denies_generation_exhaustion() {
    let stored = principal(1, 7, "alice@example.test");
    let view = DurableSessionView {
        state: DurableSmState::Detached,
        generation: generation(i64::MAX as u64),
        principal: &stored,
        window: OutboundWindow::EMPTY,
        detached_at: None,
        max_resume: Duration::from_secs(1),
    };
    let attempt = ResumeAttempt {
        presented: &stored,
        stored_standing: PrincipalStanding::Current,
        h: 0,
        now: at(0),
    };
    assert_eq!(
        authorize_resume(&view, &attempt),
        Err(ResumeDenial::GenerationExhausted)
    );
}

#[test]
fn resume_token_round_trips_and_hides_its_bytes() {
    let token = SmResumeToken::mint();
    let wire = token.to_wire();
    let parsed = SmResumeToken::from_wire(&wire).expect("own token parses");
    assert_eq!(parsed.digest(), token.digest());
    assert_ne!(SmResumeToken::mint().digest(), token.digest());
    assert!(!format!("{token:?}").contains(&wire));
    assert!(SmResumeToken::from_wire("not a token").is_err());
    assert!(SmResumeToken::from_wire("c2hvcnQ").is_err());
}

proptest! {
    /// Every handled count inside the exposed window advances the floor to
    /// exactly that ordinal, wherever the window sits in the counter space.
    #[test]
    fn exposed_handled_counts_advance_exactly(
        acked in 0_u64..(1 << 40),
        exposed in 0_u32..5_000,
        unexposed in 0_u32..5_000,
        pick in any::<u32>(),
    ) {
        let stored = window(
            acked,
            acked + u64::from(exposed),
            acked + u64::from(exposed) + u64::from(unexposed),
        );
        let step = if exposed == 0 { 0 } else { pick % (exposed + 1) };
        let target = ordinal(acked + u64::from(step));
        let (after, decision) = stored
            .acknowledge(target.wire(), AckKind::Live)
            .expect("exposed count is valid");
        prop_assert_eq!(after.acked(), target);
        prop_assert_eq!(after.exposure_ceiling(), stored.exposure_ceiling());
        prop_assert_eq!(after.appended(), stored.appended());
        if step == 0 {
            prop_assert_eq!(decision, HandledDecision::Unchanged);
        } else {
            prop_assert_eq!(decision, HandledDecision::Advanced { through: target, released: step });
        }
    }

    /// No handled count ever moves the floor above the exposure ceiling or
    /// below itself, and an error leaves the window untouched.
    #[test]
    fn handled_counts_never_escape_the_window(
        acked in 0_u64..(1 << 40),
        exposed in 0_u32..5_000,
        unexposed in 0_u32..5_000,
        h in any::<u32>(),
        resume in any::<bool>(),
    ) {
        let stored = window(
            acked,
            acked + u64::from(exposed),
            acked + u64::from(exposed) + u64::from(unexposed),
        );
        let kind = if resume { AckKind::Resume } else { AckKind::Live };
        match stored.acknowledge(h, kind) {
            Ok((after, _)) => {
                prop_assert!(after.acked() >= stored.acked());
                prop_assert!(after.acked() <= stored.exposure_ceiling());
                prop_assert_eq!(after.appended(), stored.appended());
            }
            Err(HandledError::TooHigh { .. }) => {
                let delta = h.wrapping_sub(stored.acked().wire());
                prop_assert!(delta > exposed && delta < (1 << 31));
            }
            Err(HandledError::Ambiguous { .. }) => {
                prop_assert_eq!(h.wrapping_sub(stored.acked().wire()), 1 << 31);
            }
            Err(HandledError::Behind { .. }) => prop_assert!(resume),
        }
    }

    /// Appending then acknowledging everything returns to an empty window
    /// with matching ordinals; ordinals are dense and strictly increasing.
    #[test]
    fn appended_ordinals_are_dense(start in 0_u64..(1 << 40), count in 1_u32..200) {
        let bounds = ExposureBounds::new(1_000, u64::MAX).expect("bounds");
        let mut stored = window(start, start, start);
        for offset in 1..=count {
            let (next, allocated) = stored
                .append(Exposure::Exposed, bounds, 0, 1)
                .expect("within bounds");
            prop_assert_eq!(allocated, ordinal(start + u64::from(offset)));
            stored = next;
        }
        let (settled, _) = stored
            .acknowledge(stored.appended().wire(), AckKind::Live)
            .expect("all exposed");
        prop_assert_eq!(settled.outstanding(), 0);
        prop_assert!(settled.replay().is_empty());
    }
}
