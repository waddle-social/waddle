//! L2 — cross-handler XEP invariants for the message pipeline.
//!
//! Per the #229 Q9 test layering, L1 tests live with each handler and
//! cover that handler's XEP rules in isolation. L2 tests live here and
//! cover invariants that **emerge from handler ordering**:
//!
//! - **Privacy invariant** (XEP-0191 §3 + XEP-0313): a stanza halted by
//!   the blocking filter MUST NOT reach the archive — no later handler
//!   that emits an archive event runs.
//! - **Stamping invariant** (XEP-0359 §5 + XEP-0280 §4): the
//!   canonicalize handler's `<stanza-id>` is observable to every later
//!   handler in the same dispatch pass — a probe registered after
//!   `CanonicalizeHandler` reads the stamped id from the live message
//!   reference.
//!
//! Test names are NOT `xep_NNNN_*` here because these are pipeline-shape
//! invariants that span multiple XEPs. They use the `dispatch_message_*`
//! convention from L3.

use super::dispatch::{MessageDispatchTermination, StanzaDispatcher};
use super::event::OutboundEvent;
use super::handlers::blocking_filter::BlockingFilterHandler;
use super::handlers::canonicalize::CanonicalizeHandler;
use super::id_gen::FixedIdGenerator;
use super::message_context::{MessageContext, MessageContextEnv};
use super::session_state::{Blocklist, CarbonsState, MucOccupancy};
use super::traits::{HandlerOutcome, MessageHandler};
use crate::xep::xep0359::extract_stanza_id_by;
use jid::{BareJid, FullJid};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use xmpp_parsers::message::{Body, Message, MessageType};

// ---------------------------------------------------------------------
// Probe handlers used by the L2 invariants.
// ---------------------------------------------------------------------

/// Probe that emits a fake `ArchiveDirect` event whenever it runs. Used
/// by the privacy invariant to prove the archive never sees a blocked
/// stanza.
struct ArchiveProbe {
    invocations: Arc<AtomicUsize>,
}

impl MessageHandler for ArchiveProbe {
    fn name(&self) -> &'static str {
        "test-archive-probe"
    }

    fn handle(&self, message: &mut Message, _ctx: &MessageContext<'_>) -> HandlerOutcome {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        let from_bare = message
            .from
            .as_ref()
            .map(|j| j.to_bare())
            .unwrap_or_else(|| "unknown@example.com".parse().expect("bare"));
        let to_bare = message
            .to
            .as_ref()
            .map(|j| j.to_bare())
            .unwrap_or_else(|| "unknown@example.com".parse().expect("bare"));
        let archive_jid = from_bare.clone();
        HandlerOutcome::Continue(vec![OutboundEvent::ArchiveDirect {
            archive_jid,
            from: from_bare,
            to: to_bare,
            message: Box::new(message.clone()),
        }])
    }
}

/// Probe that records the stanza-id stamped under `by=` for assertion
/// in the stamping invariant test.
struct StampReader {
    by: String,
    captured: Arc<Mutex<Option<String>>>,
}

impl MessageHandler for StampReader {
    fn name(&self) -> &'static str {
        "test-stamp-reader"
    }

    fn handle(&self, message: &mut Message, _ctx: &MessageContext<'_>) -> HandlerOutcome {
        let id = extract_stanza_id_by(message, &self.by);
        *self.captured.lock().expect("mutex") = id;
        HandlerOutcome::Continue(Vec::new())
    }
}

// ---------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------

fn full(s: &str) -> FullJid {
    s.parse().expect("valid full jid")
}

fn bare(s: &str) -> BareJid {
    s.parse().expect("valid bare jid")
}

fn chat_msg(from: &str, to: &str) -> Message {
    let mut m = Message::new(Some(to.parse().expect("jid")));
    m.from = Some(from.parse().expect("jid"));
    m.type_ = MessageType::Chat;
    m.bodies.insert(String::new(), Body("hi".to_string()));
    m
}

fn build_ctx<'a>(
    local: &'a FullJid,
    bl: &'a Blocklist,
    occ: &'a MucOccupancy,
    gen: &'a FixedIdGenerator,
    msg: &Message,
) -> MessageContext<'a> {
    let env = MessageContextEnv {
        domain: "example.com",
        full_jid: local,
        blocklist: bl,
        carbons: CarbonsState::Disabled,
        muc_occupancy: occ,
        has_live_transport: true,
        id_gen: gen,
    };
    MessageContext::derive(env, msg)
}

// ---------------------------------------------------------------------
// Privacy invariant — XEP-0191 + XEP-0313
// ---------------------------------------------------------------------

#[test]
fn dispatch_message_blocked_sender_emits_no_archive_event() {
    // Recipient pass: incoming from a blocked sender.
    let local = full("bob@example.com/desk");
    let bl = Blocklist::new([bare("alice@example.com")]);
    let occ = MucOccupancy::empty();
    let gen = FixedIdGenerator("id".to_string());

    let mut dispatcher = StanzaDispatcher::new();
    dispatcher.register_message(Arc::new(BlockingFilterHandler));
    let archive_invocations = Arc::new(AtomicUsize::new(0));
    dispatcher.register_message(Arc::new(ArchiveProbe {
        invocations: archive_invocations.clone(),
    }));

    let mut msg = chat_msg("alice@example.com/web", "bob@example.com");
    let ctx = build_ctx(&local, &bl, &occ, &gen, &msg);
    let outcome = dispatcher.dispatch_message(&mut msg, &ctx);

    // §3.1 silent drop: BlockingFilterHandler returns Halt with no events.
    assert!(matches!(
        outcome.termination,
        MessageDispatchTermination::Halted { .. }
    ));
    assert!(outcome.events.is_empty());
    // Archive probe must NEVER have run.
    assert_eq!(archive_invocations.load(Ordering::SeqCst), 0);
    // No ArchiveDirect event present in the outcome.
    assert!(!outcome
        .events
        .iter()
        .any(|e| matches!(e, OutboundEvent::ArchiveDirect { .. })));
}

#[test]
fn dispatch_message_blocked_recipient_outgoing_emits_error_no_archive() {
    // Sender pass: outgoing to a blocked recipient.
    let local = full("alice@example.com/web");
    let bl = Blocklist::new([bare("blocked@example.com")]);
    let occ = MucOccupancy::empty();
    let gen = FixedIdGenerator("id".to_string());

    let mut dispatcher = StanzaDispatcher::new();
    dispatcher.register_message(Arc::new(BlockingFilterHandler));
    let archive_invocations = Arc::new(AtomicUsize::new(0));
    dispatcher.register_message(Arc::new(ArchiveProbe {
        invocations: archive_invocations.clone(),
    }));

    let mut msg = chat_msg("alice@example.com/web", "blocked@example.com");
    let ctx = build_ctx(&local, &bl, &occ, &gen, &msg);
    let outcome = dispatcher.dispatch_message(&mut msg, &ctx);

    assert!(matches!(
        outcome.termination,
        MessageDispatchTermination::Halted { .. }
    ));
    // §3.2 not-acceptable reply present.
    assert!(outcome
        .events
        .iter()
        .any(|e| matches!(e, OutboundEvent::SendStanza(_))));
    assert_eq!(archive_invocations.load(Ordering::SeqCst), 0);
    assert!(!outcome
        .events
        .iter()
        .any(|e| matches!(e, OutboundEvent::ArchiveDirect { .. })));
}

#[test]
fn dispatch_message_unblocked_message_reaches_archive() {
    // Control: not blocked → archive probe runs.
    let local = full("bob@example.com/desk");
    let bl = Blocklist::empty();
    let occ = MucOccupancy::empty();
    let gen = FixedIdGenerator("id".to_string());

    let mut dispatcher = StanzaDispatcher::new();
    dispatcher.register_message(Arc::new(BlockingFilterHandler));
    let archive_invocations = Arc::new(AtomicUsize::new(0));
    dispatcher.register_message(Arc::new(ArchiveProbe {
        invocations: archive_invocations.clone(),
    }));

    let mut msg = chat_msg("alice@example.com/web", "bob@example.com");
    let ctx = build_ctx(&local, &bl, &occ, &gen, &msg);
    let outcome = dispatcher.dispatch_message(&mut msg, &ctx);

    assert!(matches!(
        outcome.termination,
        MessageDispatchTermination::Completed
    ));
    assert_eq!(archive_invocations.load(Ordering::SeqCst), 1);
    assert!(outcome
        .events
        .iter()
        .any(|e| matches!(e, OutboundEvent::ArchiveDirect { .. })));
}

// ---------------------------------------------------------------------
// Stamping invariant — XEP-0359 §5 + XEP-0280 §4
// ---------------------------------------------------------------------

#[test]
fn dispatch_message_canonicalize_stamp_visible_to_later_handlers() {
    let local = full("alice@example.com/web");
    let bl = Blocklist::empty();
    let occ = MucOccupancy::empty();
    let gen = FixedIdGenerator("stamped-1".to_string());

    let mut dispatcher = StanzaDispatcher::new();
    dispatcher.register_message(Arc::new(CanonicalizeHandler));
    let captured = Arc::new(Mutex::new(None::<String>));
    dispatcher.register_message(Arc::new(StampReader {
        by: "alice@example.com".to_string(),
        captured: captured.clone(),
    }));

    let mut msg = chat_msg("alice@example.com/web", "bob@example.com");
    let ctx = build_ctx(&local, &bl, &occ, &gen, &msg);
    let outcome = dispatcher.dispatch_message(&mut msg, &ctx);

    assert!(matches!(
        outcome.termination,
        MessageDispatchTermination::Completed
    ));
    let captured = captured.lock().expect("mutex").clone();
    assert_eq!(captured, Some("stamped-1".to_string()));
}

#[test]
fn dispatch_message_canonicalize_stamp_visible_after_blocking_filter_passthrough() {
    // The full ordered chain: Blocking → Canonicalize → StampReader.
    // For an unblocked message the reader sees the canonicalize stamp.
    let local = full("alice@example.com/web");
    let bl = Blocklist::empty();
    let occ = MucOccupancy::empty();
    let gen = FixedIdGenerator("stamped-2".to_string());

    let mut dispatcher = StanzaDispatcher::new();
    dispatcher.register_message(Arc::new(BlockingFilterHandler));
    dispatcher.register_message(Arc::new(CanonicalizeHandler));
    let captured = Arc::new(Mutex::new(None::<String>));
    dispatcher.register_message(Arc::new(StampReader {
        by: "alice@example.com".to_string(),
        captured: captured.clone(),
    }));

    let mut msg = chat_msg("alice@example.com/web", "bob@example.com");
    let ctx = build_ctx(&local, &bl, &occ, &gen, &msg);
    let outcome = dispatcher.dispatch_message(&mut msg, &ctx);

    assert!(matches!(
        outcome.termination,
        MessageDispatchTermination::Completed
    ));
    let captured = captured.lock().expect("mutex").clone();
    assert_eq!(captured, Some("stamped-2".to_string()));
}
