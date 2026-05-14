use super::super::{
    registration::{register_bound_connection_after_frame, RegistrationAfterFrame},
    session_init::load_blocklist_for_bind,
    stream_management::SmRegistrationFinalization,
};
use super::*;

// ---------------------------------------------------------------
// #229 PR11 — DeliveryKind dispatch in the per-connection main loop
// ---------------------------------------------------------------
//
// The actual main-loop entry point is `xmpp_websocket_handler`, an
// async function tied to a real WebSocket sink. To test the
// dispatch logic in isolation we exercise its two helpers
// (`build_interpret_deps`, `drive_interpret_loop`) and the
// `WsConnState::ensure_state_machine` lifecycle directly. End-to-
// end coverage of the routing flow lands once PR12 emits
// `OutboundStanza::peer_stanza` from `RouteToConnection`.

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
            user_id: session.user_id.clone(),
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
            presence_available: true,
            presence_show: Some(xmpp_parsers::presence::Show::Chat),
            presence_status: Some("back".to_string()),
            presence_priority: 5,
        })
        .await
        .expect("store detached session");

    conn.phase = ConnectionPhase::authenticated(&jid);
    conn.authenticated_session = Some(session.clone());
    let resume_frame = format!("<resume xmlns='urn:xmpp:sm:3' previd='{stream_id}' h='9'/>");
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
    assert_eq!(
        conn.sm_state.get_stanzas_to_resend(9),
        vec!["<message id='m10'/>".to_string()]
    );
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
        .add_blocks(&alice_bare, &[blocked_bare.to_string()])
        .await
        .expect("add_blocks");

    // Mirror the bind-site loader.
    let blocklist = load_blocklist_for_bind(&state.deps.app_state.db_pool, &alice_full)
        .await
        .expect("blocklist load succeeds when storage is healthy");
    let loaded: Vec<_> = blocklist.iter().cloned().collect();
    assert_eq!(loaded, vec![blocked_bare.clone()]);

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
    msg.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("hello".to_string()),
    );
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
