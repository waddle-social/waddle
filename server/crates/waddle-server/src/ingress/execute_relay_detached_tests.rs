//! A frozen remote route retains its receipt identity when execution falls back locally.
use super::*;
use crate::ingress::{commit::commit_submission, test_support::IngressFixture};
use crate::server::routes::interpret::test_bots_domain;
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
        promotion_custody: waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed(),
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
    let mut deps = Deps::new(
        &state.deps.protocol.connection_registry,
        "example.com",
        test_bots_domain(),
    );
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
        prepared: None,
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
    expects_keyed_obligation: bool,
}

#[cfg(feature = "clustering")]
impl CrossNodeReceiver {
    async fn new(
        fixture: &IngressFixture,
        sm: Arc<InMemorySmSessionRegistry>,
        recipient: &jid::FullJid,
    ) -> Self {
        Self::new_with_origin(fixture, sm, recipient, None).await
    }

    async fn new_with_origin(
        fixture: &IngressFixture,
        sm: Arc<InMemorySmSessionRegistry>,
        recipient: &jid::FullJid,
        room_origin: Option<jid::BareJid>,
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
        let expects_keyed_obligation = room_origin.is_none();
        let origin = match room_origin {
            Some(room) => Entity::new(EntityType::RoomActor, room.to_string()),
            None => Entity::new(
                EntityType::UserActor,
                fixture.principal.bare_jid().to_string(),
            ),
        };
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
            expects_keyed_obligation,
        }
    }

    pub(crate) async fn deliver(&self, envelope: RemoteStanzaEnvelope) -> OrderedRelayReply {
        let OrderedRelayPayload::Message { ingress_append, .. } = &envelope.payload else {
            panic!("message relay payload");
        };
        assert_eq!(
            ingress_append.is_some(),
            self.expects_keyed_obligation,
            "sealed legacy wire must not export manifest acceptance authority"
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

#[cfg(feature = "clustering")]
#[tokio::test]
async fn xep0045_sealed_live_remote_wire_ack_keeps_manifest_pending_without_keyed_custody() {
    use crate::ingress::xep0045_room_manifest_tests::room_submission;
    use crate::server::routes::interpret::{OrderedRelayRouteOrigin, OrderedRelayRouteOriginKind};
    use waddle_xmpp::ownership::{Entity, EntityType};
    let fixture = IngressFixture::sqlite().await;
    let persistence = Arc::new(
        crate::sm_persistence::DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("SM persistence"),
    );
    let sm = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence));
    let mut submission = room_submission(&fixture);
    let IngressEffectIntent::RouteMucGroupchat {
        room,
        occupants,
        route_identity,
        ..
    } = &submission.plan.intents[0]
    else {
        panic!("room route");
    };
    let room = room.clone();
    let identity = route_identity.clone();
    let target = occupants
        .iter()
        .find(|target| *target != &submission.sender)
        .expect("remote occupant")
        .clone();
    sm.store_session(DetachedSession {
        promotion_custody: waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed(),
        stream_id: target.to_string(),
        user_id: target.to_bare().to_string(),
        jid: target.clone(),
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
    .expect("receiver detached session");
    let mut copy = submission
        .plan
        .room_canonical_message
        .as_ref()
        .expect("canonical")
        .as_ref()
        .clone();
    copy.to = Some(target.clone().into());
    let sender_entity = Entity::new(EntityType::RoomActor, room.to_string());
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(
        ExternalEffect::Delivery(ExternalDeliveryEffect::RelayFullJid {
            route_identity: Some(identity),
            origin: Some(OrderedRelayRouteOrigin {
                kind: OrderedRelayRouteOriginKind::Entity(sender_entity.clone()),
                sender_entity,
                inbound_sequence: 1,
                handoff: None,
            }),
            target: target.clone(),
            stanza: Box::new(Stanza::Message(copy)),
            call_setup: None,
        }),
    ))];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("seal room manifest");
    let key = decision.message_key.expect("message key");
    let receiver = Arc::new(
        CrossNodeReceiver::new_with_origin(&fixture, sm.clone(), &target, Some(room)).await,
    );
    let mut deps = Deps::new(
        &receiver.origin_state.deps.protocol.connection_registry,
        "example.com",
        test_bots_domain(),
    );
    deps.web_socket_state = Some(&receiver.origin_state);
    let report = relay_scope(
        Some(&receiver),
        execute_effects(
            &fixture.uow,
            &fixture.db,
            &decision,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        ),
    )
    .await;
    assert_eq!(
        report.outcomes[0].1,
        ExternalOutcome::Uncertain,
        "legacy ACK is not a manifest child receipt"
    );
    assert_eq!(
        queue_len(&sm, &target).await,
        1,
        "existing remote wire remains delivered"
    );
    assert_eq!(*receiver.delivered_sequences.lock().await, vec![1]);
    let accepted = sm
        .peek_session(&target.to_string())
        .await
        .expect("receiver session read")
        .expect("receiver session");
    assert!(
        accepted.unacked_stanzas[0].ingress_receipts.is_empty(),
        "legacy replay carries no manifest frame receipt"
    );
    assert_eq!(
        accepted.unacked_stanzas[0].original_receipt_at,
        decision
            .route_progress
            .iter()
            .find_map(|progress| progress.received_at)
            .expect("canonical receipt time"),
        "legacy delivery preserves the original XEP-0203 replay timestamp"
    );
    assert_eq!(
        fixture.count("sm_ingress_appends").await,
        0,
        "receiver has no canonical child key"
    );
    assert_eq!(fixture.count("ingress_send_attempts").await, 0);
    assert_eq!(fixture.count("ingress_delivery_receipts").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert!(
        !terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("pending manifest")
    );
    deps.delivery_execution_context = DeliveryExecutionContext::MaintenanceRecovery;
    let _ = relay_scope(
        Some(&receiver),
        execute_effects(
            &fixture.uow,
            &fixture.db,
            &decision,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        ),
    )
    .await;
    assert_eq!(
        *receiver.delivered_sequences.lock().await,
        vec![1],
        "maintenance cannot resend legacy wire"
    );
    assert_eq!(queue_len(&sm, &target).await, 1);
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn xep0045_sealed_registered_remote_peer_wire_ack_keeps_manifest_pending() {
    use crate::ingress::xep0045_room_manifest_tests::room_submission;
    use crate::server::routes::interpret::{
        effects::delivery::PeerDeliveryKind, CONTROLLED_REGISTERED_REMOTE_DELIVERY,
    };
    let fixture = IngressFixture::sqlite().await;
    let mut submission = room_submission(&fixture);
    let IngressEffectIntent::RouteMucGroupchat {
        occupants,
        route_identity,
        ..
    } = &submission.plan.intents[0]
    else {
        panic!("room route");
    };
    let target = occupants
        .iter()
        .find(|target| *target != &submission.sender)
        .expect("captured peer")
        .clone();
    let identity = route_identity.clone();
    let mut copy = submission
        .plan
        .room_canonical_message
        .as_ref()
        .expect("canonical")
        .as_ref()
        .clone();
    copy.to = Some(target.clone().into());
    let stanza = Stanza::Message(copy);
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(
        ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
            route_identity: Some(identity),
            jid: target.clone(),
            stanza: Box::new(stanza.clone()),
            kind: PeerDeliveryKind::PeerStanza,
            call_setup: None,
        }),
    ))];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("sealed peer route");
    let connections = waddle_xmpp::registry::ConnectionRegistry::new();
    let mut deps = Deps::new(&connections, "example.com", test_bots_domain());
    let wire = Arc::new(std::sync::Mutex::new(Vec::new()));
    let report = CONTROLLED_REGISTERED_REMOTE_DELIVERY
        .scope(
            (FullJidDeliveryOutcome::Delivered, wire.clone()),
            execute_effects(
                &fixture.uow,
                &fixture.db,
                &decision,
                &ImmediateSink,
                &deps,
                Duration::from_secs(5),
            ),
        )
        .await;
    {
        let delivered = wire.lock().expect("wire deliveries");
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].0, target);
        let (Stanza::Message(offered), Stanza::Message(expected)) = (&delivered[0].1, &stanza)
        else {
            panic!("typed room wire message");
        };
        assert_eq!(offered, expected);
    }
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Uncertain);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(fixture.count("ingress_delivery_receipts").await, 0);
    deps.delivery_execution_context = DeliveryExecutionContext::MaintenanceRecovery;
    let _ = CONTROLLED_REGISTERED_REMOTE_DELIVERY
        .scope(
            (FullJidDeliveryOutcome::Delivered, wire.clone()),
            execute_effects(
                &fixture.uow,
                &fixture.db,
                &decision,
                &ImmediateSink,
                &deps,
                Duration::from_secs(5),
            ),
        )
        .await;
    assert_eq!(wire.lock().expect("wire deliveries").len(), 1);
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn xep0045_sealed_live_remote_successor_passes_pending_archive_child_without_retirement() {
    use crate::ingress::xep0045_room_manifest_tests::room_submission;
    use crate::ingress_uow::{
        ArchiveDispatchObligation, ArchiveDispatchRepository, DispatchReadiness, DispatchTarget,
    };
    use crate::server::routes::interpret::{
        effects::delivery::PeerDeliveryKind, CONTROLLED_REGISTERED_REMOTE_DELIVERY,
    };
    use waddle_xmpp::ingress::{DigestContext, DigestInput};
    let fixture = IngressFixture::sqlite().await;
    let connections = waddle_xmpp::registry::ConnectionRegistry::new();
    let mut deps = Deps::new(&connections, "example.com", test_bots_domain());
    let target: jid::FullJid = "alice@example.com/phone".parse().expect("remote occupant");
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let wire = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut decisions = Vec::new();
    let mut submissions = Vec::new();
    for ordinal in [1, 2] {
        let mut submission = room_submission(&fixture);
        submission
            .plan
            .sanitized_message
            .payloads
            .retain(|payload| !waddle_xmpp_core::xep0359::is_origin_id_element(payload));
        waddle_xmpp_core::xep0359::add_origin_id(
            &mut submission.plan.sanitized_message,
            if ordinal == 1 {
                "remote-first"
            } else {
                "remote-second"
            },
        );
        submission.digest_input = DigestInput::from_parsed(
            &submission.plan.sanitized_message,
            &DigestContext {
                target: submission.target.clone(),
                server_authorities: vec![room.clone()],
                stanza_lang: None,
            },
        )
        .expect("distinct canonical row");
        let mut message = submission
            .plan
            .room_canonical_message
            .as_ref()
            .expect("canonical")
            .as_ref()
            .clone();
        message.to = Some(target.clone().into());
        let identity = match &submission.plan.intents[0] {
            IngressEffectIntent::RouteMucGroupchat { route_identity, .. } => route_identity.clone(),
            _ => panic!("room route"),
        };
        submission.plan.plan = vec![PlannedEffect::new(Effect::External(
            ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
                route_identity: Some(identity),
                jid: target.clone(),
                stanza: Box::new(Stanza::Message(message)),
                kind: PeerDeliveryKind::PeerStanza,
                call_setup: None,
            }),
        ))];
        let decision = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("sealed acceptance");
        let key = decision.message_key.expect("key");
        let receipt = decision.external_receipts[0][0].clone();
        let mut tx = fixture.uow.begin().await.expect("archive position");
        ArchiveDispatchRepository::record(
            &mut tx,
            key,
            &room,
            waddle_xmpp::mam::ArchiveOrdinal::from_storage(ordinal).expect("ordinal"),
            &[ArchiveDispatchObligation {
                receipt: receipt.clone(),
                target: DispatchTarget::Resource(target.clone()),
            }],
        )
        .await
        .expect("record ordering authority");
        if ordinal == 2 {
            assert!(
                matches!(
                    ArchiveDispatchRepository::readiness(
                        &mut tx,
                        key,
                        &receipt,
                        Some(&target),
                        None
                    )
                    .await
                    .expect("readiness"),
                    DispatchReadiness::Blocked(_)
                ),
                "the first remote child is still a durable predecessor"
            );
        }
        tx.commit().await.expect("position commit");
        let report = CONTROLLED_REGISTERED_REMOTE_DELIVERY
            .scope(
                (FullJidDeliveryOutcome::Delivered, wire.clone()),
                execute_effects(
                    &fixture.uow,
                    &fixture.db,
                    &decision,
                    &ImmediateSink,
                    &deps,
                    Duration::from_secs(2),
                ),
            )
            .await;
        assert_eq!(report.outcomes[0].1, ExternalOutcome::Uncertain);
        submissions.push(submission);
        decisions.push(decision);
    }
    assert_eq!(
        wire.lock().expect("live wire").len(),
        2,
        "pending P2 predecessor cannot silence the second live remote copy"
    );
    assert_eq!(fixture.count("ingress_delivery_receipts").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(fixture.count("ingress_send_attempts").await, 0);
    assert_eq!(fixture.count("sm_ingress_appends").await, 0);
    let retry = commit_submission(&fixture.uow, &submissions[1], 1)
        .await
        .expect("live original retry");
    assert_eq!(retry.alias, crate::ingress::AliasOutcomeClass::Existing);
    let _ = CONTROLLED_REGISTERED_REMOTE_DELIVERY
        .scope(
            (FullJidDeliveryOutcome::Delivered, wire.clone()),
            execute_effects(
                &fixture.uow,
                &fixture.db,
                &retry,
                &ImmediateSink,
                &deps,
                Duration::from_secs(2),
            ),
        )
        .await;
    {
        let attempts = wire.lock().expect("original retry wire");
        assert_eq!(attempts.len(), 3);
        assert_eq!(
            attempts[1].0, attempts[2].0,
            "retry retains the original endpoint"
        );
        let (Stanza::Message(original), Stanza::Message(replayed)) =
            (&attempts[1].1, &attempts[2].1)
        else {
            panic!("typed room copies");
        };
        assert_eq!(original, replayed, "retry retains the exact frozen payload");
    }
    let mut widened = submissions[1].clone();
    let sibling = target
        .to_bare()
        .with_resource_str("new-sibling")
        .expect("new target");
    let IngressEffectIntent::RouteMucGroupchat { occupants, .. } = &mut widened.plan.intents[0]
    else {
        panic!("room route");
    };
    occupants.push(sibling.clone());
    let Effect::External(ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
        jid,
        stanza,
        ..
    })) = &mut widened.plan.plan[0].effect
    else {
        panic!("peer route");
    };
    *jid = sibling.clone();
    let Stanza::Message(message) = stanza.as_mut() else {
        panic!("room copy");
    };
    message.to = Some(sibling.into());
    let rejected_target = commit_submission(&fixture.uow, &widened, 1)
        .await
        .expect("current audience cannot expand saved manifest");
    assert!(
        rejected_target.external.is_empty(),
        "live retry cannot add a sibling target"
    );
    let (local_tx, mut local_rx) = tokio::sync::mpsc::channel(4);
    connections.register(target.clone(), local_tx);
    let _ = CONTROLLED_REGISTERED_REMOTE_DELIVERY
        .scope(
            (FullJidDeliveryOutcome::Delivered, wire.clone()),
            execute_effects(
                &fixture.uow,
                &fixture.db,
                &decisions[1],
                &ImmediateSink,
                &deps,
                Duration::from_millis(100),
            ),
        )
        .await;
    assert!(
        local_rx.try_recv().is_err(),
        "local fallback still waits for its archive predecessor"
    );
    assert_eq!(wire.lock().expect("local fallback").len(), 3);
    connections.unregister(&target);
    deps.delivery_execution_context = DeliveryExecutionContext::MaintenanceRecovery;
    for decision in decisions {
        let _ = CONTROLLED_REGISTERED_REMOTE_DELIVERY
            .scope(
                (FullJidDeliveryOutcome::Delivered, wire.clone()),
                execute_effects(
                    &fixture.uow,
                    &fixture.db,
                    &decision,
                    &ImmediateSink,
                    &deps,
                    Duration::from_secs(2),
                ),
            )
            .await;
    }
    assert_eq!(wire.lock().expect("maintenance wire").len(), 3);
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn xep0045_sealed_live_remote_rejoined_occupancy_settles_without_another_wire_attempt() {
    use crate::ingress::{xep0045_room_manifest_tests::room_submission, RoomManifestSnapshot};
    use crate::ingress_uow::{CanonicalMessageRepository, DeliveryProgressRepository};
    use crate::server::routes::interpret::{
        effects::delivery::PeerDeliveryKind, CONTROLLED_REGISTERED_REMOTE_DELIVERY,
    };
    use waddle_xmpp::muc::{
        room_actor::{
            GetRoomSnapshot, Join, LeaveAttemptId, LeaveByRealJid, LeaveOrigin,
            LeaveSessionSelector,
        },
        room_registry_actor::CreateRoom,
    };
    for (reflection, detached) in [(false, false), (true, false), (false, true), (true, true)] {
        let fixture = IngressFixture::sqlite().await;
        let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
        let mut submission = room_submission(&fixture);
        let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
        let peer: jid::FullJid = "alice@example.com/phone".parse().expect("peer");
        let target = if reflection {
            submission.sender.clone()
        } else {
            peer.clone()
        };
        let actor = state
            .deps
            .protocol
            .room_registry
            .ask(CreateRoom {
                room_jid: room.clone(),
                waddle_id: "remote-rejoin".into(),
                channel_id: "remote-rejoin".into(),
                config: Default::default(),
            })
            .await
            .expect("room actor");
        for (jid, nick) in [(submission.sender.clone(), "sender"), (peer, "alice")] {
            actor
                .ask(Join {
                    session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                    nick: nick.into(),
                    real_jid: jid,
                    role: waddle_xmpp::Role::Participant,
                    affiliation: waddle_xmpp::Affiliation::Member,
                })
                .await
                .expect("original occupancy");
        }
        let snapshot = actor
            .ask(GetRoomSnapshot {
                sender_jid: submission.sender.clone(),
            })
            .await
            .expect("original roster");
        if let crate::ingress::RoomExecutionPath::Local {
            manifest_snapshot,
            snapshot_generation,
            ..
        } = &mut submission.plan.room_execution
        {
            *manifest_snapshot = Some(RoomManifestSnapshot::from(&snapshot));
            *snapshot_generation = snapshot.admission_revision;
        }
        let mut message = submission
            .plan
            .room_canonical_message
            .as_ref()
            .expect("canonical")
            .as_ref()
            .clone();
        message.to = Some(target.clone().into());
        let route_identity = match &submission.plan.intents[0] {
            IngressEffectIntent::RouteMucGroupchat { route_identity, .. } => route_identity.clone(),
            _ => panic!("room route"),
        };
        let delivery = if detached {
            ExternalDeliveryEffect::QueueDetached {
                route_identity: Some(route_identity),
                bare: target.to_bare(),
                resources: vec![target.clone()],
                stanza: Box::new(Stanza::Message(message)),
                call_setup: None,
            }
        } else {
            ExternalDeliveryEffect::RouteToPeer {
                route_identity: Some(route_identity),
                jid: target.clone(),
                stanza: Box::new(Stanza::Message(message)),
                kind: PeerDeliveryKind::PeerStanza,
                call_setup: None,
            }
        };
        submission.plan.plan = vec![PlannedEffect::new(Effect::External(
            ExternalEffect::Delivery(delivery),
        ))];
        let decision = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("original sealed acceptance");
        let key = decision.message_key.expect("key");
        let receipt = decision.external_receipts[0][0].clone();
        let mut tx = fixture.uow.begin().await.expect("original authority");
        let original = CanonicalMessageRepository::load_envelope(&mut tx, key)
            .await
            .expect("load")
            .expect("envelope");
        tx.commit().await.expect("read commit");
        let mut deps = Deps::new(
            &state.deps.protocol.connection_registry,
            "example.com",
            test_bots_domain(),
        );
        deps.room_registry = Some(&state.deps.protocol.room_registry);
        deps.web_socket_state = Some(&state);
        let wire = Arc::new(std::sync::Mutex::new(Vec::new()));
        let _ = CONTROLLED_REGISTERED_REMOTE_DELIVERY
            .scope(
                (FullJidDeliveryOutcome::Delivered, wire.clone()),
                execute_effects(
                    &fixture.uow,
                    &fixture.db,
                    &decision,
                    &ImmediateSink,
                    &deps,
                    Duration::from_secs(2),
                ),
            )
            .await;
        assert_eq!(wire.lock().expect("original wire").len(), 1);
        actor
            .ask(LeaveByRealJid {
                sender_jid: target.clone(),
                cause: waddle_xmpp::muc::durable::OccupancyLeaveCause::Explicit,
                session: LeaveSessionSelector::Any,
                attempt: LeaveAttemptId::generate(),
                origin: LeaveOrigin::Fresh,
            })
            .await
            .expect("leave original occupancy");
        actor
            .ask(Join {
                session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                nick: "replacement".into(),
                real_jid: target.clone(),
                role: waddle_xmpp::Role::Participant,
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("same resource rejoins in the same room incarnation");
        let retry = commit_submission(&fixture.uow, &submission, 1)
            .await
            .expect("live frozen retry");
        let report = CONTROLLED_REGISTERED_REMOTE_DELIVERY
            .scope(
                (FullJidDeliveryOutcome::Delivered, wire.clone()),
                execute_effects(
                    &fixture.uow,
                    &fixture.db,
                    &retry,
                    &ImmediateSink,
                    &deps,
                    Duration::from_secs(2),
                ),
            )
            .await;
        assert_eq!(
            wire.lock().expect("replacement wire").len(),
            1,
            "old child cannot reach replacement occupancy"
        );
        assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
        let mut tx = fixture.uow.begin().await.expect("logical disposition");
        assert_eq!(
            DeliveryProgressRepository::load(&mut tx, key, &receipt)
                .await
                .expect("progress"),
            vec![target]
        );
        let saved = CanonicalMessageRepository::load_envelope(&mut tx, key)
            .await
            .expect("load")
            .expect("saved");
        assert_eq!(original.room_manifest(), saved.room_manifest());
        tx.commit().await.expect("read commit");
        assert_eq!(
            fixture.count("ingress_send_attempts").await,
            0,
            "retirement is not a send completion"
        );
        assert_eq!(
            fixture.count("sm_ingress_appends").await,
            0,
            "retirement creates no physical replay frame"
        );
        fixture.close().await;
    }
}
