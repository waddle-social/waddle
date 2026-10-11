use super::*;
use crate::ingress::test_support::IngressFixture;
use crate::ingress_substrate::{gc_expired_aliases, AliasGcBudget, ALIAS_RETENTION};
use crate::ingress_uow::{
    CanonicalMessageRepository, DeliveryEffectRepository, EffectDeliveryBinding,
    EffectDescendantRepository, EffectIntentRepository, EffectReceiptRepository,
    SmIngressStreamRepository,
};
use waddle_xmpp::ingress::{
    DeliveryKey, EffectMessageIdentity, IngressEffectIntent, MessageKey, SemanticDigest,
    WireHandledCount,
};
use waddle_xmpp::pending_delivery::SmSessionId;

async fn collect_at(fixture: &IngressFixture, now: chrono::DateTime<chrono::Utc>) -> usize {
    gc_expired_aliases(
        &fixture.db,
        now,
        AliasGcBudget {
            deadline: tokio::time::Instant::now() + Duration::from_secs(10),
            lock_timeout: Duration::from_secs(1),
            statement_timeout: Duration::from_secs(2),
            scan_timeout: Duration::from_secs(2),
            progress: Default::default(),
        },
    )
    .await
    .expect("canonical collection")
    .deleted_messages
}

async fn relay_ack_never_settles_canonical_ancestry_or_starts_retention(fixture: IngressFixture) {
    let terminal = chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("terminal time");
    let settled = terminal + chrono::Duration::days(9);
    let keys = [MessageKey::new(), MessageKey::new()];
    let descendant = uuid::Uuid::new_v4();
    let stream = SmSessionId::new("stream-1");
    let intent = IngressEffectIntent::RouteDirect {
        recipient: target_bare(),
        fanout: vec![target_full()],
        route_identity: EffectMessageIdentity::capture_ordinal(0),
        prepared: None,
    };
    let effect = intent.semantic_key();
    let receipt = crate::ingress::receipt_key(&intent).expect("route receipt");
    let mut tx = fixture.uow.begin().await.expect("seed custody");
    let stream_id = SmIngressStreamRepository::mint(&mut tx, &stream)
        .await
        .expect("canonical stream");
    for key in keys {
        CanonicalMessageRepository::record_message(
            &mut tx,
            key,
            &SemanticDigest::from_storage(1, [71; 32]).expect("digest"),
            None,
        )
        .await
        .expect("canonical ancestor");
        EffectIntentRepository::reconcile(&mut tx, key, std::slice::from_ref(&intent), false)
            .await
            .expect("route authority");
        assert_eq!(
            DeliveryEffectRepository::bind_effect(&mut tx, key, &effect)
                .await
                .expect("bind route"),
            EffectDeliveryBinding::Bound(DeliveryKey::effect(key, &effect)),
        );
        EffectDescendantRepository::attach(&mut tx, key, &effect, descendant)
            .await
            .expect("pending descendant custody");
        EffectReceiptRepository::record_receipt(
            &mut tx,
            key,
            receipt.kind,
            &receipt.semantic_identity_hash,
        )
        .await
        .expect("operational route receipt");
        CanonicalMessageRepository::terminalize(&mut tx, key, terminal)
            .await
            .expect("terminal operational proof");
    }
    tx.commit().await.expect("custody commit");
    assert_eq!(
        fixture
            .count(
                "ingress_messages WHERE terminal_at IS NOT NULL AND retention_eligible_at IS NULL"
            )
            .await,
        2
    );
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NULL")
            .await,
        2
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);

    let mut services = services_with_claims(
        origin_identity(),
        receiver_identity(),
        origin_identity(),
        test_peer_id(),
    )
    .await;
    services.occupancy_database = fixture.db.clone();
    let services = Arc::new(services);
    let context = crate::server::routes::interpret::SmIngressAppendContext {
        message_key: keys[0],
        receipt,
        received_at: Some(terminal),
        archive_positions: Vec::new(),
        dispatch_stream: None,
        authority: crate::ingress::append_authority::AppendAuthority::Verified,
    };
    let mut offered = envelope_for_services(&services).await;
    let OrderedRelayPayload::Message { ingress_append, .. } = &mut offered.payload else {
        panic!("message envelope");
    };
    *ingress_append = Some(
        crate::ingress::identity::IngressAppendObligationRef::from_context(
            &context,
            sender_full().to_bare(),
        ),
    );
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    let mut receiver = crate::clustering::ordered_relay::OrderedRelayReceiverState::default();
    let crate::clustering::ordered_relay::OrderedRelayReservation::Reserved(reserved) =
        receiver.reserve(offered.clone())
    else {
        panic!("ordered reservation");
    };
    let ack = receiver.commit_reserved(*reserved);
    assert!(matches!(&ack, OrderedRelayReply::Ack(_)));
    // Consume both the fresh and cached ACK through the origin's actual handler.
    for reply in [
        ack,
        match receiver.reserve(offered.clone()) {
            crate::clustering::ordered_relay::OrderedRelayReservation::Completed(reply) => reply,
            _ => panic!("duplicate ACK"),
        },
    ] {
        let outcome = Arc::clone(&bridge)
            .finish_prepared_delivery_result(
                PreparedRemoteDelivery {
                    ingress_append_context: Some(context.clone()),
                    services: Arc::clone(&services),
                    target_entity: target_entity(),
                    previous_owner: receiver_identity(),
                    channel: offered.channel.clone(),
                    envelope: offered.clone(),
                    target: target_full().into(),
                    stanza: match &offered.payload {
                        OrderedRelayPayload::Message { stanza, .. } => stanza.0.clone(),
                        _ => unreachable!("message envelope"),
                    },
                    is_iq: false,
                },
                Ok(reply),
            )
            .await
            .expect("ACK delivery outcome");
        assert_eq!(outcome.delivery, FullJidDeliveryOutcome::Delivered);
        assert!(!outcome.maybe_committed);
        assert_eq!(
            fixture
                .count("ingress_effect_descendants WHERE settled_at IS NULL")
                .await,
            2,
            "ACK must not settle either canonical ancestor"
        );
        assert_eq!(fixture.count("ingress_messages WHERE terminal_at IS NOT NULL AND retention_eligible_at IS NULL").await, 2, "ACK must not start the canonical retention tail");
        let mut tx = fixture.uow.begin().await.expect("canonical frontier check");
        assert_eq!(
            SmIngressStreamRepository::load_stream_checkpoint(&mut tx, stream_id)
                .await
                .expect("canonical frontier"),
            Some(WireHandledCount::from_storage(0)),
            "relay acknowledgement is not a Foundation handled checkpoint"
        );
        tx.commit().await.expect("frontier check commit");
    }
    assert_eq!(
        collect_at(&fixture, settled).await,
        0,
        "old terminal proofs are still protected by pending custody"
    );
    let mut tx = fixture
        .uow
        .begin()
        .await
        .expect("explicit durable settlement");
    EffectDescendantRepository::settle_all(&mut tx, descendant, settled)
        .await
        .expect("settle every ancestor");
    tx.commit().await.expect("settlement commit");
    assert_eq!(
        fixture
            .count("ingress_effect_descendants WHERE settled_at IS NOT NULL")
            .await,
        2
    );
    assert_eq!(
        fixture
            .count("ingress_messages WHERE retention_eligible_at IS NOT NULL")
            .await,
        2
    );
    assert_eq!(
        collect_at(
            &fixture,
            settled + ALIAS_RETENTION - chrono::Duration::microseconds(1)
        )
        .await,
        0,
        "retain the full eight-day tail after durable settlement"
    );
    assert_eq!(
        collect_at(&fixture, settled + ALIAS_RETENTION).await,
        2,
        "explicit settlement permits reclamation at the boundary"
    );
    assert_eq!(fixture.count("ingress_messages").await, 0);
    assert_eq!(fixture.count("ingress_effect_descendants").await, 0);
    drop(services);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_relay_ack_never_settles_canonical_ancestry_or_starts_retention() {
    relay_ack_never_settles_canonical_ancestry_or_starts_retention(IngressFixture::sqlite().await)
        .await;
}

#[tokio::test]
async fn postgres_relay_ack_never_settles_canonical_ancestry_or_starts_retention() {
    if let Some(fixture) = IngressFixture::postgres("relay_ack_retention_boundary").await {
        relay_ack_never_settles_canonical_ancestry_or_starts_retention(fixture).await;
    }
}
