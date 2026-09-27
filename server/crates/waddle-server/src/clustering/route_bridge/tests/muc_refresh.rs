//! Exercise the real owner-refresh fallback from controlled ingress relay tests.
use super::*;
use crate::server::routes::interpret::SmIngressAppendContext;

// Build the database fixture before entering the recursive relay callback;
// delivery itself must remain Send, whereas SQLx fixture setup need not be.
pub(crate) async fn owner_refresh_services(
    sm_session_registry: Arc<InMemorySmSessionRegistry>,
) -> Arc<OrderedRelayDeliveryServices> {
    let mut services = services_with_claims(
        origin_identity(),
        origin_identity(),
        origin_identity(),
        test_peer_id(),
    )
    .await;
    services.sm_session_registry = sm_session_registry;
    Arc::new(services)
}

pub(crate) async fn deliver_after_owner_refresh(
    target: &jid::FullJid,
    stanza: &Stanza,
    context: Option<SmIngressAppendContext>,
    services: Arc<OrderedRelayDeliveryServices>,
) -> FullJidDeliveryOutcome {
    let target_entity = user_entity(&target.to_bare());
    services
        .claim_store
        .acquire(&target_entity, &origin_identity())
        .await
        .expect("target now owned locally");
    let mut envelope = envelope();
    envelope.channel.recipient = OrderedRelayRecipient::FullJid(target.clone());
    envelope.target_claim.entity = target_entity.clone();
    envelope.payload = payload_for_recipient(target.clone().into(), stanza)
        .expect("ordinary occupant copy relay payload");
    let prepared = PreparedRemoteDelivery {
        ingress_append_context: context,
        services,
        target_entity,
        previous_owner: receiver_identity(),
        channel: envelope.channel.clone(),
        envelope,
        target: target.clone().into(),
        stanza: stanza.clone(),
        is_iq: false,
    };
    let bridge = OrderedRelayDeliveryBridge::new(
        CancellationToken::new(),
        &ClusteringMessagingConfig::default(),
    );
    let outcome = bridge
        .retry_after_target_owner_refresh(&prepared)
        .await
        .expect("refreshed local owner runs local fallback");
    caller_delivery_outcome(outcome)
}
