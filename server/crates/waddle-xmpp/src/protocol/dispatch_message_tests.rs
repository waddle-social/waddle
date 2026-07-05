//! L3 callback-machinery tests for the message pipeline.
//!
//! Asserts the contract that PR2–PR5 handlers depend on:
//!
//! - [`HandlerOutcome::Continue`] runs subsequent handlers.
//! - [`HandlerOutcome::Halt`] short-circuits the pipeline; no later
//!   handler runs and the termination is `Halted`.
//! - [`HandlerOutcome::AwaitCallback`] short-circuits the pipeline; the
//!   termination carries the `resume_after` handler id supplied by the
//!   awaiting handler.
//! - `resume_message` runs only handlers strictly after `resume_after`.
//! - Resumed handlers can themselves halt or await again — the dispatcher
//!   threads the new termination through the second outcome.
//!
//! These tests don't exercise any concrete XEP behaviour; that's L1's
//! job in the per-handler test files. L3 locks the *machinery* so handler
//! authors can rely on it.
//!
//! Test naming: prefixed `dispatch_message_*` rather than `xep_NNNN_*`
//! because these are architectural invariants, not XEP rules.

use super::dispatch::{MessageDispatchTermination, StanzaDispatcher};
use super::event::OutboundEvent;
use super::id_gen::FixedIdGenerator;
use super::message_context::{MessageContext, MessageContextEnv};
use super::session_state::{Blocklist, CarbonsState, MucOccupancy};
use super::traits::{HandlerId, HandlerOutcome, MessageHandler};
use jid::FullJid;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tracing::Level;
use xmpp_parsers::message::{Message, MessageType};

// ---------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------

fn local_jid() -> FullJid {
    "alice@waddle.social/web".parse().expect("valid full jid")
}

fn chat_message() -> Message {
    let mut m = Message::new(Some("bob@waddle.social".parse().expect("jid")));
    m.from = Some("alice@waddle.social/web".parse().expect("jid"));
    m.type_ = MessageType::Chat;
    m
}

struct Fixture {
    blocklist: Blocklist,
    occupancy: MucOccupancy,
    id_gen: FixedIdGenerator,
    jid: FullJid,
}

impl Fixture {
    fn new() -> Self {
        Self {
            blocklist: Blocklist::empty(),
            occupancy: MucOccupancy::empty(),
            id_gen: FixedIdGenerator("test-id".to_string()),
            jid: local_jid(),
        }
    }

    fn ctx<'a>(&'a self, message: &Message) -> MessageContext<'a> {
        let env = MessageContextEnv {
            domain: "waddle.social",
            full_jid: &self.jid,
            blocklist: &self.blocklist,
            carbons: CarbonsState::Disabled,
            muc_occupancy: &self.occupancy,
            has_live_transport: true,
            delivery_fanout: &[],
            id_gen: &self.id_gen,
        };
        MessageContext::derive(env, message)
    }
}

// ---------------------------------------------------------------------
// Probe handlers
// ---------------------------------------------------------------------

/// Records the order in which it was called and emits one log event with
/// its name. Always returns `Continue`.
struct ContinueProbe {
    name: &'static str,
    call_order: Arc<AtomicUsize>,
}

impl ContinueProbe {
    fn new(name: &'static str, call_order: Arc<AtomicUsize>) -> Arc<Self> {
        Arc::new(Self { name, call_order })
    }
}

impl MessageHandler for ContinueProbe {
    fn name(&self) -> &'static str {
        self.name
    }

    fn handle(&self, _message: &mut Message, _ctx: &MessageContext<'_>) -> HandlerOutcome {
        self.call_order.fetch_add(1, Ordering::SeqCst);
        HandlerOutcome::Continue(vec![OutboundEvent::Log {
            level: Level::DEBUG,
            message: format!("{}-ran", self.name),
        }])
    }
}

/// Halts immediately, emitting one log event.
struct HaltProbe {
    name: &'static str,
}

impl HaltProbe {
    fn new(name: &'static str) -> Arc<Self> {
        Arc::new(Self { name })
    }
}

impl MessageHandler for HaltProbe {
    fn name(&self) -> &'static str {
        self.name
    }

    fn handle(&self, _message: &mut Message, _ctx: &MessageContext<'_>) -> HandlerOutcome {
        HandlerOutcome::Halt(vec![OutboundEvent::Log {
            level: Level::DEBUG,
            message: format!("{}-halted", self.name),
        }])
    }
}

/// Pauses the pipeline at this handler's position; the dispatcher fills
/// in `resume_after` from the iteration index.
struct AwaitProbe {
    name: &'static str,
}

impl AwaitProbe {
    fn new(name: &'static str) -> Arc<Self> {
        Arc::new(Self { name })
    }
}

impl MessageHandler for AwaitProbe {
    fn name(&self) -> &'static str {
        self.name
    }

    fn handle(&self, _message: &mut Message, _ctx: &MessageContext<'_>) -> HandlerOutcome {
        HandlerOutcome::AwaitCallback(vec![OutboundEvent::Log {
            level: Level::DEBUG,
            message: format!("{}-paused", self.name),
        }])
    }
}

// ---------------------------------------------------------------------
// L3 invariants
// ---------------------------------------------------------------------

#[test]
fn dispatch_message_runs_handlers_in_registration_order_and_completes() {
    let mut dispatcher = StanzaDispatcher::new();
    let order = Arc::new(AtomicUsize::new(0));
    dispatcher.register_message(ContinueProbe::new("h0", order.clone()));
    dispatcher.register_message(ContinueProbe::new("h1", order.clone()));
    dispatcher.register_message(ContinueProbe::new("h2", order.clone()));

    let fx = Fixture::new();
    let mut msg = chat_message();
    let ctx = fx.ctx(&msg);
    let outcome = dispatcher.dispatch_message(&mut msg, &ctx);

    assert_eq!(outcome.events.len(), 3);
    assert!(matches!(
        outcome.termination,
        MessageDispatchTermination::Completed
    ));
}

#[test]
fn dispatch_message_halt_short_circuits_later_handlers() {
    let mut dispatcher = StanzaDispatcher::new();
    let order = Arc::new(AtomicUsize::new(0));
    dispatcher.register_message(ContinueProbe::new("h0", order.clone()));
    dispatcher.register_message(HaltProbe::new("h1-halt"));
    // Counter starts at 0; if h2 runs it would increment.
    let counter_after_halt = Arc::new(AtomicUsize::new(0));
    dispatcher.register_message(ContinueProbe::new("h2", counter_after_halt.clone()));

    let fx = Fixture::new();
    let mut msg = chat_message();
    let ctx = fx.ctx(&msg);
    let outcome = dispatcher.dispatch_message(&mut msg, &ctx);

    // h0 (continue) + h1 (halt) emitted events; h2 did not run.
    assert_eq!(outcome.events.len(), 2);
    assert_eq!(counter_after_halt.load(Ordering::SeqCst), 0);
    match outcome.termination {
        MessageDispatchTermination::Halted { halted_at } => {
            assert_eq!(halted_at, HandlerId(1));
        }
        other => panic!("expected Halted, got {other:?}"),
    }
}

#[test]
fn dispatch_message_await_short_circuits_and_carries_resume_after() {
    let mut dispatcher = StanzaDispatcher::new();
    let order = Arc::new(AtomicUsize::new(0));
    dispatcher.register_message(ContinueProbe::new("h0", order.clone()));
    let id_h1 = dispatcher.register_message(AwaitProbe::new("h1-await"));
    let counter_after_await = Arc::new(AtomicUsize::new(0));
    dispatcher.register_message(ContinueProbe::new("h2", counter_after_await.clone()));

    let fx = Fixture::new();
    let mut msg = chat_message();
    let ctx = fx.ctx(&msg);
    let outcome = dispatcher.dispatch_message(&mut msg, &ctx);

    assert_eq!(outcome.events.len(), 2);
    assert_eq!(counter_after_await.load(Ordering::SeqCst), 0);
    match outcome.termination {
        MessageDispatchTermination::Awaiting { resume_after } => {
            assert_eq!(resume_after, id_h1);
        }
        other => panic!("expected Awaiting, got {other:?}"),
    }
}

#[test]
fn resume_message_skips_handlers_up_to_and_including_resume_after() {
    let mut dispatcher = StanzaDispatcher::new();
    let h0_count = Arc::new(AtomicUsize::new(0));
    let h1_count = Arc::new(AtomicUsize::new(0));
    let h2_count = Arc::new(AtomicUsize::new(0));
    dispatcher.register_message(ContinueProbe::new("h0", h0_count.clone()));
    dispatcher.register_message(ContinueProbe::new("h1", h1_count.clone()));
    dispatcher.register_message(ContinueProbe::new("h2", h2_count.clone()));

    let fx = Fixture::new();
    let mut msg = chat_message();
    let ctx = fx.ctx(&msg);
    let outcome = dispatcher.resume_message(&mut msg, &ctx, HandlerId(1));

    // h0 and h1 must NOT run on resume; only h2.
    assert_eq!(h0_count.load(Ordering::SeqCst), 0);
    assert_eq!(h1_count.load(Ordering::SeqCst), 0);
    assert_eq!(h2_count.load(Ordering::SeqCst), 1);
    assert_eq!(outcome.events.len(), 1);
    assert!(matches!(
        outcome.termination,
        MessageDispatchTermination::Completed
    ));
}

#[test]
fn resume_message_can_halt_or_await_again() {
    let mut dispatcher = StanzaDispatcher::new();
    let h0_count = Arc::new(AtomicUsize::new(0));
    dispatcher.register_message(ContinueProbe::new("h0", h0_count));
    dispatcher.register_message(AwaitProbe::new("h1-await"));
    dispatcher.register_message(HaltProbe::new("h2-halt"));

    let fx = Fixture::new();
    let mut msg = chat_message();
    let ctx = fx.ctx(&msg);
    // Resume after h0; the next handler (h1) parks again. Then resume
    // after h1; the next handler (h2) halts.
    let resumed = dispatcher.resume_message(&mut msg, &ctx, HandlerId(0));
    assert!(matches!(
        resumed.termination,
        MessageDispatchTermination::Awaiting {
            resume_after: HandlerId(1)
        }
    ));

    let resumed_again = dispatcher.resume_message(&mut msg, &ctx, HandlerId(1));
    assert!(matches!(
        resumed_again.termination,
        MessageDispatchTermination::Halted {
            halted_at: HandlerId(2)
        }
    ));
}

#[test]
fn resume_message_with_resume_after_at_last_handler_completes_immediately() {
    // resume_after points at the last registered handler — there's nothing
    // after it. The pipeline completes without running any handler.
    let mut dispatcher = StanzaDispatcher::new();
    let h0_count = Arc::new(AtomicUsize::new(0));
    let h1_count = Arc::new(AtomicUsize::new(0));
    dispatcher.register_message(ContinueProbe::new("h0", h0_count.clone()));
    dispatcher.register_message(ContinueProbe::new("h1", h1_count.clone()));

    let fx = Fixture::new();
    let mut msg = chat_message();
    let ctx = fx.ctx(&msg);
    let outcome = dispatcher.resume_message(&mut msg, &ctx, HandlerId(1));

    assert_eq!(h0_count.load(Ordering::SeqCst), 0);
    assert_eq!(h1_count.load(Ordering::SeqCst), 0);
    assert!(outcome.events.is_empty());
    assert!(matches!(
        outcome.termination,
        MessageDispatchTermination::Completed
    ));
}

#[test]
fn empty_pipeline_completes_with_no_events() {
    let dispatcher = StanzaDispatcher::new();
    let fx = Fixture::new();
    let mut msg = chat_message();
    let ctx = fx.ctx(&msg);
    let outcome = dispatcher.dispatch_message(&mut msg, &ctx);
    assert!(outcome.events.is_empty());
    assert!(matches!(
        outcome.termination,
        MessageDispatchTermination::Completed
    ));
}

#[test]
fn handler_outcome_noop_helper_is_an_empty_continue() {
    match HandlerOutcome::noop() {
        HandlerOutcome::Continue(events) => assert!(events.is_empty()),
        other => panic!("expected Continue([]), got {other:?}"),
    }
}

// `dispatch_message_rejects_handler_supplied_resume_after_past_own_position`
// from an earlier draft is obsolete: `HandlerOutcome::AwaitCallback` is a
// tuple variant whose `resume_after` is filled in by the dispatcher from
// the iteration index, so handlers cannot supply an out-of-range value.
// The complementary bounds-check on caller-supplied `resume_after` lives
// in `resume_message` and is exercised below.

#[test]
fn resume_message_rejects_out_of_range_resume_after() {
    let mut dispatcher = StanzaDispatcher::new();
    let counter = Arc::new(AtomicUsize::new(0));
    dispatcher.register_message(ContinueProbe::new("h0", counter.clone()));

    let fx = Fixture::new();
    let mut msg = chat_message();
    let ctx = fx.ctx(&msg);
    // resume_after points past the only registered handler.
    let outcome = dispatcher.resume_message(&mut msg, &ctx, HandlerId(99));

    assert!(matches!(
        outcome.termination,
        MessageDispatchTermination::Halted { .. }
    ));
    assert_eq!(counter.load(Ordering::SeqCst), 0);
    assert!(outcome.events.iter().any(|e| matches!(
        e,
        OutboundEvent::Log { level, .. } if *level == Level::ERROR
    )));
}
