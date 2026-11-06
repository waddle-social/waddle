#![cfg(feature = "test-support")]

//! Scheduler-model coverage for the dark P3.1 output continuation fixture.
//! These tests do not exercise `WebSocketOutput` or claim transport custody.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use waddle_server::test_support::connection_actor::{
    ContinuationArbiter, ContinuationError, ContinuationLimits, ContinuationPhase,
    OutputContinuation,
};
use waddle_xmpp::ownership::{ConnectionRef, NodeLeaseIdentity};
use waddle_xmpp::Stanza;
use xmpp_parsers::message::Message;

fn connection() -> ConnectionRef {
    ConnectionRef::fresh(NodeLeaseIdentity::local_process())
}

fn stanza() -> Stanza {
    Stanza::Message(Message::new(None::<jid::Jid>))
}

fn encoded_len(stanza: &Stanza) -> usize {
    let mut encoded = Vec::new();
    stanza
        .to_element()
        .write_to(&mut encoded)
        .expect("synthetic message serializes");
    encoded.len()
}

fn limits(max_frames: usize, max_bytes: usize) -> ContinuationLimits {
    ContinuationLimits {
        max_frames,
        max_serialized_bytes: max_bytes,
        max_frame_bytes: max_bytes,
    }
}

#[test]
fn output_is_lazy_effect_once_and_dropping_a_ticket_observer_keeps_the_cursor() {
    let exact = encoded_len(&stanza());
    let calls = Arc::new(AtomicUsize::new(0));
    let mut continuation =
        OutputContinuation::new(connection(), limits(1, exact)).expect("valid exact budget");

    assert_eq!(continuation.retained_credits(), (0, 0));
    let calls_before_effect = Arc::clone(&calls);
    let first = continuation
        .prepare_next(move |_| {
            calls_before_effect.fetch_add(1, Ordering::SeqCst);
            Some(stanza())
        })
        .expect("first production")
        .expect("one frame");
    assert_eq!(first.ordinal(), 0);
    assert_eq!(continuation.retained_credits(), (1, exact));

    // Dropping this scheduler-model ticket observation owns no frame or
    // cursor. A later observation gets the same ticket without repeating the
    // effect. This is not a transport submission-cancellation test.
    drop(first);
    let duplicate_effect = Arc::clone(&calls);
    let current = continuation
        .prepare_next(move |_| {
            duplicate_effect.fetch_add(1, Ordering::SeqCst);
            Some(stanza())
        })
        .expect("retained frame is returned")
        .expect("same frame remains current");
    assert_eq!(current.ordinal(), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        continuation.phase(&current),
        Ok(ContinuationPhase::Prepared)
    );
}

#[test]
fn hard_count_and_serialized_byte_credits_are_reserved_before_effects() {
    let exact = encoded_len(&stanza());
    let calls = Arc::new(AtomicUsize::new(0));
    let mut continuation =
        OutputContinuation::new(connection(), limits(1, exact)).expect("valid exact budget");
    let effect_calls = Arc::clone(&calls);
    let ticket = continuation
        .prepare_next(move |_| {
            effect_calls.fetch_add(1, Ordering::SeqCst);
            Some(stanza())
        })
        .expect("exact boundary admitted")
        .expect("frame exists");
    assert_eq!(continuation.serialized_bytes(&ticket), Ok(exact));
    continuation
        .mark_handled(1)
        .expect("handled input sequence");
    continuation.checkpoint_contiguous_handled_prefix();
    assert_eq!(
        continuation.acknowledgement_at_safe_yield(),
        Some(1),
        "the scheduler model exposes an uncounted SM ack at a cooperative yield even while data credits are full"
    );

    let forbidden_effect = Arc::clone(&calls);
    let same_ticket = continuation
        .prepare_next(move |_| {
            forbidden_effect.fetch_add(1, Ordering::SeqCst);
            Some(stanza())
        })
        .expect("credit-saturated continuation returns its retained frame");
    assert_eq!(same_ticket, Some(ticket.clone()));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    continuation.admit(&ticket).expect("admission milestone");
    continuation.attempted(&ticket).expect("attempt milestone");
    continuation
        .mark_flushed(&ticket)
        .expect("local flush settles credits");
    assert_eq!(continuation.retained_credits(), (0, 0));
    assert_eq!(continuation.phase(&ticket), Ok(ContinuationPhase::Flushed));

    let next = continuation
        .prepare_next(|_| Some(stanza()))
        .expect("credits available after flush")
        .expect("next lazy frame");
    assert_eq!(next.ordinal(), 1);

    let too_small = OutputContinuation::new(
        connection(),
        ContinuationLimits {
            max_frames: 1,
            max_serialized_bytes: exact - 1,
            max_frame_bytes: exact,
        },
    );
    assert!(matches!(too_small, Err(ContinuationError::InvalidBounds)));
}

#[test]
fn stop_before_admission_retains_the_prepared_frame_without_retry() {
    let exact = encoded_len(&stanza());
    let mut continuation =
        OutputContinuation::new(connection(), limits(1, exact)).expect("valid bounds");
    let ticket = continuation
        .prepare_next(|_| Some(stanza()))
        .expect("effect completes")
        .expect("frame retained");
    continuation.stop();
    assert_eq!(continuation.phase(&ticket), Ok(ContinuationPhase::Prepared));
    assert_eq!(continuation.retained_credits(), (1, exact));
    assert_eq!(continuation.admit(&ticket), Err(ContinuationError::Stopped));
    assert_eq!(
        continuation.prepare_next(|_| Some(stanza())),
        Err(ContinuationError::Stopped)
    );
}

#[test]
fn uncertain_attempt_retains_frame_and_credits_and_forbids_the_suffix() {
    let exact = encoded_len(&stanza());
    let mut continuation =
        OutputContinuation::new(connection(), limits(2, exact * 2)).expect("valid budget");
    let ticket = continuation
        .prepare_next(|_| Some(stanza()))
        .expect("producer runs")
        .expect("frame exists");
    continuation.admit(&ticket).expect("admitted");
    continuation.attempted(&ticket).expect("attempted");
    continuation
        .mark_uncertain(&ticket)
        .expect("uncertain result is terminal");

    assert_eq!(
        continuation.phase(&ticket),
        Ok(ContinuationPhase::Uncertain)
    );
    assert_eq!(continuation.retained_credits(), (1, exact));
    assert_eq!(
        continuation.prepare_next(|_| Some(stanza())),
        Err(ContinuationError::Stopped)
    );
}

#[test]
fn duplicate_exact_owner_claims_fail_and_ack_waits_for_checkpointed_prefix() {
    let connection = connection();
    let arbiter = ContinuationArbiter::default();
    let claim = arbiter
        .claim(connection.clone())
        .expect("first owner claims exact connection");
    assert_eq!(claim.connection_ref(), &connection);
    assert_eq!(
        arbiter.claim(connection.clone()).map(|_| ()),
        Err(ContinuationError::Stopped)
    );
    drop(claim);
    assert!(arbiter.claim(connection).is_ok());

    let mut continuation = OutputContinuation::new(
        ConnectionRef::fresh(NodeLeaseIdentity::local_process()),
        limits(1, 1024),
    )
    .expect("valid model");
    continuation
        .mark_handled(2)
        .expect("sequence in bounded window");
    assert_eq!(continuation.checkpoint_contiguous_handled_prefix(), 0);
    assert_eq!(continuation.acknowledgement_at_safe_yield(), Some(0));
    continuation.mark_handled(1).expect("hole filled");
    assert_eq!(continuation.checkpoint_contiguous_handled_prefix(), 2);
    assert_eq!(continuation.acknowledgement_at_safe_yield(), Some(2));

    continuation.seed_handled_frontier_for_test(u32::MAX as u64);
    continuation
        .mark_handled(u32::MAX as u64 + 1)
        .expect("next absolute sequence is in the window");
    assert_eq!(
        continuation.checkpoint_contiguous_handled_prefix(),
        u32::MAX as u64 + 1
    );
    assert_eq!(continuation.acknowledgement_at_safe_yield(), Some(0));
    assert_eq!(
        continuation.mark_handled(u32::MAX as u64 + 1 + 257),
        Err(ContinuationError::HandledWindowFull)
    );
}
