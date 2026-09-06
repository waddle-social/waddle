//! XEP-0198 §4: the failed ordered-relay ask follows committed responsibility.
use super::*;
use crate::clustering::{
    ordered_relay::OrderedRelayPayload,
    route_bridge::{
        tests::services_with_claims, OrderedRelayDeliveryBridge, TEST_CANCELLED_ENVELOPES,
    },
    ClusteringHandles,
};
use crate::ingress_uow::{CanonicalMessageRepository, EffectIntentRepository};
use waddle_xmpp::{
    ingress::{IngressEffectIntent, MessageKey},
    ownership::{Entity, EntityType, NodeIdentity},
};

/// XEP-0198 §4–§6: a failed relay ask leaves a durable obligation and h=1, with no client retry.
#[tokio::test]
async fn failed_ordered_room_ask_after_commit_preserves_ack_sqlite() {
    failed_ordered_room_ask(create_test_websocket_state().await).await;
}

/// XEP-0198 §4–§6: PostgreSQL has the same post-commit relay responsibility contract.
#[tokio::test]
async fn failed_ordered_room_ask_after_commit_preserves_ack_postgres() {
    super::recovery::postgres_case(failed_ordered_room_ask).await;
}

async fn failed_ordered_room_ask(base: Arc<WebSocketState>) {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let local = NodeIdentity::new("local-node", "local-epoch");
    let remote = NodeIdentity::new("remote-node", "remote-epoch");
    let keypair = libp2p::identity::Keypair::generate_ed25519();
    let mut services = services_with_claims(
        local.clone(),
        remote.clone(),
        local.clone(),
        keypair.public().to_peer_id().to_string(),
    )
    .await;
    let room: jid::BareJid = "lost-relay@muc.example.com".parse().expect("room");
    for (entity, owner) in [
        (
            Entity::new(EntityType::RoomActor, room.to_string()),
            &remote,
        ),
        (
            Entity::new(EntityType::UserActor, "alice@example.com"),
            &local,
        ),
        (
            Entity::new(EntityType::SmSession, "authority-connection"),
            &local,
        ),
    ] {
        services
            .claim_store
            .acquire(&entity, owner)
            .await
            .expect("relay claims");
    }
    let stopped = tokio_util::sync::CancellationToken::new();
    stopped.cancel();
    let bridge = OrderedRelayDeliveryBridge::new(
        stopped,
        &crate::config::ClusteringMessagingConfig::default(),
    );
    bridge.wire_origin_signer(keypair);
    let state = create_test_websocket_state_with_extension_manager(
        empty_extension_manager().await,
        TestStateOverrides {
            db_pool: Some(base.deps.app_state.db_pool.clone()),
            clustering: Some(ClusteringHandles {
                claim_store: Some(services.claim_store.clone()),
                node_identity: Some(services.node_identity.clone()),
                ordered_relay_delivery_bridge: Some(bridge.clone()),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await;
    services.web_socket_state = Arc::downgrade(&state);
    bridge.wire(Arc::new(services));
    let mut conn = connection(&state, true).await;
    assert_ack(&state, &mut conn, 0).await;
    let mut message = xmpp_parsers::message::Message::new(Some(room.clone().into()));
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message
        .bodies
        .insert(Default::default(), "committed remote body".into());
    waddle_xmpp_core::xep0359::add_origin_id(&mut message, "lost-room-ask");
    let wire = super::super::super::transport_xml::stanza_to_xml(&Stanza::Message(message.clone()));
    let cancelled = TEST_CANCELLED_ENVELOPES
        .scope(std::cell::RefCell::new(Vec::new()), async {
            let frames = handle_xmpp_frame(&wire, "example.com", &state, &mut conn).await;
            assert!(frames.is_empty(), "failed room ask has no reply frame");
            TEST_CANCELLED_ENVELOPES.with(|captured| captured.take())
        })
        .await;
    assert_eq!(
        cancelled.len(),
        1,
        "must reach exactly one actual cancelled ordered ask"
    );
    let offered = &cancelled[0];
    assert!(
        offered.origin_proof.is_some(),
        "actual ask carries signed provenance"
    );
    let wire_payload = serde_json::to_vec(&offered.payload).expect("relay wire");
    let decoded: OrderedRelayPayload = serde_json::from_slice(&wire_payload).expect("relay decode");
    let OrderedRelayPayload::MucProxy {
        canonical: Some(canonical),
        room_jid,
        stanza,
        ..
    } = decoded
    else {
        panic!("canonical MUC relay envelope");
    };
    assert_eq!(room_jid, room);
    let Stanza::Message(relayed) = stanza.0 else {
        panic!("relay message");
    };
    assert_eq!(relayed.bodies, message.bodies);
    assert_eq!(conn.sm_state.get_inbound_count(), 1);
    assert!(!conn.sm_inbound_completion.has_unhandled_hole());
    assert_ack(&state, &mut conn, 1).await;
    assert_durable(&state, canonical.message_key, &room, &relayed).await;
    assert!(metrics
        .counter_sum("ingress.effects.unresolved", &[("kind", "room")])
        .is_some_and(|count| count >= 1));
}

async fn assert_ack(state: &WebSocketState, conn: &mut WsConnState, expected: u32) {
    let frames = handle_xmpp_frame(
        &waddle_xmpp::stream_management::SmRequest::to_xml(),
        "example.com",
        state,
        conn,
    )
    .await;
    let xml: minidom::Element = frames.first().expect("ACK").parse().expect("ACK XML");
    assert!(xml.is("a", waddle_xmpp::stream_management::SM_NS));
    assert_eq!(
        xml.attr("h").expect("h").parse::<u32>().expect("count"),
        expected
    );
}

async fn assert_durable(
    state: &WebSocketState,
    key: MessageKey,
    room: &jid::BareJid,
    message: &xmpp_parsers::message::Message,
) {
    let uow = crate::ingress_uow::IngressUnitOfWork::open(
        state.deps.app_state.db_pool.global().clone(),
        crate::ingress::test_lineage_config(),
    )
    .expect("read UoW");
    let mut tx = uow.begin().await.expect("read authority rows");
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("envelope")
        .expect("durable envelope");
    assert_eq!(
        envelope,
        crate::ingress_substrate::MessageEnvelope::new(message.clone())
    );
    let intents = EffectIntentRepository::load(&mut tx, key)
        .await
        .expect("intents");
    assert!(intents.iter().any(|intent| matches!(intent, IngressEffectIntent::DispatchToRoomRemote { room: recorded, .. } if recorded == room)));
    tx.commit().await.expect("close read");
    let database = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("database");
    for (sql, expected) in [
        ("SELECT COUNT(*) FROM ingress_messages WHERE envelope IS NOT NULL AND terminal_at IS NULL", 1_i64),
        ("SELECT COUNT(*) FROM ingress_effect_receipts", 0),
        ("SELECT COUNT(*) FROM ingress_sm_streams WHERE checkpoint_h = 1", 1),
        ("SELECT COUNT(*) FROM ingress_sm_refs WHERE wire_h = 1 AND ingress_ordinal = 1", 1),
    ] {
        let mut rows = database.query(sql, ()).await.expect("rows");
        assert_eq!(rows.next().await.expect("row").expect("count").get::<i64>(0).expect("integer"), expected);
    }
}
