//! A frozen remote route retains its receipt identity when execution falls back locally.
#[cfg(feature = "clustering")]
use super::FullJidDeliveryOutcome;
use super::{
    execute_effects, terminalize_if_complete, Deps, Duration, Effect, ExternalDeliveryEffect,
    ExternalEffect, ExternalOutcome, ImmediateSink, PlannedEffect, Stanza,
};
use crate::ingress::commit::commit_submission;
use crate::ingress::test_support::IngressFixture;
use crate::server::routes::interpret::DeliveryExecutionContext;
use std::sync::Arc;
use waddle_xmpp::ingress::{EffectMessageIdentity, IngressEffectIntent};
use waddle_xmpp::stream_management::{
    DetachedSession, InMemorySmSessionRegistry, SmIngressAppendKey, SmIngressReceiptKind,
    SmSessionRegistry,
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
use crate::clustering::ordered_relay::{
    OrderedRelayPayload, OrderedRelayReceiverState, OrderedRelayReply, OrderedRelayReservation,
    RemoteStanzaEnvelope,
};
#[cfg(feature = "clustering")]
use crate::clustering::route_bridge::OrderedRelayDeliveryBridge;

/// Replaces only the transport hop; reservations, claim/signature validation,
/// append authorization and the receiving node's durable SM append are real.
#[cfg(feature = "clustering")]
pub(crate) struct CrossNodeReceiver {
    bridge: Arc<OrderedRelayDeliveryBridge>,
    _state: Arc<crate::server::routes::websocket::WebSocketState>,
    receiver: tokio::sync::Mutex<OrderedRelayReceiverState>,
    origin_state: Arc<crate::server::routes::websocket::WebSocketState>,
    delivered_sequences: tokio::sync::Mutex<Vec<u64>>,
    /// The canonical obligation of every copy offered to this node, in order.
    obligations: tokio::sync::Mutex<Vec<crate::ingress::identity::IngressAppendObligationRef>>,
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
        let pool = crate::db::DatabasePool::try_new(
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
            obligations: Default::default(),
        }
    }

    pub(crate) async fn deliver(&self, envelope: RemoteStanzaEnvelope) -> OrderedRelayReply {
        let OrderedRelayPayload::Message { ingress_append, .. } = &envelope.payload else {
            panic!("message relay payload");
        };
        // The hosting node completes a copy under the copy's own key, so a
        // copy an ingress transaction owns never travels without it.
        self.obligations.lock().await.push(
            ingress_append
                .clone()
                .expect("an ingress-owned copy carries its canonical obligation on the wire"),
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

/// Stands in for the node that hosts a registered remote resource: it answers
/// every offer of a copy with one outcome and records what was offered and
/// the canonical obligation the offer carried.
#[cfg(feature = "clustering")]
#[derive(Default)]
struct HostingNode {
    offers: Arc<std::sync::Mutex<Vec<(jid::FullJid, Stanza)>>>,
    obligations: crate::server::routes::interpret::ControlledRemoteObligations,
}

#[cfg(feature = "clustering")]
impl HostingNode {
    async fn answering<T>(
        &self,
        outcome: FullJidDeliveryOutcome,
        execution: impl std::future::Future<Output = T>,
    ) -> T {
        use crate::server::routes::interpret::{
            CONTROLLED_REGISTERED_REMOTE_DELIVERY, CONTROLLED_REGISTERED_REMOTE_OBLIGATIONS,
        };
        CONTROLLED_REGISTERED_REMOTE_OBLIGATIONS
            .scope(
                Arc::clone(&self.obligations),
                CONTROLLED_REGISTERED_REMOTE_DELIVERY
                    .scope((outcome, Arc::clone(&self.offers)), execution),
            )
            .await
    }

    /// The target and the message of every offer, in order.
    fn offers(&self) -> Vec<(jid::FullJid, xmpp_parsers::message::Message)> {
        self.offers
            .lock()
            .expect("offers")
            .iter()
            .map(|(target, stanza)| match stanza {
                Stanza::Message(message) => (target.clone(), message.clone()),
                _ => panic!("a room copy is a message"),
            })
            .collect()
    }

    /// The canonical obligation each offer carried, in order.
    fn obligations(
        &self,
    ) -> Vec<
        Option<(
            waddle_xmpp::ingress::MessageKey,
            crate::ingress::EffectReceiptKey,
        )>,
    > {
        self.obligations.lock().expect("obligations").clone()
    }
}

/// The resources recorded as having received their copy of `receipt`.
#[cfg(feature = "clustering")]
async fn recorded_copies(
    fixture: &IngressFixture,
    key: waddle_xmpp::ingress::MessageKey,
    receipt: &crate::ingress::EffectReceiptKey,
) -> Vec<jid::FullJid> {
    let mut tx = fixture.uow.begin().await.expect("read delivery progress");
    let recorded = crate::ingress_uow::DeliveryProgressRepository::load(&mut tx, key, receipt)
        .await
        .expect("delivery progress");
    tx.commit().await.expect("read commit");
    recorded
}

/// A copy of a sealed room message for a resource whose session is detached
/// on another node. The owner sends it over the room's ordered channel with
/// the copy's canonical obligation; the hosting node appends it to the
/// session under that key, and its acknowledgement is the copy's outcome.
///
/// With `lose_first_record` the owner's record of the acknowledged copy
/// fails, so the execution reports the copy's outcome as unknown. The
/// hosting node's append is recorded under the copy's key, and the owner's
/// next execution settles the child from that record without another offer.
#[cfg(feature = "clustering")]
async fn sealed_copy_for_a_session_detached_on_another_node(lose_first_record: bool) {
    use crate::ingress::execute_uow::FAIL_DELIVERY_PROGRESS_TX;
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
    let receipt = decision.external_receipts[0][0].clone();
    let receiver = Arc::new(
        CrossNodeReceiver::new_with_origin(&fixture, sm.clone(), &target, Some(room.clone())).await,
    );
    let mut deps = Deps::new(
        &receiver.origin_state.deps.protocol.connection_registry,
        "example.com",
    );
    deps.web_socket_state = Some(&receiver.origin_state);
    let report = relay_scope(
        Some(&receiver),
        FAIL_DELIVERY_PROGRESS_TX.scope(
            lose_first_record,
            execute_effects(
                &fixture.uow,
                &fixture.db,
                &decision,
                &ImmediateSink,
                &deps,
                Duration::from_secs(5),
            ),
        ),
    )
    .await;

    // The wire carried the copy's canonical obligation.
    assert_eq!(*receiver.delivered_sequences.lock().await, vec![1]);
    {
        let exported = receiver.obligations.lock().await;
        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0].message_key, key);
        assert_eq!(exported[0].receipt, receipt);
        assert_eq!(
            exported[0].sender_bare, room,
            "the obligation is bound to the room whose claim the sender holds"
        );
    }

    // The hosting node appended the copy to the detached session under the
    // copy's key, with the frame receipt and the original receipt time.
    assert_eq!(queue_len(&sm, &target).await, 1);
    let accepted = sm
        .peek_session(&target.to_string())
        .await
        .expect("receiver session read")
        .expect("receiver session");
    assert!(
        accepted.unacked_stanzas[0].ingress_receipts.is_empty(),
        "the keyed append is the copy's acceptance; its frame owes no further receipt"
    );
    assert_eq!(
        accepted.unacked_stanzas[0].original_receipt_at,
        decision
            .route_progress
            .iter()
            .find_map(|progress| progress.received_at)
            .expect("canonical receipt time"),
        "the copy keeps the original XEP-0203 replay timestamp"
    );
    let append_key = SmIngressAppendKey {
        message_key: key,
        kind: SmIngressReceiptKind::from_storage(receipt.kind.to_storage()),
        semantic_identity_hash: receipt.semantic_identity_hash,
        resource: target.clone(),
    };
    assert!(
        crate::sm_persistence::ingress_append::get(&fixture.db, &append_key)
            .await
            .expect("ledger lookup")
            .is_some(),
        "the append is recorded under the child's key"
    );
    assert_eq!(fixture.count("sm_ingress_appends").await, 1);
    assert_eq!(
        fixture.count("ingress_send_attempts").await,
        0,
        "a detached append is recorded in the stream-management ledger, not as a socket attempt"
    );

    if lose_first_record {
        // The acknowledged copy could not be recorded: its outcome is not
        // known to the owner, and nothing claims it was delivered.
        assert_eq!(report.outcomes[0].1, ExternalOutcome::Uncertain);
        assert!(recorded_copies(&fixture, key, &receipt).await.is_empty());
    } else {
        // The hosting node's acknowledgement is the child's outcome.
        assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
        assert_eq!(
            recorded_copies(&fixture, key, &receipt).await,
            vec![target.clone()]
        );
    }
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        if lose_first_record { 0 } else { 1 },
        "the route's receipt follows the record of its last copy"
    );
    assert!(
        !terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("terminalisation check"),
        "the message stays open for the obligations this execution did not carry"
    );

    // A repeat by the owner's live retry and by its recovery.
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("retry sealed acceptance");
    assert_eq!(retry.alias, crate::ingress::AliasOutcomeClass::Existing);
    let repeated = relay_scope(
        Some(&receiver),
        execute_effects(
            &fixture.uow,
            &fixture.db,
            &retry,
            &ImmediateSink,
            &deps,
            Duration::from_secs(5),
        ),
    )
    .await;
    if lose_first_record {
        // The hosting node's append is recorded under the child's key in
        // the database both nodes share. The owner finds it there, records
        // the child and offers nothing again.
        assert_eq!(repeated.outcomes[0].1, ExternalOutcome::Done);
    } else {
        assert!(
            repeated.outcomes.is_empty(),
            "a settled child is not executed again"
        );
    }
    assert_eq!(
        *receiver.delivered_sequences.lock().await,
        vec![1],
        "the copy is offered to the hosting node once"
    );
    assert_eq!(receiver.obligations.lock().await.len(), 1);
    deps.delivery_execution_context = DeliveryExecutionContext::MaintenanceRecovery;
    let sequences_before_recovery = receiver.delivered_sequences.lock().await.clone();
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
        sequences_before_recovery,
        "recovery does not send a settled child"
    );
    assert_eq!(
        queue_len(&sm, &target).await,
        1,
        "the session holds the copy once"
    );
    assert_eq!(fixture.count("sm_ingress_appends").await, 1);
    assert_eq!(fixture.count("ingress_send_attempts").await, 0);
    assert_eq!(
        recorded_copies(&fixture, key, &receipt).await,
        vec![target],
        "the copy is recorded once"
    );
    assert_eq!(fixture.count("ingress_delivery_receipts").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    fixture.close().await;
}

/// Run the exchange on a stack of the size the server gives its workers
/// (`main.rs`). The receiving node's delivery runs inside the owner's
/// execution here, which is deeper than either node goes on its own and
/// deeper than a test thread's default stack.
#[cfg(feature = "clustering")]
fn run_sealed_copy_for_a_session_detached_on_another_node(lose_first_record: bool) {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(sealed_copy_for_a_session_detached_on_another_node(
                    lose_first_record,
                ));
        })
        .expect("exchange thread")
        .join()
        .expect("exchange");
}

#[cfg(feature = "clustering")]
#[test]
fn xep0045_sealed_live_remote_copy_exports_its_obligation_and_the_hosting_ack_settles_it() {
    run_sealed_copy_for_a_session_detached_on_another_node(false);
}

#[cfg(feature = "clustering")]
#[test]
fn xep0045_sealed_live_remote_copy_with_an_unrecorded_ack_is_settled_from_its_keyed_append() {
    run_sealed_copy_for_a_session_detached_on_another_node(true);
}

/// One sealed room message whose only planned copy is for `alice`'s phone, a
/// resource registered from another node.
#[cfg(feature = "clustering")]
struct SealedPeerCopy {
    submission: crate::ingress::IngressSubmission,
    target: jid::FullJid,
    message: xmpp_parsers::message::Message,
}

#[cfg(feature = "clustering")]
fn sealed_peer_copy(fixture: &IngressFixture) -> SealedPeerCopy {
    use crate::ingress::xep0045_room_manifest_tests::room_submission;
    use crate::server::routes::interpret::effects::delivery::PeerDeliveryKind;
    let mut submission = room_submission(fixture);
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
    let mut message = submission
        .plan
        .room_canonical_message
        .as_ref()
        .expect("canonical")
        .as_ref()
        .clone();
    message.to = Some(target.clone().into());
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(
        ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
            route_identity: Some(identity),
            jid: target.clone(),
            stanza: Box::new(Stanza::Message(message.clone())),
            kind: PeerDeliveryKind::PeerStanza,
            call_setup: None,
        }),
    ))];
    SealedPeerCopy {
        submission,
        target,
        message,
    }
}

/// A sealed room copy for a resource registered from another node is offered
/// to that node with its canonical obligation. The hosting node's
/// acknowledgement settles the child, and neither the owner's retry nor its
/// recovery offers a settled child again.
#[cfg(feature = "clustering")]
#[tokio::test]
async fn xep0045_sealed_registered_remote_peer_ack_settles_the_child_once() {
    let fixture = IngressFixture::sqlite().await;
    let SealedPeerCopy {
        submission,
        target,
        message,
    } = sealed_peer_copy(&fixture);
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("sealed peer route");
    let key = decision.message_key.expect("message key");
    let receipt = decision.external_receipts[0][0].clone();
    let connections = waddle_xmpp::registry::ConnectionRegistry::new();
    let mut deps = Deps::new(&connections, "example.com");
    let hosting = HostingNode::default();
    let report = hosting
        .answering(
            FullJidDeliveryOutcome::Delivered,
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
        hosting.offers(),
        vec![(target.clone(), message)],
        "the frozen copy is offered once to its frozen endpoint"
    );
    assert_eq!(
        hosting.obligations(),
        vec![Some((key, receipt.clone()))],
        "the offer carries the child's canonical obligation"
    );
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    assert_eq!(
        recorded_copies(&fixture, key, &receipt).await,
        vec![target.clone()]
    );
    assert_eq!(
        fixture.count("ingress_send_attempts").await,
        0,
        "the send attempt is claimed by the node that writes to the socket"
    );
    assert_eq!(
        fixture.count("ingress_effect_receipts").await,
        1,
        "the route's receipt follows its last copy"
    );
    assert!(
        !terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("terminalisation check"),
        "the message stays open for the obligations this execution did not carry"
    );

    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("retry sealed acceptance");
    assert_eq!(retry.alias, crate::ingress::AliasOutcomeClass::Existing);
    let repeated = hosting
        .answering(
            FullJidDeliveryOutcome::Delivered,
            execute_effects(
                &fixture.uow,
                &fixture.db,
                &retry,
                &ImmediateSink,
                &deps,
                Duration::from_secs(5),
            ),
        )
        .await;
    assert!(
        repeated.outcomes.is_empty(),
        "a settled child is not executed again"
    );
    deps.delivery_execution_context = DeliveryExecutionContext::MaintenanceRecovery;
    let _ = hosting
        .answering(
            FullJidDeliveryOutcome::Delivered,
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
        hosting.offers().len(),
        1,
        "neither a retry nor recovery offers a settled child again"
    );
    assert_eq!(
        recorded_copies(&fixture, key, &receipt).await,
        vec![target],
        "the copy is recorded once"
    );
    fixture.close().await;
}

/// While the hosting node's answer to an offer is unknown the child stays
/// owed and nothing records it. Every further offer is the same frozen copy
/// for the same endpoint under the same canonical obligation, so the hosting
/// node's one send attempt for that key decides whether a socket is written;
/// the first definite answer settles the child once.
#[cfg(feature = "clustering")]
#[tokio::test]
async fn xep0045_sealed_registered_remote_peer_unknown_outcome_stays_owed_under_one_key() {
    let fixture = IngressFixture::sqlite().await;
    let SealedPeerCopy {
        submission,
        target,
        message,
    } = sealed_peer_copy(&fixture);
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("sealed peer route");
    let key = decision.message_key.expect("message key");
    let receipt = decision.external_receipts[0][0].clone();
    let connections = waddle_xmpp::registry::ConnectionRegistry::new();
    let mut deps = Deps::new(&connections, "example.com");
    let hosting = HostingNode::default();
    let report = hosting
        .answering(
            FullJidDeliveryOutcome::MaybeCommitted,
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
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Uncertain);
    assert!(recorded_copies(&fixture, key, &receipt).await.is_empty());
    assert_eq!(fixture.count("ingress_delivery_receipts").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(fixture.count("ingress_send_attempts").await, 0);
    assert!(
        !terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("pending manifest")
    );

    // The owner's live retry: still no answer.
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("retry sealed acceptance");
    assert_eq!(retry.alias, crate::ingress::AliasOutcomeClass::Existing);
    let repeated = hosting
        .answering(
            FullJidDeliveryOutcome::MaybeCommitted,
            execute_effects(
                &fixture.uow,
                &fixture.db,
                &retry,
                &ImmediateSink,
                &deps,
                Duration::from_secs(5),
            ),
        )
        .await;
    assert_eq!(repeated.outcomes[0].1, ExternalOutcome::Uncertain);
    assert!(recorded_copies(&fixture, key, &receipt).await.is_empty());

    // The owner's recovery: the hosting node answers.
    deps.delivery_execution_context = DeliveryExecutionContext::MaintenanceRecovery;
    let recovered = hosting
        .answering(
            FullJidDeliveryOutcome::Delivered,
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
    assert_eq!(recovered.outcomes[0].1, ExternalOutcome::Done);
    assert_eq!(
        hosting.offers(),
        vec![(target.clone(), message); 3],
        "every offer is the same frozen copy for the same endpoint"
    );
    assert_eq!(
        hosting.obligations(),
        vec![Some((key, receipt.clone())); 3],
        "every offer carries the same canonical obligation"
    );
    assert_eq!(
        recorded_copies(&fixture, key, &receipt).await,
        vec![target.clone()]
    );
    let _ = hosting
        .answering(
            FullJidDeliveryOutcome::Delivered,
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
        hosting.offers().len(),
        3,
        "a settled child is not offered again"
    );
    assert_eq!(
        recorded_copies(&fixture, key, &receipt).await,
        vec![target],
        "the copy is recorded once"
    );
    fixture.close().await;
}

/// Two sealed room messages with consecutive archive positions and a copy
/// each for the same remote resource. The second copy is the first's
/// successor in the room's archive order: it is not offered while the first
/// copy's outcome is unknown, neither to the hosting node nor to a local
/// connection of the target, and it is offered once the hosting node's
/// acknowledgement has settled the first.
#[cfg(feature = "clustering")]
#[tokio::test]
async fn xep0045_sealed_live_remote_successor_follows_its_settled_archive_predecessor() {
    use crate::ingress::xep0045_room_manifest_tests::room_submission;
    use crate::ingress_uow::{
        ArchiveDispatchObligation, ArchiveDispatchRepository, DispatchReadiness, DispatchTarget,
    };
    use crate::server::routes::interpret::effects::delivery::PeerDeliveryKind;
    use waddle_xmpp::ingress::{DigestContext, DigestInput};
    let fixture = IngressFixture::sqlite().await;
    let connections = waddle_xmpp::registry::ConnectionRegistry::new();
    let mut deps = Deps::new(&connections, "example.com");
    let target: jid::FullJid = "alice@example.com/phone".parse().expect("remote occupant");
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let hosting = HostingNode::default();
    let mut decisions = Vec::new();
    let mut submissions = Vec::new();
    let mut copies = Vec::new();
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
                stanza: Box::new(Stanza::Message(message.clone())),
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
        // The hosting node does not answer the first copy; the second is
        // executed behind it.
        let report = hosting
            .answering(
                FullJidDeliveryOutcome::MaybeCommitted,
                execute_effects(
                    &fixture.uow,
                    &fixture.db,
                    &decision,
                    &ImmediateSink,
                    &deps,
                    Duration::from_millis(500),
                ),
            )
            .await;
        if ordinal == 1 {
            assert_eq!(report.outcomes[0].1, ExternalOutcome::Uncertain);
        } else {
            assert_eq!(
                report.outcomes[0].1,
                ExternalOutcome::AwaitingPredecessor,
                "the successor waits for the copy before it in the archive"
            );
        }
        submissions.push(submission);
        decisions.push(decision);
        copies.push((key, receipt, message));
    }
    assert_eq!(
        hosting.offers(),
        vec![(target.clone(), copies[0].2.clone())],
        "only the first copy was offered"
    );
    for (key, receipt, _) in &copies {
        assert!(recorded_copies(&fixture, *key, receipt).await.is_empty());
    }
    assert_eq!(fixture.count("ingress_delivery_receipts").await, 0);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(fixture.count("ingress_send_attempts").await, 0);
    assert_eq!(fixture.count("sm_ingress_appends").await, 0);

    // The saved manifest is the audience: a retry that names another
    // resource plans nothing for it, while the unchanged retry still owes
    // the frozen copy.
    let unchanged = commit_submission(&fixture.uow, &submissions[1], 1)
        .await
        .expect("live original retry");
    assert_eq!(unchanged.alias, crate::ingress::AliasOutcomeClass::Existing);
    assert_eq!(unchanged.external.len(), 1);
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

    // A connection of the target on this node does not receive the
    // successor ahead of its predecessor either.
    let (local_tx, mut local_rx) = tokio::sync::mpsc::channel(4);
    connections.register(target.clone(), local_tx);
    let _ = hosting
        .answering(
            FullJidDeliveryOutcome::Delivered,
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
    assert_eq!(hosting.offers().len(), 1);
    connections.unregister(&target);

    // The first copy is offered again, unchanged, and the hosting node's
    // acknowledgement settles it.
    let retry = commit_submission(&fixture.uow, &submissions[0], 1)
        .await
        .expect("retry the first acceptance");
    assert_eq!(retry.alias, crate::ingress::AliasOutcomeClass::Existing);
    let report = hosting
        .answering(
            FullJidDeliveryOutcome::Delivered,
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
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    {
        let offers = hosting.offers();
        assert_eq!(offers.len(), 2);
        assert_eq!(
            offers[0].0, offers[1].0,
            "retry retains the original endpoint"
        );
        assert_eq!(
            offers[0].1, offers[1].1,
            "retry retains the exact frozen payload"
        );
    }

    // Its successor is now sent and settled the same way.
    let report = hosting
        .answering(
            FullJidDeliveryOutcome::Delivered,
            execute_effects(
                &fixture.uow,
                &fixture.db,
                &unchanged,
                &ImmediateSink,
                &deps,
                Duration::from_secs(2),
            ),
        )
        .await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    assert_eq!(
        hosting.offers(),
        vec![
            (target.clone(), copies[0].2.clone()),
            (target.clone(), copies[0].2.clone()),
            (target.clone(), copies[1].2.clone()),
        ],
        "the copies reach the hosting node in archive order"
    );
    assert_eq!(
        hosting.obligations(),
        vec![
            Some((copies[0].0, copies[0].1.clone())),
            Some((copies[0].0, copies[0].1.clone())),
            Some((copies[1].0, copies[1].1.clone())),
        ],
        "each offer carries its own child's canonical obligation"
    );

    // Recovery finds both children settled and offers nothing.
    deps.delivery_execution_context = DeliveryExecutionContext::MaintenanceRecovery;
    for decision in decisions {
        let _ = hosting
            .answering(
                FullJidDeliveryOutcome::Delivered,
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
    assert_eq!(hosting.offers().len(), 3);
    for (key, receipt, _) in &copies {
        assert_eq!(
            recorded_copies(&fixture, *key, receipt).await,
            vec![target.clone()],
            "each copy is recorded once"
        );
    }
    assert_eq!(fixture.count("ingress_send_attempts").await, 0);
    assert_eq!(fixture.count("sm_ingress_appends").await, 0);
    fixture.close().await;
}

/// A copy whose hosting node did not answer is still owed when its target
/// leaves the room and joins it again. The copy was frozen for the occupancy
/// that left: the retry settles the child as superseded and offers nothing
/// to the occupancy that replaced it.
#[cfg(feature = "clustering")]
#[tokio::test]
async fn xep0045_sealed_live_remote_rejoined_occupancy_settles_without_another_wire_attempt() {
    use crate::ingress::xep0045_room_manifest_tests::room_submission;
    use crate::ingress::RoomManifestSnapshot;
    use crate::ingress_uow::CanonicalMessageRepository;
    use crate::server::routes::interpret::effects::delivery::PeerDeliveryKind;
    use waddle_xmpp::muc::room_actor::{
        GetRoomSnapshot, Join, LeaveAttemptId, LeaveByRealJid, LeaveOrigin, LeaveSessionSelector,
    };
    use waddle_xmpp::muc::room_registry_actor::CreateRoom;
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
            *manifest_snapshot = Some(Box::new(RoomManifestSnapshot::from(&snapshot)));
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
                stanza: Box::new(Stanza::Message(message.clone())),
                call_setup: None,
            }
        } else {
            ExternalDeliveryEffect::RouteToPeer {
                route_identity: Some(route_identity),
                jid: target.clone(),
                stanza: Box::new(Stanza::Message(message.clone())),
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
        let mut deps = Deps::new(&state.deps.protocol.connection_registry, "example.com");
        deps.room_registry = Some(&state.deps.protocol.room_registry);
        deps.web_socket_state = Some(&state);
        let hosting = HostingNode::default();
        // The copy is offered to the occupancy it was frozen for, and the
        // hosting node's answer is lost.
        let report = hosting
            .answering(
                FullJidDeliveryOutcome::MaybeCommitted,
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
        assert_eq!(hosting.offers(), vec![(target.clone(), message)]);
        assert_eq!(hosting.obligations(), vec![Some((key, receipt.clone()))]);
        assert!(recorded_copies(&fixture, key, &receipt).await.is_empty());
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
        let report = hosting
            .answering(
                FullJidDeliveryOutcome::Delivered,
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
            hosting.offers().len(),
            1,
            "old child cannot reach replacement occupancy"
        );
        assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
        assert_eq!(
            recorded_copies(&fixture, key, &receipt).await,
            vec![target],
            "the child is settled for the occupancy it was frozen for"
        );
        let mut tx = fixture.uow.begin().await.expect("saved manifest");
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
