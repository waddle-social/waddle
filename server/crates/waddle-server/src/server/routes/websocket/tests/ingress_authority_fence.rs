use super::*;

async fn enabled_connection(state: &WebSocketState) -> WsConnState {
    let mut conn = connection(state, false).await;
    let enable = minidom::Element::builder("enable", waddle_xmpp::stream_management::SM_NS)
        .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
        .build();
    let wire = super::super::super::transport_xml::element_to_xml(enable);
    let frames = handle_xmpp_frame(&wire, "example.com", state, &mut conn).await;
    assert!(frames.iter().any(|frame| frame.contains("enabled")));
    assert!(!conn.sm_state.enabled);
    use super::super::super::batch_write::{
        write_response_batch_with_admission, BatchAuthority, BatchSmPolicy, BatchWriteOutcome,
    };
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("serving permit");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut sink = Box::pin(futures::sink::unfold(
        (),
        |(), _: axum::extract::ws::Message| async { Ok::<(), std::io::Error>(()) },
    ));
    let mut reader =
        futures::stream::pending::<Result<axum::extract::ws::Message, std::io::Error>>();
    let outcome = write_response_batch_with_admission(
        &mut sink,
        &mut reader,
        state,
        &mut conn,
        frames,
        BatchSmPolicy::Record,
        BatchAuthority {
            permit: &permit,
            shutdown: &shutdown,
        },
    )
    .await;
    assert!(matches!(outcome, BatchWriteOutcome::Continue));
    conn.publish_pending_sm_enable(state);
    assert!(conn.sm_state.is_resumable());
    assert!(conn.sm_ingress_fence.is_some());
    conn
}

fn ordinary_message() -> String {
    let mut message =
        xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("recipient")));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message
        .bodies
        .insert(Default::default(), "message after claim loss".into());
    super::super::super::transport_xml::stanza_to_xml(&Stanza::Message(message))
}

async fn assert_no_committed_rows(state: &WebSocketState, conn: &WsConnState) {
    assert_eq!(conn.sm_state.get_inbound_count(), 0);
    assert!(conn.sm_inbound_completion.has_unhandled_hole());
    assert!(conn.sm_state.is_resumable());
    assert!(!conn.sm_recovery_required);
    let db = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("database");
    let mut rows = db.query("SELECT (SELECT COUNT(*) FROM ingress_messages), (SELECT COUNT(*) FROM ingress_sm_refs), (SELECT COUNT(*) FROM ingress_effect_intents), (SELECT COALESCE(MAX(checkpoint_h), 0) FROM ingress_sm_streams), (SELECT COUNT(*) FROM mam_messages), (SELECT COUNT(*) FROM ingress_deliveries)", ()).await.expect("counts");
    let row = rows.next().await.expect("row").expect("counts row");
    for column in 0..6 {
        assert_eq!(row.get::<i64>(column).expect("count"), 0);
    }
}

/// XEP-0198 §4: missing captured authority cannot turn an enabled stream ephemeral.
#[tokio::test]
async fn resumable_missing_captured_fence_is_an_ordinary_hole_sqlite() {
    let state = create_test_websocket_state().await;
    let mut conn = enabled_connection(&state).await;
    conn.sm_ingress_fence = None;
    let frames = handle_xmpp_frame(&ordinary_message(), "example.com", &state, &mut conn).await;
    assert!(frames.is_empty());
    assert_no_committed_rows(&state, &conn).await;
}

/// XEP-0198 §4: demotion between enable and dispatch is rejected by the durable claim assertion.
#[cfg(feature = "clustering")]
#[tokio::test]
async fn demotion_before_message_dispatch_keeps_resumable_fence_postgres() {
    super::recovery::postgres_case(demotion_case).await;
}

#[cfg(feature = "clustering")]
async fn demotion_case(base: Arc<WebSocketState>) {
    use crate::ingress::commit::forced_failures;
    use waddle_xmpp::ownership::{ClaimStore, SharedNodeIdentity};
    let mut conn = enabled_connection(&base).await;
    create_test_session(&base, "bob").await;
    let stream = SmSessionId::new(conn.sm_state.stream_id.clone().expect("stream"));
    let fence = conn.sm_ingress_fence.clone().expect("captured fence");
    let database = base.deps.app_state.db_pool.global().clone();
    let claims = crate::clustering::claims::PostgresClaimStore::new(database.clone());
    claims.ensure_schema().await.expect("claim schema");
    {
        let db = database.guard().await.expect("database");
        db.execute("INSERT INTO clustering_claims (entity, entity_type, node_id, node_epoch, claim_epoch) VALUES (?, ?, ?, ?, ?)", crate::db_params![format!("sm_session:{}", stream.as_str()), "sm_session", fence.owner().node_id.clone(), fence.owner().node_epoch.clone(), fence.epoch().0]).await.expect("claim matching enabled stream");
    }
    let ingress = Arc::new(
        crate::ingress::IngressAuthority::new(
            crate::config::IngressConfig::default(),
            database.clone(),
            crate::ingress::test_lineage_config(),
            Some(SharedNodeIdentity::new(fence.owner().clone())),
        )
        .await
        .expect("clustered ingress"),
    );
    let state = create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            db_pool: Some(base.deps.app_state.db_pool.clone()),
            sm_session_registry: Some(base.deps.protocol.sm_session_registry.clone()),
            ingress: Some(ingress.clone()),
            ..Default::default()
        },
    )
    .await;
    {
        let db = database.guard().await.expect("database");
        db.execute(
            "DELETE FROM clustering_claims WHERE entity = ?",
            crate::db_params![format!("sm_session:{}", stream.as_str())],
        )
        .await
        .expect("durable owner lost");
    }
    state
        .deps
        .protocol
        .sm_session_registry
        .forget_claim_locally(stream.as_str())
        .await;
    assert!(state
        .deps
        .protocol
        .sm_session_registry
        .current_sm_claim_fence(stream.as_str())
        .is_none());
    assert_eq!(conn.sm_ingress_fence.as_ref(), Some(&fence));
    forced_failures::OBSERVED_CLASS
        .scope(std::cell::Cell::new(None), async {
            let frames =
                handle_xmpp_frame(&ordinary_message(), "example.com", &state, &mut conn).await;
            assert!(frames.is_empty());
            assert_eq!(
                forced_failures::OBSERVED_CLASS.with(std::cell::Cell::get),
                Some(crate::ingress::IngressDecisionClass::ClaimFenceMissing)
            );
        })
        .await;
    assert_no_committed_rows(&state, &conn).await;
    assert!(
        ingress
            .drain_and_join(std::time::Duration::from_secs(1))
            .await
    );
}
