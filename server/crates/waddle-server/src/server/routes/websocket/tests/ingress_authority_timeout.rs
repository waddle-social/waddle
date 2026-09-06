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
