use super::*;

async fn count(state: &WebSocketState, sql: &str) -> i64 {
    let database = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("database");
    let mut rows = database.query(sql, ()).await.expect("query");
    rows.next()
        .await
        .expect("row")
        .expect("count")
        .get(0)
        .expect("integer")
}

async fn ack(state: &WebSocketState, conn: &mut WsConnState, expected: u32) {
    let frames = handle_xmpp_frame(
        &waddle_xmpp::stream_management::SmRequest::to_xml(),
        "example.com",
        state,
        conn,
    )
    .await;
    let frame: minidom::Element = frames.first().expect("ACK").parse().expect("XML");
    assert!(frame.is("a", waddle_xmpp::stream_management::SM_NS));
    assert_eq!(
        frame.attr("h").expect("h").parse::<u32>().expect("count"),
        expected
    );
}

/// XEP-0198 §4–§6; RFC 0018 §2: an external operation timing out cannot
/// revoke the committed handled count or turn it into a retransmission hole.
#[tokio::test]
async fn committed_external_effect_timeout_preserves_wire_ack_and_pending_rows() {
    let state = create_test_websocket_state().await;
    let mut conn = connection(&state, true).await;
    ack(&state, &mut conn, 0).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let external_entered = entered.clone();
    let dispatch_state = state.clone();
    let dispatch = tokio::spawn(async move {
        let wire = offered_message();
        let frames = crate::ingress::ImmediateSink::with_hanging_external(
            external_entered,
            handle_xmpp_frame(&wire, "example.com", &dispatch_state, &mut conn),
        )
        .await;
        (conn, frames)
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
        .await
        .expect("external executor must actually be reached");
    assert!(
        !dispatch.is_finished(),
        "external operation must remain pending"
    );
    assert_eq!(count(&state, "SELECT COUNT(*) FROM ingress_messages WHERE envelope IS NOT NULL AND terminal_at IS NULL").await, 1);
    assert_eq!(
        count(
            &state,
            "SELECT COUNT(*) FROM ingress_sm_refs WHERE wire_h = 1 AND ingress_ordinal = 1"
        )
        .await,
        1
    );
    assert!(count(&state, "SELECT COUNT(*) FROM ingress_effect_intents").await > 0);
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM ingress_effect_receipts").await,
        0
    );
    assert_eq!(
        state
            .deps
            .protocol
            .ingress
            .load_resume_checkpoint(&SmSessionId::new("authority-connection"))
            .await
            .expect("checkpoint")
            .expect("stream")
            .to_storage(),
        1
    );
    let (mut conn, frames) = tokio::time::timeout(std::time::Duration::from_secs(10), dispatch)
        .await
        .expect("Phase-C budget must terminate pending external work")
        .expect("dispatch task");
    assert!(frames.is_empty(), "timed-out error frame was never written");
    assert_eq!(conn.sm_state.get_inbound_count(), 1);
    assert!(!conn.sm_inbound_completion.has_unhandled_hole());
    assert!(conn.sm_state.is_resumable());
    ack(&state, &mut conn, 1).await;
    assert_eq!(
        count(
            &state,
            "SELECT COUNT(*) FROM ingress_messages WHERE terminal_at IS NULL"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM ingress_effect_receipts").await,
        0
    );
}

struct HangingPlanningBlocklist {
    entered: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl waddle_xmpp::xep::xep0191::BlockingStorage for HangingPlanningBlocklist {
    async fn list_blocked_jids(
        &self,
        _user: &jid::BareJid,
    ) -> Result<Vec<jid::BareJid>, waddle_xmpp::xep::xep0191::BlockingStorageError> {
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn ingress_planning_backstop_meters_timeout_once() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let state = create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            blocking_storage: Some(Arc::new(HangingPlanningBlocklist {
                entered: entered.clone(),
            })),
            ..Default::default()
        },
    )
    .await;
    let mut conn = connection(&state, true).await;
    let mut message =
        xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("recipient")));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message
        .bodies
        .insert(Default::default(), "planning timeout".to_owned());
    let wire = super::super::super::transport_xml::stanza_to_xml(&Stanza::Message(message));
    create_test_session(&state, "bob").await;
    let dispatch_state = state.clone();
    let dispatch = tokio::spawn(async move {
        let frames = handle_xmpp_frame(&wire, "example.com", &dispatch_state, &mut conn).await;
        (conn, frames)
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .expect("planning must reach blocklist read");
    assert!(!dispatch.is_finished(), "planning read must be stalled");
    // Pause only while the dispatch is provably blocked, and resume before
    // it does any further I/O: under paused time an idle runtime auto-advances
    // to the next timer, which would expire pool acquisition instantly.
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(16)).await;
    tokio::time::resume();
    let (conn, frames) = dispatch.await.expect("dispatch");
    assert!(frames.is_empty());
    assert_eq!(conn.sm_state.get_inbound_count(), 0);
    assert!(conn.sm_inbound_completion.has_unhandled_hole());
    assert_eq!(
        metrics.counter_sum("ingress.decisions", &[("class", "timeout")]),
        Some(1)
    );
    assert_eq!(metrics.counter_sum("ingress.decisions", &[]), Some(1));
    assert_eq!(
        count(&state, "SELECT COUNT(*) FROM ingress_messages").await,
        0
    );
}

#[tokio::test]
async fn ingress_committed_frame_meters_decision_once() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let state = create_test_websocket_state().await;
    let mut conn = connection(&state, true).await;
    handle_xmpp_frame(&offered_message(), "example.com", &state, &mut conn).await;
    assert_eq!(conn.sm_state.get_inbound_count(), 1);
    assert_eq!(metrics.counter_sum("ingress.decisions", &[]), Some(1));
    assert_eq!(
        metrics
            .counter_sum("ingress.decisions", &[("class", "timeout")])
            .unwrap_or(0),
        0
    );
}

#[tokio::test]
async fn ingress_stalled_commit_backstop_meters_timeout_once() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let state = create_test_websocket_state().await;
    let mut conn = connection(&state, true).await;
    let stream = SmSessionId::new("authority-connection");
    let waiting = state.deps.protocol.ingress.observe_stream_wait();
    let blocked = state.deps.protocol.ingress.block_test_stream(&stream).await;
    let dispatch_state = state.clone();
    let dispatch = tokio::spawn(async move {
        let frames = handle_xmpp_frame(
            &offered_message(),
            "example.com",
            &dispatch_state,
            &mut conn,
        )
        .await;
        (conn, frames)
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), waiting.notified())
        .await
        .expect("commit must reach the stream lock wait");
    assert!(
        !dispatch.is_finished(),
        "commit must be stalled on the stream lock"
    );
    // Pause only while the commit is provably blocked on the lock, and resume
    // before the dispatch does any further I/O (see the planning test).
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(16)).await;
    tokio::time::resume();
    let (conn, frames) = dispatch.await.expect("dispatch");
    assert!(frames.is_empty());
    assert_eq!(conn.sm_state.get_inbound_count(), 0);
    assert!(conn.sm_inbound_completion.has_unhandled_hole());
    assert_eq!(
        metrics.counter_sum("ingress.decisions", &[("class", "timeout")]),
        Some(1)
    );
    assert_eq!(metrics.counter_sum("ingress.decisions", &[]), Some(1));
    drop(blocked);
}
