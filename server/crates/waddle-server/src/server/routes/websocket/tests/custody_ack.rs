use super::*;
use waddle_xmpp::pending_delivery::storage::InMemoryPendingDeliveryStorage;
use waddle_xmpp::stream_management::persistence::{
    InMemorySmPersistence, IngressCustodyDisposition, SmPersistenceStorage,
};
use waddle_xmpp::stream_management::{
    DetachedSession, SmIngressAppendKey, SmIngressReceiptKind, SmSessionRegistry,
};

async fn wrapped_custody_session() -> (
    Arc<WebSocketState>,
    Arc<InMemorySmPersistence>,
    WsConnState,
    Vec<SmIngressAppendKey>,
) {
    let persistence = Arc::new(InMemorySmPersistence::new());
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence.clone()));
    let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
        registry.clone(),
        Arc::new(InMemoryPendingDeliveryStorage::unlimited()),
    )
    .await;
    let jid: FullJid = "alice@example.com/custody-ack"
        .parse()
        .expect("resource JID");
    let stream = "custody-ack-wrap";
    registry
        .store_session(DetachedSession {
            stream_id: stream.to_owned(),
            user_id: "alice@example.com".to_owned(),
            jid: jid.clone(),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            inbound_count: 0,
            outbound_count: u32::MAX - 1,
            last_acked: u32::MAX - 1,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("persist detached stream");
    let mut keys = Vec::new();
    for (ordinal, sequence) in [u32::MAX, 0, 1].into_iter().enumerate() {
        let key = SmIngressAppendKey {
            message_key: waddle_xmpp::ingress::MessageKey::new(),
            kind: SmIngressReceiptKind::from_storage(3),
            semantic_identity_hash: [ordinal as u8; 32],
            resource: jid.clone(),
        };
        let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(jid.clone())));
        message.id = Some(xmpp_parsers::message::Id(sequence.to_string()));
        registry
            .record_keyed_stanza_for_detached_bound_resource(
                &jid,
                &Stanza::Message(message),
                chrono::Utc::now(),
                key.clone(),
            )
            .await
            .expect("durable keyed append");
        let proof = persistence
            .get_ingress_append(&key)
            .await
            .expect("read custody proof")
            .expect("custody proof");
        assert_eq!(proof.sequence, sequence);
        assert_eq!(proof.disposition, IngressCustodyDisposition::Pending);
        keys.push(key);
    }
    let detached = registry
        .peek_session(stream)
        .await
        .expect("read stream")
        .expect("stream snapshot");
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid, false);
    conn.sm_state.restore_from_session(&detached);
    (state, persistence, conn, keys)
}

#[tokio::test]
async fn custody_ack_completes_only_the_valid_wrapped_window() {
    let (state, persistence, mut conn, keys) = wrapped_custody_session().await;
    let frames = super::super::stream_management::apply_sm_ack(
        state.as_ref(),
        &mut conn.sm_state,
        &mut conn.phase,
        0,
    )
    .await;
    assert!(frames.is_empty());
    assert!(!conn.phase.is_closing());
    assert_eq!(conn.sm_state.last_acked, 0);
    assert_eq!(conn.sm_state.get_stanzas_to_resend(0).len(), 1);
    for (index, key) in keys.iter().enumerate() {
        let proof = persistence
            .get_ingress_append(key)
            .await
            .expect("read proof")
            .expect("proof retained");
        assert_eq!(
            proof.disposition,
            if index < 2 {
                IngressCustodyDisposition::Acknowledged
            } else {
                IngressCustodyDisposition::Pending
            }
        );
    }
    let pending = persistence
        .list_pending_ingress_appends(10)
        .await
        .expect("recovery queue");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].key, keys[2]);
}

#[tokio::test]
async fn custody_ack_rejects_high_count_without_discharging_payloads() {
    let (state, persistence, mut conn, keys) = wrapped_custody_session().await;
    let frames = super::super::stream_management::apply_sm_ack(
        state.as_ref(),
        &mut conn.sm_state,
        &mut conn.phase,
        2,
    )
    .await;
    assert!(!frames.is_empty());
    assert!(conn.phase.is_closing());
    assert_eq!(conn.sm_state.last_acked, u32::MAX - 1);
    assert_eq!(conn.sm_state.get_stanzas_to_resend(u32::MAX - 1).len(), 3);
    let pending = persistence
        .list_pending_ingress_appends(10)
        .await
        .expect("recovery queue");
    assert_eq!(pending.len(), keys.len());
    for key in keys {
        assert!(pending.iter().any(
            |proof| proof.key == key && proof.disposition == IngressCustodyDisposition::Pending
        ));
    }
}

#[tokio::test]
async fn custody_ack_storage_failure_preserves_replay_and_ack_window() {
    let (_, memory, mut conn, keys) = wrapped_custody_session().await;
    let fixture = crate::ingress::test_support::IngressFixture::sqlite().await;
    let persistence = Arc::new(
        crate::sm_persistence::DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("SQL persistence"),
    );
    let stream = waddle_xmpp::pending_delivery::SmSessionId::new("custody-ack-wrap");
    let session = memory
        .get_session(&stream)
        .await
        .expect("session")
        .expect("retained");
    let queue = memory.list_unacked(&stream).await.expect("replay queue");
    for key in &keys {
        let append = memory
            .get_ingress_append(key)
            .await
            .expect("read")
            .expect("custody");
        persistence
            .store_session_atomic_with_ingress_append(session.clone(), queue.clone(), append)
            .await
            .expect("copy durable allocation");
    }
    fixture.db.execute("CREATE TRIGGER fail_custody_ack BEFORE UPDATE ON sm_ingress_appends BEGIN SELECT RAISE(ABORT, 'ack storage unavailable'); END")
        .await.expect("inject failure");
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence.clone()));
    let state = create_test_websocket_state_with_sm_registry_and_pending_storage(
        registry,
        Arc::new(InMemoryPendingDeliveryStorage::unlimited()),
    )
    .await;
    let frames = super::super::stream_management::apply_sm_ack(
        &state,
        &mut conn.sm_state,
        &mut conn.phase,
        0,
    )
    .await;
    assert!(conn.phase.is_closing());
    assert_eq!(frames.len(), 2);
    assert_eq!(conn.sm_state.last_acked, u32::MAX - 1);
    assert_eq!(conn.sm_state.get_stanzas_to_resend(u32::MAX - 1).len(), 3);
    assert_eq!(
        persistence
            .list_pending_ingress_appends(10)
            .await
            .expect("pending custody")
            .len(),
        3
    );
}
