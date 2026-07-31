use super::*;

impl OrderedRelayDeliveryBridge {
    pub(super) async fn deliver_reserved_full_jid(
        &self,
        services: &OrderedRelayDeliveryServices,
        target: &jid::FullJid,
        stanza: &Stanza,
    ) -> Result<(), OrderedRelayNackReason> {
        if let Some(outcome) = self
            .try_deliver_registered_remote_resource(target, stanza, DeliveryKind::PeerStanza)
            .await
        {
            return match outcome {
                FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::QueuedDetached => {
                    Ok(())
                }
                FullJidDeliveryOutcome::Dropped => Err(OrderedRelayNackReason::Backpressure),
                FullJidDeliveryOutcome::MaybeCommitted => {
                    Err(OrderedRelayNackReason::MaybeCommitted)
                }
                FullJidDeliveryOutcome::Unavailable => {
                    Err(OrderedRelayNackReason::TargetUnavailable)
                }
            };
        }
        if matches!(stanza, Stanza::Iq(_)) {
            return deliver_reserved_full_jid_peer_live_only(services, target, stanza).await;
        }
        match crate::server::routes::interpret::deliver_peer_to_full(
            Some(&services.user_registry),
            Some(&services.sm_session_registry),
            target,
            stanza,
        )
        .await
        {
            FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::QueuedDetached => Ok(()),
            FullJidDeliveryOutcome::Dropped => Err(OrderedRelayNackReason::Backpressure),
            FullJidDeliveryOutcome::MaybeCommitted => Err(OrderedRelayNackReason::MaybeCommitted),
            FullJidDeliveryOutcome::Unavailable => Err(OrderedRelayNackReason::TargetUnavailable),
        }
    }
}

pub(in super::super) async fn deliver_reserved_full_jid_peer_live_only(
    services: &OrderedRelayDeliveryServices,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> Result<(), OrderedRelayNackReason> {
    let user_actor = services
        .user_registry
        .ask(waddle_xmpp::registry::GetUser {
            bare_jid: target.to_bare(),
        })
        .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .await
        .map_err(|error| {
            tracing::warn!(
                target = %target,
                %error,
                "ordered relay: failed to resolve target UserActor for full-JID IQ"
            );
            OrderedRelayNackReason::Unreachable
        })?;
    let Some(user_actor) = user_actor else {
        return Err(OrderedRelayNackReason::TargetUnavailable);
    };

    match user_actor
        .ask(waddle_xmpp::registry::TrySendPeer {
            jid: target.clone(),
            stanza: stanza.clone(),
        })
        .mailbox_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .reply_timeout(ORDERED_DELIVERY_MAILBOX_TIMEOUT)
        .await
    {
        Ok(BroadcastOutcome::Delivered) => Ok(()),
        Ok(BroadcastOutcome::NotConnected | BroadcastOutcome::DroppedClosed) => {
            Err(OrderedRelayNackReason::TargetUnavailable)
        }
        Ok(BroadcastOutcome::DroppedFull) => Err(OrderedRelayNackReason::Backpressure),
        Err(error) => {
            tracing::warn!(
                target = %target,
                %error,
                "ordered relay: live-only full-JID IQ peer delivery failed"
            );
            Err(OrderedRelayNackReason::InFlight)
        }
    }
}

pub(in super::super) async fn deliver_reserved_bare_jid(
    services: &OrderedRelayDeliveryServices,
    target: &jid::BareJid,
    stanza: &Stanza,
) -> Result<(), OrderedRelayNackReason> {
    if matches!(
        stanza,
        Stanza::Presence(presence) if !is_server_handled_presence_request(presence)
    ) {
        return deliver_reserved_bare_presence_direct(services, target, stanza).await;
    }

    let sender_entity = user_entity(target);
    let origin = OrderedRelayRouteOrigin {
        kind: OrderedRelayRouteOriginKind::Entity(sender_entity.clone()),
        sender_entity,
        inbound_sequence: 0,
        handoff: None,
    };
    let replies = route_local_bare_jid_with_timeout(services, target, stanza, Some(origin)).await?;
    if !replies.is_empty() {
        tracing::warn!(
            bare_jid = %target,
            reply_count = replies.len(),
            "ordered relay: receiver-side bare-JID delivery produced local fallback replies"
        );
        return Err(OrderedRelayNackReason::TargetUnavailable);
    }
    Ok(())
}

pub(in super::super) async fn deliver_reserved_bare_presence_direct(
    services: &OrderedRelayDeliveryServices,
    target: &jid::BareJid,
    stanza: &Stanza,
) -> Result<(), OrderedRelayNackReason> {
    if remote_presence_blocked_for_recipient(services, target, stanza).await? {
        tracing::debug!(
            bare_jid = %target,
            "ordered relay: dropping bare-JID presence from blocked sender"
        );
        return Ok(());
    }

    let live_targets =
        waddle_xmpp::registry::available_resources_for_user(&services.user_registry, target).await;
    let live_set: std::collections::HashSet<jid::FullJid> =
        live_targets.iter().map(|(jid, _)| jid.clone()).collect();
    let mut landed = false;
    for resource in live_targets.into_iter().map(|(jid, _)| jid) {
        match deliver_direct_or_registered_remote_resource(services, &resource, stanza).await {
            FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::QueuedDetached => {
                landed = true;
            }
            FullJidDeliveryOutcome::Unavailable | FullJidDeliveryOutcome::Dropped => {}
            FullJidDeliveryOutcome::MaybeCommitted => {
                landed = true;
            }
        }
    }

    match services
        .sm_session_registry
        .available_detached_resources_for_user(target)
        .await
    {
        Ok(detached) => {
            for resource in detached {
                if live_set.contains(&resource) {
                    continue;
                }
                match services
                    .sm_session_registry
                    .record_stanza_for_detached_resource(&resource, stanza, chrono::Utc::now())
                    .await
                {
                    Ok(true) => {
                        landed = true;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        tracing::warn!(
                            resource = %resource,
                            %error,
                            "ordered relay: failed to record bare-JID presence for detached resource"
                        );
                    }
                }
            }
        }
        Err(error) => {
            tracing::warn!(
                bare_jid = %target,
                %error,
                "ordered relay: failed to enumerate detached resources for bare-JID presence"
            );
        }
    }

    if landed {
        Ok(())
    } else {
        Err(OrderedRelayNackReason::TargetUnavailable)
    }
}

pub(in super::super) async fn deliver_direct_or_registered_remote_resource(
    services: &OrderedRelayDeliveryServices,
    target: &jid::FullJid,
    stanza: &Stanza,
) -> FullJidDeliveryOutcome {
    if let Some(state) = services.web_socket_state.upgrade() {
        if let Some(bridge) = state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .as_ref()
        {
            if let Some(outcome) = bridge
                .try_deliver_registered_remote_resource(target, stanza, DeliveryKind::DirectFrame)
                .await
            {
                return outcome;
            }
        }
    }
    crate::server::routes::interpret::deliver_direct_to_full(
        Some(&services.user_registry),
        Some(&services.sm_session_registry),
        target,
        stanza,
    )
    .await
}

pub(in super::super) async fn remote_presence_blocked_for_recipient(
    services: &OrderedRelayDeliveryServices,
    target: &jid::BareJid,
    stanza: &Stanza,
) -> Result<bool, OrderedRelayNackReason> {
    let Stanza::Presence(presence) = stanza else {
        return Ok(false);
    };
    let Some(sender) = presence.from.as_ref() else {
        return Ok(false);
    };
    let entries = services
        .blocking_storage
        .list_blocked_jid_entries(target)
        .await
        .map_err(|error| {
            tracing::warn!(
                bare_jid = %target,
                sender = %sender,
                %error,
                "ordered relay: failed to load recipient blocklist for remote presence"
            );
            OrderedRelayNackReason::InFlight
        })?;
    Ok(waddle_xmpp::protocol::Blocklist::new(entries).contains_jid(sender))
}

pub(in super::super) async fn route_local_bare_jid_with_timeout(
    services: &OrderedRelayDeliveryServices,
    target: &jid::BareJid,
    stanza: &Stanza,
    origin: Option<OrderedRelayRouteOrigin>,
) -> Result<Vec<Stanza>, OrderedRelayNackReason> {
    let Some(state) = services.web_socket_state.upgrade() else {
        tracing::warn!(
            bare_jid = %target,
            "ordered relay: WebSocket state is gone; cannot deliver bare-JID relay payload"
        );
        return Err(OrderedRelayNackReason::Unreachable);
    };
    if let (Stanza::Presence(presence), Some(origin)) = (stanza, origin.clone()) {
        if is_server_handled_presence_request(presence) {
            return match tokio::time::timeout(
                ORDERED_RECEIVER_DELIVERY_TIMEOUT,
                crate::server::routes::websocket::handlers::presence::handle_ordered_relay_presence_request(
                    state.as_ref(),
                    target,
                    presence.clone(),
                    origin,
                ),
            )
            .await
            {
                Ok(Ok(())) => Ok(Vec::new()),
                Ok(Err(())) => Err(OrderedRelayNackReason::ParseFailure),
                Err(_) => {
                    tracing::warn!(
                        bare_jid = %target,
                        timeout_ms = ORDERED_RECEIVER_DELIVERY_TIMEOUT.as_millis(),
                        "ordered relay: local presence request handling timed out"
                    );
                    Err(OrderedRelayNackReason::MaybeCommitted)
                }
            };
        }
    }
    let deps = build_interpret_deps(state.as_ref(), None).with_ordered_relay_origin(origin);
    match tokio::time::timeout(
        ORDERED_RECEIVER_DELIVERY_TIMEOUT,
        crate::server::routes::interpret::route_to_connection(
            &deps,
            jid::Jid::from(target.clone()),
            Box::new(stanza.clone()),
            0,
            None,
        ),
    )
    .await
    {
        Ok(replies) => Ok(replies),
        Err(_) => {
            tracing::warn!(
                bare_jid = %target,
                timeout_ms = ORDERED_RECEIVER_DELIVERY_TIMEOUT.as_millis(),
                "ordered relay: local bare-JID delivery timed out"
            );
            Err(OrderedRelayNackReason::MaybeCommitted)
        }
    }
}

pub(in super::super) fn is_server_handled_presence_request(
    presence: &xmpp_parsers::presence::Presence,
) -> bool {
    matches!(
        presence.type_,
        xmpp_parsers::presence::Type::Probe
            | xmpp_parsers::presence::Type::Subscribe
            | xmpp_parsers::presence::Type::Subscribed
            | xmpp_parsers::presence::Type::Unsubscribe
            | xmpp_parsers::presence::Type::Unsubscribed
    )
}

pub(in super::super) async fn current_claim(
    services: &OrderedRelayDeliveryServices,
    entity: &Entity,
) -> Option<ClaimSnapshot> {
    match services.claim_store.current_claim(entity).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::warn!(
                entity = %entity,
                %error,
                "ordered relay: claim lookup failed"
            );
            None
        }
    }
}

pub(in super::super) fn user_entity(bare: &jid::BareJid) -> Entity {
    Entity::new(EntityType::UserActor, bare.to_string())
}

pub(in super::super) fn room_entity(room: &jid::BareJid) -> Entity {
    Entity::new(EntityType::RoomActor, room.to_string())
}

pub(in super::super) enum RelayPayloadTarget<'a> {
    Full(&'a jid::FullJid, &'a Stanza),
    Bare(jid::BareJid, &'a Stanza),
    Muc(&'a jid::BareJid, OrderedRelayMucProxyKind, &'a Stanza),
}

pub(in super::super) fn relay_payload_target(
    envelope: &RemoteStanzaEnvelope,
) -> Result<RelayPayloadTarget<'_>, OrderedRelayNackReason> {
    let (recipient, stanza) = match &envelope.payload {
        OrderedRelayPayload::Message { recipient, stanza }
        | OrderedRelayPayload::Iq { recipient, stanza }
        | OrderedRelayPayload::Presence { recipient, stanza } => Ok((recipient, &stanza.0)),
        OrderedRelayPayload::MucProxy {
            room_jid,
            kind,
            stanza,
        } => return Ok(RelayPayloadTarget::Muc(room_jid, *kind, &stanza.0)),
    }?;
    match &envelope.channel.recipient {
        OrderedRelayRecipient::FullJid(full) if recipient == &jid::Jid::from(full.clone()) => {
            Ok(RelayPayloadTarget::Full(full, stanza))
        }
        OrderedRelayRecipient::BareJid(bare) if recipient == &jid::Jid::from(bare.clone()) => {
            Ok(RelayPayloadTarget::Bare(bare.clone(), stanza))
        }
        OrderedRelayRecipient::FullJid(_) | OrderedRelayRecipient::BareJid(_) => {
            Err(OrderedRelayNackReason::ParseFailure)
        }
        OrderedRelayRecipient::Room { .. } => Err(OrderedRelayNackReason::ParseFailure),
    }
}

/// Return `stanza` with its `from` replaced by `sender`.
///
/// Used where a locally-executed relay payload must be attributed to
/// the session the caller already authenticated rather than to
/// whatever the serialized stanza claims (#1445).
pub(in super::super) fn rebind_stanza_sender(stanza: &Stanza, sender: &jid::FullJid) -> Stanza {
    let mut rebound = stanza.clone();
    let from = Some(jid::Jid::from(sender.clone()));
    match &mut rebound {
        Stanza::Iq(iq) => match iq.as_mut() {
            xmpp_parsers::iq::Iq::Get { from: f, .. }
            | xmpp_parsers::iq::Iq::Set { from: f, .. }
            | xmpp_parsers::iq::Iq::Result { from: f, .. }
            | xmpp_parsers::iq::Iq::Error { from: f, .. } => *f = from,
        },
        Stanza::Message(message) => message.from = from,
        Stanza::Presence(presence) => presence.from = from,
    }
    rebound
}
