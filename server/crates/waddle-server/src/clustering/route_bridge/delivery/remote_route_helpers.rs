use super::*;

pub(in super::super) fn route_origin_claim(
    kind: &OrderedRelayRouteOriginKind,
) -> (Entity, OrderedRelayOrigin) {
    match kind {
        OrderedRelayRouteOriginKind::SmSession(stream_id) => (
            Entity::new(EntityType::SmSession, stream_id.to_string()),
            OrderedRelayOrigin::SmSession(stream_id.clone()),
        ),
        OrderedRelayRouteOriginKind::Entity(entity) => {
            (entity.clone(), OrderedRelayOrigin::Entity(entity.clone()))
        }
        OrderedRelayRouteOriginKind::RemoteResource(remote) => {
            let entity = user_entity(&remote.jid.to_bare());
            (entity.clone(), OrderedRelayOrigin::Entity(entity))
        }
    }
}

pub(in super::super) fn remote_resource_origin(
    origin: &OrderedRelayRouteOrigin,
) -> Option<RemoteResourceOriginSnapshot> {
    match &origin.kind {
        OrderedRelayRouteOriginKind::RemoteResource(remote) => Some(remote.clone()),
        OrderedRelayRouteOriginKind::SmSession(_) | OrderedRelayRouteOriginKind::Entity(_) => None,
    }
}

pub(in super::super) async fn current_fresh_local_relay_claim(
    services: &OrderedRelayDeliveryServices,
    entity: &Entity,
    me: &NodeIdentity,
    role: &'static str,
) -> Option<OrderedRelayClaim> {
    let snapshot = current_claim(services, entity).await?;
    if !snapshot.owner_lease_fresh || snapshot.owner != *me {
        tracing::debug!(
            entity = %entity,
            role,
            "ordered relay: entity is not currently owned locally; keeping local fallback path"
        );
        return None;
    }
    Some(OrderedRelayClaim {
        entity: entity.clone(),
        epoch: snapshot.claim_epoch,
    })
}

pub(in super::super) fn payload_for_recipient(
    recipient: jid::Jid,
    stanza: &Stanza,
) -> Option<OrderedRelayPayload> {
    match stanza {
        Stanza::Message(message)
            if message.type_ == xmpp_parsers::message::MessageType::Groupchat =>
        {
            None
        }
        Stanza::Message(_) => Some(OrderedRelayPayload::Message {
            recipient,
            stanza: RemoteStanza(stanza.clone()),
        }),
        Stanza::Iq(_) => Some(OrderedRelayPayload::Iq {
            recipient,
            stanza: RemoteStanza(stanza.clone()),
        }),
        Stanza::Presence(_) => Some(OrderedRelayPayload::Presence {
            recipient,
            stanza: RemoteStanza(stanza.clone()),
        }),
    }
}
