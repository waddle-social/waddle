use super::*;
use crate::protocol::handlers::enrichment_dispatch::ENRICHMENT_CALLBACK_SENTINEL;
use crate::protocol::handlers::ping::PingHandler;
use crate::protocol::handlers::rich_target_validation::RICH_TARGET_LOOKUP_CALLBACK_SENTINEL;
use minidom::Element;
use std::sync::Arc;
use xmpp_parsers::iq::Iq;

fn make_ping_iq(id: &str) -> Iq {
    let ping_elem = Element::builder("ping", crate::xep::xep0199::NS_PING).build();
    Iq::Get {
        from: None,
        to: None,
        id: id.to_string(),
        payload: ping_elem,
    }
}

fn test_jid() -> jid::FullJid {
    "alice@waddle.social/web"
        .parse()
        .expect("test JID is valid")
}

#[test]
fn ping_iq_in_ready_phase_emits_send_stanza() {
    let mut dispatcher = StanzaDispatcher::new();
    dispatcher.register_iq(Arc::new(PingHandler));
    let mut sm = test_support::ready_machine("waddle.social", test_jid(), dispatcher);

    let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
        Stanza::Iq(Box::new(make_ping_iq("ping-42"))),
    ))));

    assert_eq!(events.len(), 1, "expected one SendStanza event");
    match &events[0] {
        OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
            Stanza::Iq(reply) => {
                assert_eq!(reply.id(), "ping-42");
                assert!(matches!(reply.as_ref(), Iq::Result { .. }));
            }
            _ => panic!("expected IQ reply stanza"),
        },
        _ => panic!("expected SendStanza event"),
    }
}

#[test]
fn ping_iq_before_auth_is_logged_and_dropped() {
    let mut dispatcher = StanzaDispatcher::new();
    dispatcher.register_iq(Arc::new(PingHandler));
    let mut sm = XmppStateMachine::new("waddle.social", dispatcher);

    let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
        Stanza::Iq(Box::new(make_ping_iq("ping-early"))),
    ))));

    assert!(
        events
            .iter()
            .all(|e| !matches!(e, OutboundEvent::SendStanza(_))),
        "pre-auth stanzas must never produce a reply stanza"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, OutboundEvent::Log { .. })),
        "pre-auth stanzas should be logged for diagnostics"
    );
}

fn peer_presence(from: &str, to: &str) -> Stanza {
    let mut presence = xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
    presence.from = Some(from.parse::<jid::Jid>().expect("from jid"));
    presence.to = Some(to.parse::<jid::Jid>().expect("to jid"));
    presence.statuses.insert(
        xmpp_parsers::message::Lang::new(),
        "cluster-ready".to_string(),
    );
    Stanza::Presence(presence)
}

#[test]
fn peer_routed_presence_emits_send_stanza() {
    let mut sm = test_support::ready_machine(
        "waddle.social",
        "bob@waddle.social/phone".parse().expect("bound jid"),
        StanzaDispatcher::new(),
    );

    let events = sm.handle(InboundEvent::StanzaFromPeer(Box::new(peer_presence(
        "alice@waddle.social/web",
        "bob@waddle.social",
    ))));

    assert_eq!(events.len(), 1, "peer presence should produce one event");
    match &events[0] {
        OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
            Stanza::Presence(presence) => {
                assert_eq!(
                    presence.from.as_ref().map(ToString::to_string).as_deref(),
                    Some("alice@waddle.social/web")
                );
                assert_eq!(
                    presence
                        .statuses
                        .get(&xmpp_parsers::message::Lang::new())
                        .map(String::as_str),
                    Some("cluster-ready")
                );
            }
            other => panic!("expected presence stanza, got {}", other.name()),
        },
        _ => panic!("expected SendStanza event"),
    }
}

#[test]
fn peer_routed_presence_from_blocked_sender_is_dropped() {
    let mut sm = test_support::ready_machine(
        "waddle.social",
        "bob@waddle.social/phone".parse().expect("bound jid"),
        StanzaDispatcher::new(),
    );
    sm.set_blocklist(Blocklist::new(["alice@waddle.social"
        .parse::<jid::Jid>()
        .expect("blocked jid")]));

    let events = sm.handle(InboundEvent::StanzaFromPeer(Box::new(peer_presence(
        "alice@waddle.social/web",
        "bob@waddle.social",
    ))));

    assert!(
        events
            .iter()
            .all(|event| !matches!(event, OutboundEvent::SendStanza(_))),
        "blocked peer presence must not reach the wire"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            OutboundEvent::Log { level, .. } if *level == Level::DEBUG
        )),
        "blocked peer presence should emit a debug log"
    );
}

#[test]
fn unknown_iq_namespace_emits_log_warning() {
    let dispatcher = StanzaDispatcher::new(); // no handlers registered
    let mut sm = test_support::ready_machine("waddle.social", test_jid(), dispatcher);

    let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
        Stanza::Iq(Box::new(make_ping_iq("ping-unhandled"))),
    ))));

    assert!(
        events.iter().any(|e| matches!(
            e,
            OutboundEvent::Log { level, .. } if *level == Level::WARN
        )),
        "unhandled namespaces should emit a WARN log"
    );
}

#[test]
fn transport_closed_emits_info_log() {
    let mut sm = XmppStateMachine::new("waddle.social", StanzaDispatcher::new());
    let events = sm.handle(InboundEvent::TransportClosed);
    assert!(events.iter().any(|e| matches!(
        e,
        OutboundEvent::Log { level, .. } if *level == Level::INFO
    )));
}

#[test]
fn open_frame_is_noop_and_close_enters_closing_phase() {
    let mut sm = XmppStateMachine::new("waddle.social", StanzaDispatcher::new());
    assert!(sm
        .handle(InboundEvent::FrameReceived(InboundFrame::Open))
        .is_empty());
    assert!(sm
        .handle(InboundEvent::FrameReceived(InboundFrame::Close))
        .is_empty());
    assert!(matches!(sm.phase(), ConnectionPhase::Closing { .. }));
}

#[test]
fn callback_ids_are_monotonic_and_unique() {
    let mut sm = XmppStateMachine::new("waddle.social", StanzaDispatcher::new());
    let a = sm.next_callback_id();
    let b = sm.next_callback_id();
    let c = sm.next_callback_id();
    assert_ne!(a, b);
    assert_ne!(b, c);
    assert_eq!(a, CallbackId(1));
    assert_eq!(b, CallbackId(2));
    assert_eq!(c, CallbackId(3));
}

#[test]
fn pending_op_round_trip_is_matched_by_completion_event() {
    // Allocate a callback, register a pending op, then feed the
    // matching completion InboundEvent. The machine must look up
    // the op, emit a DEBUG log (matched=true) and drop the entry.
    let mut sm = XmppStateMachine::new("waddle.social", StanzaDispatcher::new());
    let id = sm.next_callback_id();
    sm.register_pending_op(
        id,
        PendingOp::MamQuery {
            request_id: "mam-7".to_string(),
            requester: test_jid(),
        },
    );

    let events = sm.handle(InboundEvent::MamQueryComplete {
        id,
        result: crate::protocol::event::CallbackResult::Ok { stanza: None },
    });

    assert!(events.iter().any(|e| matches!(
        e,
        OutboundEvent::Log { level, .. } if *level == Level::DEBUG
    )));
    // Second completion with the same id must now miss the pending
    // map (op consumed) and log at WARN — this is the late/duplicate
    // completion diagnostic path.
    let events2 = sm.handle(InboundEvent::MamQueryComplete {
        id,
        result: crate::protocol::event::CallbackResult::Ok { stanza: None },
    });
    assert!(events2.iter().any(|e| matches!(
        e,
        OutboundEvent::Log { level, .. } if *level == Level::WARN
    )));
}

// ----------------------------------------------------------------
// Pause/resume integration — the message pipeline parks via
// `AwaitCallback`, the matching `InboundEvent` arrives, the
// pipeline resumes and runs to completion.
// ----------------------------------------------------------------

use crate::protocol::event::ArchivedMessage;
use crate::protocol::handlers::canonicalize::CanonicalizeHandler;
use crate::protocol::handlers::enrichment_dispatch::EnrichmentDispatchHandler;
use crate::protocol::handlers::rich_target_validation::RichTargetValidationHandler;
use crate::protocol::message_context::MessageContext;
use crate::protocol::traits::{HandlerOutcome, MessageHandler};
use std::sync::atomic::{AtomicUsize, Ordering};
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::message::{Message, MessageType};

fn ready_machine_with_dispatcher(
    dispatcher: StanzaDispatcher,
    domain: &str,
    full_jid: jid::FullJid,
) -> XmppStateMachine {
    test_support::ready_machine(domain, full_jid, dispatcher)
}

fn alice() -> jid::FullJid {
    "alice@example.com/web".parse().expect("jid")
}

fn chat_with_body(from: &str, to: &str, body: &str) -> Message {
    let mut m = Message::new(Some(to.parse().expect("jid")));
    m.from = Some(from.parse().expect("jid"));
    m.type_ = MessageType::Chat;
    m.bodies
        .insert(xmpp_parsers::message::Lang::new(), body.to_string());
    m
}

/// Probe handler that records every invocation so tests can assert
/// "ran on resume" or "did not run".
struct TailProbe {
    invocations: Arc<AtomicUsize>,
}

impl MessageHandler for TailProbe {
    fn name(&self) -> &'static str {
        "test-tail-probe"
    }

    fn handle(&self, _message: &mut Message, _ctx: &MessageContext<'_>) -> HandlerOutcome {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        HandlerOutcome::Continue(Vec::new())
    }
}

#[test]
fn enrichment_await_then_complete_resumes_pipeline_with_rewritten_message() {
    let mut dispatcher = StanzaDispatcher::new();
    dispatcher.register_message(Arc::new(EnrichmentDispatchHandler));
    let tail = Arc::new(AtomicUsize::new(0));
    dispatcher.register_message(Arc::new(TailProbe {
        invocations: tail.clone(),
    }));

    let mut sm = ready_machine_with_dispatcher(dispatcher, "example.com", alice());
    let msg = chat_with_body(
        "alice@example.com/web",
        "bob@example.com",
        "see https://example.com/page",
    );
    let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
        Stanza::Message(msg),
    ))));

    // Pipeline parked: RequestEnrichment with a real CallbackId
    // (sentinel was 0; the state machine swapped it).
    let callback_id = events
        .iter()
        .find_map(|e| match e {
            OutboundEvent::RequestEnrichment { id, .. } => Some(*id),
            _ => None,
        })
        .expect("RequestEnrichment emitted");
    assert_ne!(callback_id, ENRICHMENT_CALLBACK_SENTINEL);
    // Tail handler has not run — pipeline is paused.
    assert_eq!(tail.load(Ordering::SeqCst), 0);

    // Feed the matching completion with a rewritten message; the
    // pipeline resumes and the tail probe runs.
    let mut rewritten = chat_with_body(
        "alice@example.com/web",
        "bob@example.com",
        "see https://example.com/page",
    );
    rewritten
        .payloads
        .push(minidom::Element::builder("reference", "urn:xmpp:reference:0").build());
    let resume_events = sm.handle(InboundEvent::EnrichmentComplete {
        id: callback_id,
        message: Box::new(rewritten),
    });
    assert_eq!(tail.load(Ordering::SeqCst), 1);
    // Resume produced no events from the no-op probe — but it
    // didn't error either. Don't format the events Vec into the
    // panic message: the OutboundEvent payload includes typed
    // Message stanzas that carry user content (CodeQL flags
    // Debug-formatting them in any logging/panic context as a
    // cleartext-logging hazard).
    let error_count = resume_events
        .iter()
        .filter(|e| matches!(e, OutboundEvent::Log { level, .. } if *level == Level::ERROR))
        .count();
    assert_eq!(
        error_count, 0,
        "resume must not log ERROR for the happy path"
    );
}

#[test]
fn xep_0308_await_then_loaded_with_valid_correction_target_resumes_pipeline() {
    let mut dispatcher = StanzaDispatcher::new();
    dispatcher.register_message(Arc::new(RichTargetValidationHandler));
    // Register canonicalize so resume actually does something
    // visible (stamps a stanza-id under alice's archive).
    dispatcher.register_message(Arc::new(CanonicalizeHandler));
    let tail = Arc::new(AtomicUsize::new(0));
    dispatcher.register_message(Arc::new(TailProbe {
        invocations: tail.clone(),
    }));

    let mut sm = ready_machine_with_dispatcher(dispatcher, "example.com", alice());
    let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "fixed text");
    msg.payloads
        .push(crate::xep::xep0308::build_replace_element("orig-msg-1"));
    let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
        Stanza::Message(msg),
    ))));

    let callback_id = events
        .iter()
        .find_map(|e| match e {
            OutboundEvent::LookupArchivedMessage { id, .. } => Some(*id),
            _ => None,
        })
        .expect("LookupArchivedMessage emitted");
    assert_ne!(callback_id, RICH_TARGET_LOOKUP_CALLBACK_SENTINEL);
    assert_eq!(tail.load(Ordering::SeqCst), 0);

    // Loaded with a valid same-author archived message → resume.
    let mut archived_msg =
        chat_with_body("alice@example.com/web", "bob@example.com", "original text");
    archived_msg.id = Some(xmpp_parsers::message::Id("orig-msg-1".to_string()));
    let archived = ArchivedMessage {
        stanza_id: StanzaId::new(
            "archive-A1",
            "alice@example.com".parse::<jid::Jid>().expect("jid"),
        ),
        message: Box::new(archived_msg),
        tombstoned: false,
    };
    let resume_events = sm.handle(InboundEvent::ArchivedMessageLoaded {
        id: callback_id,
        result: Some(Box::new(archived)),
    });
    assert_eq!(
        tail.load(Ordering::SeqCst),
        1,
        "valid completion resumes pipeline through canonicalize and tail"
    );
    // Canonicalize stamped under alice's archive — but we can't
    // observe the stamp here without inspecting the message
    // post-resume. The tail-probe count is the resume signal.
    // Don't Debug-format `resume_events` into the panic message
    // (see comment in the enrichment test above for rationale).
    let error_count = resume_events
        .iter()
        .filter(|e| matches!(e, OutboundEvent::Log { level, .. } if *level == Level::ERROR))
        .count();
    assert_eq!(error_count, 0, "valid resume must not ERROR");
}

#[test]
fn xep_0424_retraction_target_not_found_emits_item_not_found_no_resume() {
    let mut dispatcher = StanzaDispatcher::new();
    dispatcher.register_message(Arc::new(RichTargetValidationHandler));
    let tail = Arc::new(AtomicUsize::new(0));
    dispatcher.register_message(Arc::new(TailProbe {
        invocations: tail.clone(),
    }));

    let mut sm = ready_machine_with_dispatcher(dispatcher, "example.com", alice());
    let mut msg = chat_with_body(
        "alice@example.com/web",
        "bob@example.com",
        "I take that back",
    );
    msg.payloads
        .push(crate::xep::xep0424::build_retract_element("stanza-X"));
    let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
        Stanza::Message(msg),
    ))));
    let callback_id = events
        .iter()
        .find_map(|e| match e {
            OutboundEvent::LookupArchivedMessage { id, .. } => Some(*id),
            _ => None,
        })
        .expect("LookupArchivedMessage emitted");

    // Result: not found → typed item-not-found reply, no resume.
    let resume_events = sm.handle(InboundEvent::ArchivedMessageLoaded {
        id: callback_id,
        result: None,
    });
    assert_eq!(
        tail.load(Ordering::SeqCst),
        0,
        "item-not-found halt must not resume the pipeline"
    );
    // Verify the typed error reply is present.
    let has_error_reply = resume_events.iter().any(|e| match e {
        OutboundEvent::SendStanza(stanza) => {
            matches!(stanza.as_ref(), Stanza::Message(m) if m.type_ == MessageType::Error)
        }
        _ => false,
    });
    assert!(has_error_reply, "expected SendStanza error reply");
}

#[test]
fn xep_0308_correction_by_wrong_author_emits_not_acceptable() {
    let mut dispatcher = StanzaDispatcher::new();
    dispatcher.register_message(Arc::new(RichTargetValidationHandler));
    let tail = Arc::new(AtomicUsize::new(0));
    dispatcher.register_message(Arc::new(TailProbe {
        invocations: tail.clone(),
    }));

    let mut sm = ready_machine_with_dispatcher(dispatcher, "example.com", alice());
    let mut msg = chat_with_body("alice@example.com/web", "bob@example.com", "fixed text");
    msg.payloads
        .push(crate::xep::xep0308::build_replace_element("orig-msg-1"));
    let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
        Stanza::Message(msg),
    ))));
    let callback_id = events
        .iter()
        .find_map(|e| match e {
            OutboundEvent::LookupArchivedMessage { id, .. } => Some(*id),
            _ => None,
        })
        .expect("LookupArchivedMessage emitted");

    // Loaded with an archived message whose author differs.
    let mut archived_msg = chat_with_body("mallory@example.com/web", "bob@example.com", "imposter");
    archived_msg.id = Some(xmpp_parsers::message::Id("orig-msg-1".to_string()));
    let archived = ArchivedMessage {
        stanza_id: StanzaId::new(
            "archive-X",
            "alice@example.com".parse::<jid::Jid>().expect("jid"),
        ),
        message: Box::new(archived_msg),
        tombstoned: false,
    };
    let resume_events = sm.handle(InboundEvent::ArchivedMessageLoaded {
        id: callback_id,
        result: Some(Box::new(archived)),
    });
    assert_eq!(tail.load(Ordering::SeqCst), 0);
    let has_error_reply = resume_events.iter().any(|e| match e {
        OutboundEvent::SendStanza(stanza) => {
            matches!(stanza.as_ref(), Stanza::Message(m) if m.type_ == MessageType::Error)
        }
        _ => false,
    });
    assert!(has_error_reply);
}

#[test]
fn snapshot_is_frozen_across_re_park_does_not_leak_mutated_state() {
    // Regression test for the PR5 snapshot-drift bug. A long
    // pipeline can park twice (rich-target lookup, then enrichment
    // on resume). The original dispatch-start snapshot of session
    // state must be threaded through to the second park; without
    // the fix, the second `PendingOp::MessageDispatchResume` would
    // capture `self.*` mutated between the two parks and the
    // resumed handlers would see a different view than the initial
    // dispatch.
    use std::sync::Mutex;
    let captured: Arc<Mutex<Vec<Blocklist>>> = Arc::new(Mutex::new(Vec::new()));

    struct SnapshotProbe {
        captured: Arc<Mutex<Vec<Blocklist>>>,
    }
    impl MessageHandler for SnapshotProbe {
        fn name(&self) -> &'static str {
            "test-snapshot-probe"
        }
        fn handle(&self, _message: &mut Message, ctx: &MessageContext<'_>) -> HandlerOutcome {
            self.captured
                .lock()
                .expect("mutex")
                .push(ctx.blocklist.clone());
            HandlerOutcome::Continue(Vec::new())
        }
    }

    let mut dispatcher = StanzaDispatcher::new();
    // Order: rich-target → canonicalize → enrichment → snapshot probe.
    dispatcher.register_message(Arc::new(RichTargetValidationHandler));
    dispatcher.register_message(Arc::new(CanonicalizeHandler));
    dispatcher.register_message(Arc::new(EnrichmentDispatchHandler));
    dispatcher.register_message(Arc::new(SnapshotProbe {
        captured: captured.clone(),
    }));

    let mut sm = test_support::ready_machine("example.com", alice(), dispatcher);
    let original: jid::BareJid = "original@example.com".parse().expect("bare");
    sm.blocklist = Blocklist::new([original.clone()]);

    // Correction message with URL body — both handlers will fire
    // (rich-target first, then enrichment after resume).
    let mut msg = chat_with_body(
        "alice@example.com/web",
        "bob@example.com",
        "https://example.com/page",
    );
    msg.payloads
        .push(crate::xep::xep0308::build_replace_element("orig-msg-1"));
    let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
        Stanza::Message(msg),
    ))));
    let cb_rich = events
        .iter()
        .find_map(|e| match e {
            OutboundEvent::LookupArchivedMessage { id, .. } => Some(*id),
            _ => None,
        })
        .expect("rich-target parked");

    // Mutate session state between the two parks. The fix
    // requires the resumed dispatch (and its own re-park) to
    // ignore this mutation.
    let mutated: jid::BareJid = "mutated@example.com".parse().expect("bare");
    sm.blocklist = Blocklist::new([mutated.clone()]);

    // Provide rich-target completion → pipeline resumes → hits
    // EnrichmentDispatchHandler → parks again.
    let mut archived_msg = chat_with_body("alice@example.com/web", "bob@example.com", "orig");
    archived_msg.id = Some(xmpp_parsers::message::Id("orig-msg-1".to_string()));
    let archived = ArchivedMessage {
        stanza_id: StanzaId::new("A1", "alice@example.com".parse::<jid::Jid>().expect("jid")),
        message: Box::new(archived_msg),
        tombstoned: false,
    };
    let events2 = sm.handle(InboundEvent::ArchivedMessageLoaded {
        id: cb_rich,
        result: Some(Box::new(archived)),
    });
    let cb_enrich = events2
        .iter()
        .find_map(|e| match e {
            OutboundEvent::RequestEnrichment { id, .. } => Some(*id),
            _ => None,
        })
        .expect("enrichment parked on resume");

    // Mutate again before the final completion — must still not
    // leak through to the SnapshotProbe.
    let doubly: jid::BareJid = "doubly@example.com".parse().expect("bare");
    sm.blocklist = Blocklist::new([doubly]);

    // Final completion. The resumed pipeline runs through to the
    // SnapshotProbe; the probe's `ctx.blocklist` must reflect the
    // ORIGINAL snapshot, not either mutation.
    let rewritten = chat_with_body(
        "alice@example.com/web",
        "bob@example.com",
        "https://example.com/page",
    );
    sm.handle(InboundEvent::EnrichmentComplete {
        id: cb_enrich,
        message: Box::new(rewritten),
    });

    let captured_blocklists = captured.lock().expect("mutex").clone();
    assert_eq!(
        captured_blocklists.len(),
        1,
        "probe should run exactly once on the final resume"
    );
    let entries: Vec<_> = captured_blocklists[0].iter().cloned().collect();
    assert_eq!(
        entries,
        vec![original],
        "snapshot must be frozen at dispatch start across both pause/resume hops"
    );
}

#[test]
fn set_blocklist_seeds_message_context_snapshot() {
    // Regression for #229 PR13. The transport adapter calls
    // `set_blocklist` once at bind to seed the SM's session-state
    // snapshot from `DatabaseBlockingStorage`. A subsequent
    // dispatch must surface those entries through
    // `MessageContext.blocklist`. Without the seed, the
    // `BlockingFilterHandler` (post-cutover) would read an empty
    // blocklist and silently regress XEP-0191 enforcement.
    use std::sync::Mutex;

    let captured: Arc<Mutex<Vec<Blocklist>>> = Arc::new(Mutex::new(Vec::new()));

    struct SnapshotProbe {
        captured: Arc<Mutex<Vec<Blocklist>>>,
    }
    impl MessageHandler for SnapshotProbe {
        fn name(&self) -> &'static str {
            "set-blocklist-probe"
        }
        fn handle(&self, _message: &mut Message, ctx: &MessageContext<'_>) -> HandlerOutcome {
            self.captured
                .lock()
                .expect("mutex")
                .push(ctx.blocklist.clone());
            HandlerOutcome::Continue(Vec::new())
        }
    }

    let mut dispatcher = StanzaDispatcher::new();
    dispatcher.register_message(Arc::new(SnapshotProbe {
        captured: captured.clone(),
    }));

    let mut sm = test_support::ready_machine("example.com", alice(), dispatcher);

    let blocked: jid::BareJid = "blocked@example.com".parse().expect("bare");
    sm.set_blocklist(Blocklist::new([blocked.clone()]));

    // Drive a dispatch so the probe captures the live snapshot
    // built from `MessageContextEnv { blocklist: &self.blocklist, .. }`.
    let msg = chat_with_body("alice@example.com/web", "bob@example.com", "hello");
    sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
        Stanza::Message(msg),
    ))));

    let snapshots = captured.lock().expect("mutex").clone();
    assert_eq!(
        snapshots.len(),
        1,
        "probe should fire exactly once on the synchronous dispatch"
    );
    let entries: Vec<_> = snapshots[0].iter().cloned().collect();
    assert_eq!(
        entries,
        vec![blocked],
        "MessageContext snapshot must reflect the seeded blocklist"
    );
}

#[test]
fn enrichment_complete_with_unknown_callback_id_logs_warn() {
    let mut sm = test_support::ready_machine("example.com", alice(), StanzaDispatcher::new());
    let events = sm.handle(InboundEvent::EnrichmentComplete {
        id: CallbackId(99999),
        message: Box::new(chat_with_body(
            "alice@example.com/web",
            "bob@example.com",
            "ignored",
        )),
    });
    assert!(events.iter().any(|e| matches!(
        e,
        OutboundEvent::Log { level, .. } if *level == Level::WARN
    )));
}

#[test]
fn oauth_bearer_completion_consumes_pending_op_without_logging() {
    let mut sm = XmppStateMachine::new("waddle.social", StanzaDispatcher::new());
    let id = sm.next_callback_id();
    sm.register_pending_op(id, PendingOp::OAuthBearer);

    let events = sm.handle(InboundEvent::OAuthBearerValidated {
        id,
        result: crate::protocol::event::CallbackResult::Ok { stanza: None },
    });
    assert!(events.is_empty());

    let events2 = sm.handle(InboundEvent::OAuthBearerValidated {
        id,
        result: crate::protocol::event::CallbackResult::Ok { stanza: None },
    });
    assert!(events2.is_empty());
}

/// RFC 6120 §10.3.1 / RFC 6121 §8.1.1.1: a client message with no
/// 'to' is handled on behalf of the sender — treated as addressed to
/// the sender's own bare JID and routed (other resources + archive),
/// not discarded (#1266 item 2).
#[test]
fn rfc6120_to_less_message_routes_to_own_bare_jid() {
    let mut dispatcher = StanzaDispatcher::new();
    dispatcher.register_message(Arc::new(crate::protocol::handlers::route::RouteHandler));
    let mut sm = ready_machine_with_dispatcher(dispatcher, "example.com", alice());

    let mut message = Message::new(None);
    message.type_ = MessageType::Chat;
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), "note".to_string());

    let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
        Stanza::Message(message),
    ))));

    let route = events
        .iter()
        .find_map(|event| match event {
            OutboundEvent::RouteToConnection { jid, stanza, .. } => Some((jid, stanza)),
            _ => None,
        })
        .expect("to-less message must route, not be discarded");
    assert_eq!(route.0.to_string(), "alice@example.com");
    let Stanza::Message(routed) = route.1.as_ref() else {
        panic!("expected message stanza");
    };
    assert_eq!(
        routed.to.as_ref().map(ToString::to_string),
        Some("alice@example.com".to_string()),
        "delivered copy carries the sender's bare JID as 'to'"
    );
}
