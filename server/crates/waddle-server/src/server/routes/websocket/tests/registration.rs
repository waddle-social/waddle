use super::super::{
    frame::handle_xmpp_frame,
    registration::{
        publish_stream_id_and_presence, register_bound_connection_after_frame,
        RegistrationAfterFrame,
    },
    session_init::load_blocklist_for_bind,
    state::WsConnState,
    stream_management::SmRegistrationFinalization,
    transport_xml::element_to_xml,
};
use super::{create_test_session, create_test_websocket_state};
use jid::{BareJid, FullJid};
use std::sync::Arc;
use tokio::sync::mpsc;
use waddle_xmpp::{
    protocol::{Blocklist, ConnectionPhase, InboundEvent, InboundFrame, StanzaDispatcher},
    registry::OutboundStanza,
    stream_management::SM_NS,
    Stanza,
};
use xmpp_parsers::message::MessageType as XmppMessageType;
use xmpp_parsers::minidom::Element;

#[tokio::test]
async fn ensure_state_machine_initializes_sm_in_ready_phase() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: jid::FullJid = "alice@example.com/web".parse().expect("jid");

    assert!(
        conn.state_machine.is_none(),
        "fresh WsConnState has no state machine"
    );

    conn.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        jid.clone(),
        false,
        Blocklist::empty(),
    );

    let sm = conn.state_machine.as_ref().expect("SM initialized");
    assert!(matches!(sm.phase(), ConnectionPhase::Ready { .. }));
    assert_eq!(sm.phase().bound_jid(), Some(&jid));
}

#[tokio::test]
async fn register_bound_connection_after_frame_registers_ready_connection_once() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let (tx, _rx) = mpsc::channel::<OutboundStanza>(1);
    let mut pending_tx = Some(tx);

    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.carbons_enabled = true;
    conn.roster_interested = true;
    conn.presence_available = true;
    conn.presence_show = Some(xmpp_parsers::presence::Show::Chat);
    conn.presence_status = Some("ready".to_string());
    conn.presence_priority = 7;

    let result = register_bound_connection_after_frame(
        state.as_ref(),
        "example.com",
        &mut conn,
        &mut pending_tx,
    )
    .await;

    assert!(matches!(
        result,
        RegistrationAfterFrame::Registered(SmRegistrationFinalization::KeepExistingResponses)
    ));
    assert!(
        pending_tx.is_none(),
        "registration consumes the one-shot sender"
    );
    assert!(
        conn.registry_owner.is_some(),
        "registry ownership is tracked"
    );

    let sm = conn
        .state_machine
        .as_ref()
        .expect("state machine initialized");
    assert!(matches!(sm.phase(), ConnectionPhase::Ready { .. }));
    assert_eq!(sm.phase().bound_jid(), Some(&jid));

    let entry = state
        .deps
        .protocol
        .connection_registry
        .get_entry(&jid)
        .expect("registered connection");
    assert!(entry.is_carbons_enabled());
    assert!(entry
        .roster_interested
        .load(std::sync::atomic::Ordering::Relaxed));
    assert!(entry.is_presence_available());
    assert_eq!(entry.presence_priority(), 7);

    let presence = state
        .deps
        .protocol
        .connection_registry
        .get_presence_state(&jid)
        .expect("presence state restored");
    assert_eq!(presence.show.as_deref(), Some("chat"));
    assert_eq!(presence.status.as_deref(), Some("ready"));
    assert_eq!(presence.priority, 7);

    let second = register_bound_connection_after_frame(
        state.as_ref(),
        "example.com",
        &mut conn,
        &mut pending_tx,
    )
    .await;
    assert!(matches!(second, RegistrationAfterFrame::Unchanged));
    assert_eq!(
        state.deps.protocol.connection_registry.connection_count(),
        1
    );
}

#[tokio::test]
async fn register_bound_connection_after_frame_completes_pending_resume_claim() {
    use waddle_xmpp::stream_management::{
        DetachedSession, DetachedUnackedStanza, SmSessionRegistry,
    };

    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let stream_id = "registration-resume-stream".to_string();
    let session = create_test_session(state.as_ref(), "alice").await;

    state
        .deps
        .protocol
        .resumable_sessions
        .insert(stream_id.clone(), session.clone());
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: stream_id.clone(),
            user_id: session.user_jid.clone(),
            jid: jid.clone(),
            inbound_count: 4,
            outbound_count: 10,
            last_acked: 8,
            replay_gap_through: None,
            unacked_stanzas: vec![
                DetachedUnackedStanza {
                    sequence: 9,
                    stanza_xml: "<message id='m9'/>".to_string(),
                    original_receipt_at: chrono::Utc::now(),
                },
                DetachedUnackedStanza {
                    sequence: 10,
                    stanza_xml: "<message id='m10'/>".to_string(),
                    original_receipt_at: chrono::Utc::now(),
                },
            ],
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: true,
            roster_interested: true,
            blocklist_interested: false,
            presence_available: true,
            presence_show: Some(xmpp_parsers::presence::Show::Chat),
            presence_status: Some("back".to_string()),
            presence_priority: 5,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached session");

    conn.phase = ConnectionPhase::authenticated(&jid);
    conn.authenticated_session = Some(session.clone());
    let resume_frame = element_to_xml(
        Element::builder("resume", SM_NS)
            .attr(
                minidom::rxml::xml_ncname!("previd").to_owned(),
                stream_id.as_str(),
            )
            .attr(minidom::rxml::xml_ncname!("h").to_owned(), "9")
            .build(),
    );
    let resume_responses =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut conn).await;

    assert!(!resume_responses.is_empty());
    assert_eq!(
        conn.pending_resume_stream_id.as_deref(),
        Some(stream_id.as_str())
    );
    assert_eq!(conn.pending_resume_h, Some(9));
    assert!(conn.suppress_sm_record_next_batch);

    let (tx, _rx) = mpsc::channel::<OutboundStanza>(1);
    let mut pending_tx = Some(tx);
    let result = register_bound_connection_after_frame(
        state.as_ref(),
        "example.com",
        &mut conn,
        &mut pending_tx,
    )
    .await;

    match result {
        RegistrationAfterFrame::Registered(SmRegistrationFinalization::ReplaceWithResumed {
            resumed,
            replay_after_h,
        }) => {
            assert_eq!(resumed.previd.as_str(), stream_id.as_str());
            assert_eq!(resumed.h, 4);
            assert_eq!(replay_after_h, 9);
        }
        _ => panic!("expected resumed finalization"),
    }

    assert!(
        pending_tx.is_none(),
        "registration consumes the one-shot sender"
    );
    assert!(conn.pending_resume_stream_id.is_none());
    assert!(conn.pending_resume_h.is_none());
    let replay = conn.sm_state.get_stanzas_to_resend(9);
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].stanza_xml, "<message id='m10'/>");
    assert!(state
        .deps
        .protocol
        .resumable_sessions
        .get(&stream_id)
        .is_none());
    assert!(state
        .deps
        .protocol
        .sm_session_registry
        .peek_session(&stream_id)
        .await
        .expect("peek detached session")
        .is_none());

    let entry = state
        .deps
        .protocol
        .connection_registry
        .get_entry(&jid)
        .expect("registered resumed connection");
    assert!(entry.is_carbons_enabled());
    assert!(entry
        .roster_interested
        .load(std::sync::atomic::Ordering::Relaxed));
    assert!(entry.is_presence_available());
    assert_eq!(entry.presence_priority(), 5);

    let presence = state
        .deps
        .protocol
        .connection_registry
        .get_presence_state(&jid)
        .expect("resumed presence state restored");
    assert_eq!(presence.show.as_deref(), Some("chat"));
    assert_eq!(presence.status.as_deref(), Some("back"));
    assert_eq!(presence.priority, 5);
}

#[tokio::test]
async fn replay_gap_during_resume_finalization_clears_blocklist_interest_for_fresh_bind() {
    use waddle_xmpp::stream_management::{
        DetachedSession, SmSessionRegistry, DEFAULT_MAX_UNACKED_QUEUE_SIZE,
    };

    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let stream_id = "registration-resume-gap-stream".to_string();
    let session = create_test_session(state.as_ref(), "alice").await;

    state
        .deps
        .protocol
        .resumable_sessions
        .insert(stream_id.clone(), session.clone());
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: stream_id.clone(),
            user_id: session.user_jid.clone(),
            jid: jid.clone(),
            inbound_count: 4,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: true,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached session");

    conn.phase = ConnectionPhase::authenticated(&jid);
    conn.authenticated_session = Some(session);
    let resume_frame = element_to_xml(
        Element::builder("resume", SM_NS)
            .attr(
                minidom::rxml::xml_ncname!("previd").to_owned(),
                stream_id.as_str(),
            )
            .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
            .build(),
    );
    let resume_responses =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut conn).await;

    assert!(!resume_responses.is_empty());
    assert!(
        conn.blocklist_interested,
        "resume should restore the detached stream's blocklist interest before finalization"
    );

    for index in 0..=DEFAULT_MAX_UNACKED_QUEUE_SIZE {
        let mut message = xmpp_parsers::message::Message::new(None::<jid::Jid>);
        message.id = Some(xmpp_parsers::message::Id(format!("gap-{index}")));
        state
            .deps
            .protocol
            .sm_session_registry
            .record_stanza_for_detached_bound_resource(
                &jid,
                &Stanza::Message(message),
                chrono::Utc::now(),
            )
            .await
            .expect("record into claimed session");
    }

    let (tx, _rx) = mpsc::channel::<OutboundStanza>(1);
    let mut pending_tx = Some(tx);
    let result = register_bound_connection_after_frame(
        state.as_ref(),
        "example.com",
        &mut conn,
        &mut pending_tx,
    )
    .await;

    assert!(matches!(
        result,
        RegistrationAfterFrame::Registered(SmRegistrationFinalization::ReplaceWithFailed(_))
    ));
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
    assert!(
        !conn.blocklist_interested,
        "failed resume reset must not make the following fresh bind blocklist-interested"
    );
}

#[tokio::test]
async fn ensure_state_machine_seeds_blocklist_from_database_at_bind() {
    // #229 PR13: bind-time SM seeding from
    // `DatabaseBlockingStorage`. Persist a single blocked entry
    // for alice, run the bind-time loader against the same
    // global pool, hand the result to `ensure_state_machine`,
    // then drive a synchronous dispatch through a probe handler
    // and observe the seeded entry on the `MessageContext`
    // snapshot. Without the seed, the snapshot would be
    // `Blocklist::empty()` and `BlockingFilterHandler` (post
    // PR16 cutover) would silently regress XEP-0191 enforcement.
    use crate::db::blocking::DatabaseBlockingStorage;
    use std::sync::Mutex;
    use waddle_xmpp::protocol::{HandlerOutcome, MessageContext, MessageHandler};

    let state = create_test_websocket_state().await;
    let alice_full: jid::FullJid = "alice@example.com/web".parse().expect("jid");
    let alice_bare = alice_full.to_bare();
    let blocked_bare: BareJid = "blocked@example.com".parse().expect("bare");

    // Seed persistence with one entry.
    let storage = DatabaseBlockingStorage::new(state.deps.app_state.db_pool.global().clone());
    storage
        .add_blocks(&alice_bare, &[blocked_bare.clone().into()])
        .await
        .expect("add_blocks");

    // Mirror the bind-site loader.
    let blocklist = load_blocklist_for_bind(&state.deps.app_state.db_pool, &alice_full)
        .await
        .expect("blocklist load succeeds when storage is healthy");
    let loaded: Vec<_> = blocklist.iter().cloned().collect();
    let expected: jid::Jid = blocked_bare.clone().into();
    assert_eq!(loaded, vec![expected]);

    // Build a probe-only dispatcher so the assertion isolates
    // the SM seeding behaviour from any side effects of the
    // production message-pipeline chain (those have their own
    // dedicated tests). The goal here is "the seeded blocklist
    // shows up on the `MessageContext` snapshot".
    let captured: Arc<Mutex<Vec<waddle_xmpp::protocol::Blocklist>>> =
        Arc::new(Mutex::new(Vec::new()));
    struct SnapshotProbe {
        captured: Arc<Mutex<Vec<waddle_xmpp::protocol::Blocklist>>>,
    }
    impl MessageHandler for SnapshotProbe {
        fn name(&self) -> &'static str {
            "ws-bind-blocklist-probe"
        }
        fn handle(
            &self,
            _message: &mut xmpp_parsers::message::Message,
            ctx: &MessageContext<'_>,
        ) -> HandlerOutcome {
            self.captured
                .lock()
                .expect("mutex")
                .push(ctx.blocklist.clone());
            HandlerOutcome::Continue(Vec::new())
        }
    }
    let mut probe_dispatcher = StanzaDispatcher::new();
    probe_dispatcher.register_message(Arc::new(SnapshotProbe {
        captured: captured.clone(),
    }));
    let dispatcher = Arc::new(probe_dispatcher);

    let mut conn = WsConnState::new();
    conn.ensure_state_machine(
        "example.com",
        &dispatcher,
        alice_full.clone(),
        false,
        blocklist,
    );

    // Drive a chat message dispatch so the probe fires.
    let mut msg =
        xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("to jid")));
    msg.from = Some(jid::Jid::from(alice_full.clone()));
    msg.type_ = XmppMessageType::Chat;
    msg.bodies
        .insert(xmpp_parsers::message::Lang::new(), "hello".to_string());
    let sm = conn.state_machine.as_mut().expect("SM");
    sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
        Stanza::Message(msg),
    ))));

    let snapshots = captured.lock().expect("mutex").clone();
    assert_eq!(snapshots.len(), 1, "probe runs exactly once");
    let entries: Vec<_> = snapshots[0].iter().cloned().collect();
    assert_eq!(
        entries,
        vec![blocked_bare],
        "MessageContext snapshot must reflect the persisted blocklist"
    );
}

/// Concurrency review F1: after `register_with_stream_state` returns, a
/// racing same-JID replacement can take the registry slot before the
/// registering connection publishes its SM stream id and restored
/// presence. That publication runs with the OLD connection's (now
/// stale) owner token and must be owner-gated: a stale-owner
/// publication must not stamp its stream id onto the replacement's
/// entry nor overwrite the JID-keyed presence map with its restored
/// presence.
#[tokio::test]
async fn stale_owner_publication_does_not_stamp_replacement_entry() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");

    // Connection A registers and receives its owner token.
    let (tx_a, _rx_a) = mpsc::channel::<OutboundStanza>(4);
    let stale_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx_a);

    // While A still owns the slot, its publication takes effect — the
    // happy path.
    let mut conn_a = WsConnState::new();
    conn_a.sm_state.stream_id = Some("a-stream".to_string());
    conn_a.presence_available = true;
    conn_a.presence_show = Some(xmpp_parsers::presence::Show::Chat);
    conn_a.presence_status = Some("a-status".to_string());
    conn_a.presence_priority = 9;
    publish_stream_id_and_presence(state.as_ref(), &jid, &stale_owner, &conn_a);
    let entry = state
        .deps
        .protocol
        .connection_registry
        .get_entry(&jid)
        .expect("A's entry");
    assert_eq!(
        entry.sm_stream_id(),
        Some(waddle_xmpp::pending_delivery::SmSessionId::new("a-stream")),
        "owner's own publication must land"
    );

    // A same-JID replacement supersedes A and publishes ITS stream id
    // and presence.
    let (tx_b, _rx_b) = mpsc::channel::<OutboundStanza>(4);
    let _replacement_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx_b);
    let repl_entry = state
        .deps
        .protocol
        .connection_registry
        .get_entry(&jid)
        .expect("replacement entry");
    repl_entry.set_sm_stream_id(Some(waddle_xmpp::pending_delivery::SmSessionId::new(
        "replacement-stream",
    )));
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&jid, true, 3);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence_state(
            &jid,
            Some("dnd".to_string()),
            Some("busy".to_string()),
            3,
            Vec::new(),
        );

    // A's registration-completion publication now fires with its STALE
    // owner token (the mid-register race): it must be a no-op.
    publish_stream_id_and_presence(state.as_ref(), &jid, &stale_owner, &conn_a);

    let entry = state
        .deps
        .protocol
        .connection_registry
        .get_entry(&jid)
        .expect("replacement entry");
    assert_eq!(
        entry.sm_stream_id(),
        Some(waddle_xmpp::pending_delivery::SmSessionId::new(
            "replacement-stream"
        )),
        "a stale-owner publication must not stamp its stream id onto the replacement's entry"
    );
    assert_eq!(
        entry.presence_priority(),
        3,
        "a stale-owner publication must not overwrite the replacement's presence availability"
    );
    let presence = state
        .deps
        .protocol
        .connection_registry
        .get_presence_state(&jid)
        .expect("replacement presence state");
    assert_eq!(
        (
            presence.show.as_deref(),
            presence.status.as_deref(),
            presence.priority
        ),
        (Some("dnd"), Some("busy"), 3),
        "a stale-owner publication must not overwrite the JID-keyed presence map"
    );
}

/// #1454: a failed authoritative registration used to send the client a
/// stream `<internal-server-error/>` with NO server-side log or metric —
/// 32 client-side Faro events over 7 days against zero server log lines.
/// Drive the real failure path (kill the `UserRegistryActor` so the mirror
/// ask fails, forcing the ADR-0017 fail-closed rollback) and assert the
/// failure now logs at `error!` with typed context and increments the
/// alertable `waddle.session.init.failed` counter. The span-error mark
/// shares the centrally-tested `mark_span_error` helper; asserting its
/// export here would race actor scheduling (#1479), so it is deliberately
/// not export-asserted in this actor-heavy scope.
#[tokio::test(flavor = "current_thread")]
async fn failed_authoritative_registration_logs_and_increments_counter() {
    use std::sync::{Arc as StdArc, Mutex};

    #[derive(Clone, Default)]
    struct CaptureWriter(StdArc<Mutex<Vec<u8>>>);

    impl std::io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("capture buffer lock")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let buffer = StdArc::new(Mutex::new(Vec::new()));
    let _subscriber = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::ERROR)
            .with_writer(CaptureWriter(buffer.clone()))
            .finish(),
    );

    let state = create_test_websocket_state().await;
    // Kill the authoritative registry so `mirror_register_outcome`'s ask
    // fails — exactly the rollback path that was server-silent.
    state.deps.protocol.user_registry.kill();
    state.deps.protocol.user_registry.wait_for_shutdown().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    let (tx, _rx) = mpsc::channel::<OutboundStanza>(1);
    let mut pending_tx = Some(tx);

    let outcome = register_bound_connection_after_frame(
        state.as_ref(),
        "example.com",
        &mut conn,
        &mut pending_tx,
    )
    .await;

    assert!(
        matches!(outcome, RegistrationAfterFrame::SessionInitializationFailed),
        "a failed authoritative registration must fail the bind"
    );
    assert!(
        conn.registry_owner.is_none(),
        "the rollback must clear the registry owner"
    );
    assert_eq!(
        metrics.counter_sum(
            "waddle.session.init.failed",
            &[("reason", "authoritative_registration")]
        ),
        Some(1),
        "the failure must increment the alertable counter exactly once"
    );
    let logs = String::from_utf8(buffer.lock().expect("capture buffer lock").clone())
        .expect("captured logs are valid UTF-8");
    assert!(
        logs.contains("session initialization failed")
            && logs.contains("alice@example.com")
            && logs.contains("authoritative_registration"),
        "the failure must be logged at error! with the user and typed reason. Captured:\n{logs}"
    );
}

/// ADR-0017 Phase 1: the dominant (non-SM) disconnect teardown mirrors the
/// unregister into the actor tree, so a bound-then-closed connection does not
/// leak its resource — and the empty `UserActor` is pruned. Regression test
/// for the register/unregister lock-step invariant claimed in
/// `ProtocolServices::user_registry`.
#[tokio::test]
async fn cleanup_connection_shutdown_mirrors_unregister_into_actor_tree() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(1);
    let mut pending_tx = Some(tx);

    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.presence_available = true;

    let result = register_bound_connection_after_frame(
        state.as_ref(),
        "example.com",
        &mut conn,
        &mut pending_tx,
    )
    .await;
    assert!(matches!(result, RegistrationAfterFrame::Registered(_)));

    // The bind mirrored the resource into the actor tree.
    let user = state
        .deps
        .protocol
        .user_registry
        .ask(waddle_xmpp::registry::GetUser {
            bare_jid: jid.to_bare(),
        })
        .await
        .expect("get user")
        .expect("actor tree tracks the bound resource");
    let resources: Vec<FullJid> = user
        .ask(waddle_xmpp::registry::user_actor::GetResources)
        .await
        .expect("resources");
    assert_eq!(resources, vec![jid.clone()]);

    // The default (non-SM, non-resumable) teardown takes the full-cleanup
    // branch, which must mirror the unregister.
    let _ = super::super::cleanup::cleanup_connection_shutdown(
        state.as_ref(),
        &mut rx,
        &mut conn,
        false,
    )
    .await;

    // DashMap authoritative registry dropped the entry...
    assert!(
        state
            .deps
            .protocol
            .connection_registry
            .get_entry(&jid)
            .is_none(),
        "authoritative registry unregistered the resource"
    );
    // ...and the actor tree mirror pruned the now-empty user.
    let after = state
        .deps
        .protocol
        .user_registry
        .ask(waddle_xmpp::registry::GetUser {
            bare_jid: jid.to_bare(),
        })
        .await
        .expect("get user");
    assert!(
        after.is_none(),
        "teardown must mirror the unregister so the actor tree prunes the user"
    );
}
