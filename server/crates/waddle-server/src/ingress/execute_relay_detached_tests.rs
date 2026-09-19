//! A frozen remote route retains its receipt identity when execution falls back locally.
use super::*;
use crate::ingress::{commit::commit_submission, test_support::IngressFixture};
use crate::server::routes::interpret::DeliveryExecutionContext;
use std::sync::Arc;
use waddle_xmpp::{
    ingress::{EffectMessageIdentity, IngressEffectIntent},
    stream_management::{
        DetachedSession, InMemorySmSessionRegistry, SmIngressAppendKey, SmIngressReceiptKind,
        SmSessionRegistry,
    },
};

async fn relay_fallback_receipt_failure(fixture: IngressFixture, cross_node: bool) {
    let persistence = Arc::new(
        crate::sm_persistence::DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("SM persistence"),
    );
    let sm = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence));
    let recipient: jid::FullJid = "juliet@example.com/phone".parse().expect("recipient");
    sm.store_session(DetachedSession {
        stream_id: recipient.to_string(),
        user_id: recipient.to_bare().to_string(),
        jid: recipient.clone(),
        occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        inbound_count: 0,
        outbound_count: 0,
        last_acked: 0,
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
    .expect("store detached session");
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    #[cfg(feature = "clustering")]
    let receiver = if cross_node {
        Some(Arc::new(
            CrossNodeReceiver::new(&fixture, Arc::clone(&sm), &recipient).await,
        ))
    } else {
        None
    };
    let mut deps = Deps::new(&state.deps.protocol.connection_registry, "example.com");
    deps.user_registry = Some(&state.deps.protocol.user_registry);
    deps.sm_session_registry = Some(&sm);
    #[cfg(feature = "clustering")]
    if let Some(receiver) = &receiver {
        deps.web_socket_state = Some(&receiver.origin_state);
    }

    let mut submission = fixture.submission(Some("relay-detached-retry"), "remote planned DM");
    let mut delivered_message = submission.plan.sanitized_message.clone();
    if cross_node {
        delivered_message.to = Some(recipient.clone().into());
    }
    let identity = EffectMessageIdentity::capture_ordinal(7);
    let intent = IngressEffectIntent::RouteDirect {
        recipient: recipient.to_bare(),
        fanout: vec![recipient.clone()],
        route_identity: identity.clone(),
    };
    let receipt = crate::ingress::receipt_key(&intent).expect("recorded route receipt");
    submission.plan.intents = vec![intent];
    // Phase A selected the remote relay. The original case falls back locally;
    // the cross-node case executes the receiving node's actual reservation and append.
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(
        ExternalEffect::Delivery(ExternalDeliveryEffect::RelayFullJid {
            route_identity: Some(identity),
            origin: {
                #[cfg(feature = "clustering")]
                if cross_node {
                    let sender = waddle_xmpp::ownership::Entity::new(
                        waddle_xmpp::ownership::EntityType::UserActor,
                        submission.sender.to_bare().to_string(),
                    );
                    Some(crate::server::routes::interpret::OrderedRelayRouteOrigin {
                        kind: crate::server::routes::interpret::OrderedRelayRouteOriginKind::Entity(
                            sender.clone(),
                        ),
                        sender_entity: sender,
                        inbound_sequence: 1,
                        handoff: None,
                    })
                } else {
                    None
                }
                #[cfg(not(feature = "clustering"))]
                {
                    None
                }
            },
            target: recipient.clone(),
            stanza: Box::new(Stanza::Message(delivered_message)),
            call_setup: None,
        }),
    ))];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit frozen remote route");
    let message_key = decision.message_key.expect("canonical message key");
    assert_eq!(decision.external_receipts[0], vec![receipt.clone()]);
    assert!(
        decision.arm_owned_receipts.is_empty(),
        "relay settles generically"
    );
    let hash = hex::encode(receipt.semantic_identity_hash);
    // This SQL represents stored receipt bytes, not XML or protocol payloads.
    match fixture.db.driver() {
        crate::db::DatabaseDriver::Sqlite => fixture.execute(&format!("CREATE TRIGGER fail_relay_receipt BEFORE INSERT ON ingress_effect_receipts WHEN NEW.semantic_identity_hash = X'{hash}' BEGIN SELECT RAISE(FAIL, 'injected relay receipt failure'); END"), ()).await,
        crate::db::DatabaseDriver::Postgres => {
            fixture.execute(&format!("CREATE FUNCTION fail_relay_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.semantic_identity_hash = decode('{hash}', 'hex') THEN RAISE EXCEPTION 'injected relay receipt failure'; END IF; RETURN NEW; END $$"), ()).await;
            fixture.execute("CREATE TRIGGER fail_relay_receipt BEFORE INSERT ON ingress_effect_receipts FOR EACH ROW EXECUTE FUNCTION fail_relay_receipt()", ()).await;
        }
    }
    let first_execution = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    );
    #[cfg(feature = "clustering")]
    let failed = relay_scope(receiver.as_ref(), first_execution).await;
    #[cfg(not(feature = "clustering"))]
    let failed = first_execution.await;
    assert_eq!(failed.receipt_failures.len(), 1);
    assert_eq!(failed.receipt_failures[0].0, receipt);
    assert_eq!(queue_len(&sm, &recipient).await, 1);
    let append_key = SmIngressAppendKey {
        message_key,
        kind: SmIngressReceiptKind::from_storage(receipt.kind.to_storage()),
        semantic_identity_hash: receipt.semantic_identity_hash,
        resource: recipient.clone(),
    };
    assert!(
        crate::sm_persistence::ingress_append::get(&fixture.db, &append_key)
            .await
            .expect("ledger lookup")
            .is_some(),
        "fallback must preserve the exact recorded receipt identity"
    );
    assert_eq!(fixture.count("sm_ingress_appends").await, 1);
    assert_eq!(fixture.count("sm_unacked").await, 1);
    assert!(!terminalize_if_complete(
        &fixture.uow,
        message_key,
        DeliveryExecutionContext::Live.into()
    )
    .await
    .expect("pending receipt"));
    let drop_trigger = match fixture.db.driver() {
        crate::db::DatabaseDriver::Sqlite => "DROP TRIGGER fail_relay_receipt",
        crate::db::DatabaseDriver::Postgres => {
            "DROP TRIGGER fail_relay_receipt ON ingress_effect_receipts"
        }
    };
    fixture.execute(drop_trigger, ()).await;
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("retry frozen route");
    assert_eq!(retry.message_key, Some(message_key));
    assert_eq!(retry.external_receipts[0], vec![receipt]);
    deps.delivery_execution_context = DeliveryExecutionContext::MaintenanceRecovery;
    let recovery_execution = execute_effects(
        &fixture.uow,
        &fixture.db,
        &retry,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    );
    #[cfg(feature = "clustering")]
    let completed = relay_scope(receiver.as_ref(), recovery_execution).await;
    #[cfg(not(feature = "clustering"))]
    let completed = recovery_execution.await;
    assert!(completed.receipt_failures.is_empty());
    assert_eq!(completed.outcomes[0].1, ExternalOutcome::Done);
    assert_eq!(
        queue_len(&sm, &recipient).await,
        1,
        "receipt retry allocates no additional queue entry"
    );
    assert_eq!(fixture.count("sm_ingress_appends").await, 1);
    assert_eq!(fixture.count("sm_unacked").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert!(terminalize_if_complete(
        &fixture.uow,
        message_key,
        DeliveryExecutionContext::Live.into()
    )
    .await
    .expect("terminal receipt"));
    #[cfg(feature = "clustering")]
    if let Some(receiver) = receiver {
        // This is two NEW valid ordered sequences, not a same-sequence ACK replay.
        // Both enter the receiver effect; only the durable SM ledger suppresses the second append.
        assert_eq!(*receiver.delivered_sequences.lock().await, vec![1, 2]);
    }
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    fixture.close().await;
}

async fn queue_len(sm: &InMemorySmSessionRegistry, recipient: &jid::FullJid) -> usize {
    sm.peek_session(&recipient.to_string())
        .await
        .expect("peek session")
        .expect("detached session")
        .unacked_stanzas
        .len()
}

#[tokio::test]
async fn sqlite_remote_planned_relay_detached_receipt_failure_does_not_reappend() {
    relay_fallback_receipt_failure(IngressFixture::sqlite().await, false).await;
}

#[tokio::test]
async fn postgres_remote_planned_relay_detached_receipt_failure_does_not_reappend() {
    if let Some(fixture) = IngressFixture::postgres("relay_detached_receipt_retry").await {
        relay_fallback_receipt_failure(fixture, false).await;
    }
}

#[cfg(feature = "clustering")]
use crate::clustering::{ordered_relay::*, route_bridge::OrderedRelayDeliveryBridge};

/// Replaces only the transport hop; reservations, claim/signature validation,
/// append authorization and the receiving node's durable SM append are real.
#[cfg(feature = "clustering")]
pub(crate) struct CrossNodeReceiver {
    bridge: Arc<OrderedRelayDeliveryBridge>,
    _state: Arc<crate::server::routes::websocket::WebSocketState>,
    receiver: tokio::sync::Mutex<OrderedRelayReceiverState>,
    origin_state: Arc<crate::server::routes::websocket::WebSocketState>,
    delivered_sequences: tokio::sync::Mutex<Vec<u64>>,
}

#[cfg(feature = "clustering")]
impl CrossNodeReceiver {
    async fn new(
        fixture: &IngressFixture,
        sm: Arc<InMemorySmSessionRegistry>,
        recipient: &jid::FullJid,
    ) -> Self {
        use crate::clustering::route_bridge::tests::{
            origin_identity, receiver_identity, services_with_claims,
        };
        use waddle_xmpp::ownership::{ClaimStore, Entity, EntityType, InProcessClaimStore};
        let signer = libp2p::identity::Keypair::generate_ed25519();
        let mut services = services_with_claims(
            origin_identity(),
            receiver_identity(),
            receiver_identity(),
            signer.public().to_peer_id().to_string(),
        )
        .await;
        let store = Arc::new(InProcessClaimStore::new());
        let origin = Entity::new(
            EntityType::UserActor,
            fixture.principal.bare_jid().to_string(),
        );
        let target = Entity::new(EntityType::UserActor, recipient.to_bare().to_string());
        store
            .acquire(&origin, &origin_identity())
            .await
            .expect("origin claim");
        store
            .acquire(&target, &receiver_identity())
            .await
            .expect("target claim");
        let pool = crate::db::DatabasePool::new(
            crate::db::DatabaseConfig::new(fixture.db.driver(), fixture.db.database_url()),
            crate::db::PoolConfig,
        )
        .await
        .expect("receiver database");
        let state = crate::server::routes::websocket::tests::create_test_websocket_state_with_db_pool_and_ingress(
            Arc::new(pool), Arc::new(fixture.authority().await),
        ).await;
        services.claim_store = store.clone();
        services.sm_session_registry = sm;
        services.web_socket_state = Arc::downgrade(&state);
        let bridge = OrderedRelayDeliveryBridge::new(
            tokio_util::sync::CancellationToken::new(),
            &crate::config::ClusteringMessagingConfig::default(),
        );
        bridge.wire(Arc::new(services));
        let origin_bridge = OrderedRelayDeliveryBridge::new(
            tokio_util::sync::CancellationToken::new(),
            &crate::config::ClusteringMessagingConfig::default(),
        );
        origin_bridge.wire_origin_signer(signer.clone());
        let origin_state =
            crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering(
                crate::clustering::ClusteringHandles {
                    ordered_relay_delivery_bridge: Some(Arc::clone(&origin_bridge)),
                    ..Default::default()
                },
                Arc::new(InMemorySmSessionRegistry::new()),
            )
            .await;
        let mut origin_services = services_with_claims(
            origin_identity(),
            receiver_identity(),
            origin_identity(),
            signer.public().to_peer_id().to_string(),
        )
        .await;
        origin_services.claim_store = store;
        origin_services.web_socket_state = Arc::downgrade(&origin_state);
        origin_bridge.wire(Arc::new(origin_services));
        Self {
            bridge,
            _state: state,
            receiver: Default::default(),
            origin_state,
            delivered_sequences: Default::default(),
        }
    }

    pub(crate) async fn deliver(&self, envelope: RemoteStanzaEnvelope) -> OrderedRelayReply {
        assert!(
            matches!(
                &envelope.payload,
                OrderedRelayPayload::Message {
                    ingress_append: Some(_),
                    ..
                }
            ),
            "production origin path must carry its recorded append obligation"
        );
        let envelope: RemoteStanzaEnvelope =
            serde_json::from_slice(&serde_json::to_vec(&envelope).expect("encode wire envelope"))
                .expect("decode wire envelope");
        let mut receiver = self.receiver.lock().await;
        let OrderedRelayReservation::Reserved(reserved) = receiver.reserve(envelope) else {
            panic!("every execution must reserve a new sequence, never replay the ACK cache");
        };
        self.bridge
            .deliver_reserved(reserved.envelope(), &mut None)
            .await
            .expect("receiver append");
        self.delivered_sequences
            .lock()
            .await
            .push(reserved.envelope().sequence.0);
        let reply = receiver.commit_reserved(*reserved);
        assert!(matches!(reply, OrderedRelayReply::Ack(_)));
        reply
    }
}

#[cfg(feature = "clustering")]
async fn relay_scope<T>(
    receiver: Option<&Arc<CrossNodeReceiver>>,
    future: impl std::future::Future<Output = T>,
) -> T {
    match receiver {
        Some(receiver) => {
            CROSS_NODE_RECEIVER
                .scope(Arc::clone(receiver), future)
                .await
        }
        None => future.await,
    }
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn sqlite_cross_node_keyed_append_receipt_failure_recovery_1778() {
    relay_fallback_receipt_failure(IngressFixture::sqlite().await, true).await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn postgres_cross_node_keyed_append_receipt_failure_recovery_1778() {
    if let Some(fixture) = IngressFixture::postgres("cross_node_keyed_append_1778").await {
        relay_fallback_receipt_failure(fixture, true).await;
    }
}

#[cfg(feature = "clustering")]
tokio::task_local! {
    pub(crate) static CROSS_NODE_RECEIVER: Arc<CrossNodeReceiver>;
}
