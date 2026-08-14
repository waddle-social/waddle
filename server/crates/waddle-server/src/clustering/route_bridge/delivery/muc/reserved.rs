use super::*;

pub(in super::super::super) async fn deliver_reserved_muc_proxy(
    services: &OrderedRelayDeliveryServices,
    room_jid: &jid::BareJid,
    kind: OrderedRelayMucProxyKind,
    stanza: &Stanza,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    let Some(state) = services.web_socket_state.upgrade() else {
        tracing::warn!(
            room = %room_jid,
            "ordered relay: WebSocket state is gone; cannot deliver MUC relay payload"
        );
        return Err(OrderedRelayNackReason::Unreachable);
    };
    match (kind, stanza) {
        (OrderedRelayMucProxyKind::JoinPresence, Stanza::Presence(presence)) => {
            deliver_reserved_muc_join(state.as_ref(), room_jid, presence).await
        }
        (OrderedRelayMucProxyKind::GroupchatMessage, Stanza::Message(message)) => {
            deliver_reserved_muc_groupchat(state.as_ref(), room_jid, message).await
        }
        (OrderedRelayMucProxyKind::OccupantPresence, Stanza::Presence(presence)) => {
            deliver_reserved_muc_occupant_presence(state.as_ref(), room_jid, presence).await
        }
        (OrderedRelayMucProxyKind::MujiJingleIq, Stanza::Iq(iq)) => {
            deliver_reserved_muji_iq(state.as_ref(), room_jid, iq).await
        }
        _ => Err(OrderedRelayNackReason::ParseFailure),
    }
}

/// #1445: execute a relayed Muji `session-initiate` on this node — the
/// room-claim owner — and carry the reply frames (IQ ack +
/// server-initiated `session-accept` with the LiveKit token) back to
/// the origin node as client replies. Envelope validation has already
/// bound the payload's `<muji room>` to `room_jid`, so the room
/// binding needs no re-check here; the executor re-runs the membership
/// gate against the local room actor and is terminal (a denial comes
/// back as a delivered IQ-error frame, never a NACK, so no relay
/// loop can form).
pub(in super::super::super) async fn deliver_reserved_muji_iq(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    iq: &xmpp_parsers::iq::Iq,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    let frames = tokio::time::timeout(
        ORDERED_RECEIVER_DELIVERY_TIMEOUT,
        crate::server::routes::websocket::handlers::iq::jingle_muji_relay::handle_relayed_muji_initiate(
            state, iq,
        ),
    )
    .await
    .map_err(|_| {
        tracing::warn!(
            room = %room_jid,
            timeout_ms = ORDERED_RECEIVER_DELIVERY_TIMEOUT.as_millis(),
            "ordered relay: Muji Jingle execution timed out"
        );
        OrderedRelayNackReason::MaybeCommitted
    })?
    .ok_or(OrderedRelayNackReason::ParseFailure)?;
    remote_replies_from_frames(frames)
}

pub(in super::super::super) async fn deliver_reserved_muc_occupant_presence(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    presence: &xmpp_parsers::presence::Presence,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    if presence.type_ == xmpp_parsers::presence::Type::Unavailable {
        return deliver_reserved_muc_leave(state, room_jid, presence).await;
    }
    deliver_reserved_muc_update(state, room_jid, presence).await
}

pub(in super::super::super) async fn deliver_reserved_muc_join(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    presence: &xmpp_parsers::presence::Presence,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    let Some(sender_jid) = presence
        .from
        .as_ref()
        .and_then(|jid| jid.clone().try_into_full().ok())
    else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    let Some(to) = presence.to.as_ref() else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    let Some(nick) = to.resource().map(|resource| resource.as_str()) else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    if to.to_bare() != *room_jid {
        return Err(OrderedRelayNackReason::ParseFailure);
    }
    let presence_show = presence
        .show
        .clone()
        .map(crate::notification_activity::NotificationPresenceShow::from_xep0045);
    let synthetic_session = synthetic_session_for_full_jid(&sender_jid);
    let frames = tokio::time::timeout(
        ORDERED_RECEIVER_DELIVERY_TIMEOUT,
        crate::server::routes::websocket::handlers::presence::handle_muc_join(
            state,
            state.deps.auth_state.xmpp_domain.as_str(),
            room_jid,
            &sender_jid,
            nick,
            presence_show,
            &Some(synthetic_session),
        ),
    )
    .await
    .map_err(|_| {
        tracing::warn!(
            room = %room_jid,
            timeout_ms = ORDERED_RECEIVER_DELIVERY_TIMEOUT.as_millis(),
            "ordered relay: reserved MUC join handling timed out"
        );
        OrderedRelayNackReason::MaybeCommitted
    })?;
    remote_replies_from_frames(frames)
}

pub(in super::super::super) async fn deliver_reserved_muc_update(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    presence: &xmpp_parsers::presence::Presence,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    let Some(sender_jid) = presence
        .from
        .as_ref()
        .and_then(|jid| jid.clone().try_into_full().ok())
    else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    let Some(to) = presence.to.as_ref() else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    let Some(nick) = to.resource().map(|resource| resource.as_str()) else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    if to.to_bare() != *room_jid {
        return Err(OrderedRelayNackReason::ParseFailure);
    }

    match tokio::time::timeout(
        ORDERED_RECEIVER_DELIVERY_TIMEOUT,
        crate::server::routes::websocket::handlers::presence::try_handle_muc_presence_update(
            state,
            room_jid,
            &sender_jid,
            nick,
            presence,
        ),
    )
    .await
    .map_err(|_| {
        tracing::warn!(
            room = %room_jid,
            timeout_ms = ORDERED_RECEIVER_DELIVERY_TIMEOUT.as_millis(),
            "ordered relay: reserved MUC presence-update handling timed out"
        );
        OrderedRelayNackReason::MaybeCommitted
    })? {
        Some(frames) => remote_replies_from_frames(frames),
        None => Err(OrderedRelayNackReason::TargetUnavailable),
    }
}

pub(in super::super::super) async fn deliver_reserved_muc_leave(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    presence: &xmpp_parsers::presence::Presence,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    let Some(sender_jid) = presence
        .from
        .as_ref()
        .and_then(|jid| jid.clone().try_into_full().ok())
    else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    let Some(to) = presence.to.as_ref() else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    let Some(nick) = to.resource().map(|resource| resource.as_str()) else {
        return Err(OrderedRelayNackReason::ParseFailure);
    };
    if to.to_bare() != *room_jid {
        return Err(OrderedRelayNackReason::ParseFailure);
    }

    let frames = tokio::time::timeout(
        ORDERED_RECEIVER_DELIVERY_TIMEOUT,
        crate::server::routes::websocket::handlers::presence::handle_muc_leave(
            state,
            room_jid,
            &sender_jid,
            nick,
            None,
        ),
    )
    .await
    .map_err(|_| {
        tracing::warn!(
            room = %room_jid,
            timeout_ms = ORDERED_RECEIVER_DELIVERY_TIMEOUT.as_millis(),
            "ordered relay: reserved MUC leave handling timed out"
        );
        OrderedRelayNackReason::MaybeCommitted
    })?;
    remote_replies_from_frames(frames)
}

pub(in super::super::super) async fn deliver_reserved_muc_groupchat(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    message: &xmpp_parsers::message::Message,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    let synthetic_session = message
        .from
        .as_ref()
        .and_then(|jid| jid.clone().try_into_full().ok())
        .map(|sender| synthetic_session_for_full_jid(&sender));
    let sender_entity = room_entity(room_jid);
    let deps = build_interpret_deps(
        state,
        synthetic_session
            .as_ref()
            .map(crate::server::routes::websocket::ResolvedPrincipal::from_authenticated_session),
    )
    .with_ordered_relay_origin(Some(OrderedRelayRouteOrigin {
        kind: OrderedRelayRouteOriginKind::Entity(sender_entity.clone()),
        sender_entity,
        inbound_sequence: 0,
        handoff: None,
    }));
    let outcome = tokio::time::timeout(
        ORDERED_RECEIVER_DELIVERY_TIMEOUT,
        crate::server::routes::interpret::dispatch_muc_to_room_for_relay(
            &deps,
            room_jid.clone(),
            message.clone(),
        ),
    )
    .await
    .map_err(|_| OrderedRelayNackReason::MaybeCommitted)?;
    Ok(outcome
        .frames
        .into_iter()
        .filter_map(|frame| {
            match super::super::super::super::codec::decode_stanza(frame.as_str()) {
                Ok(stanza) => Some(RemoteStanza(stanza)),
                Err(error) => {
                    tracing::warn!(
                        room = %room_jid,
                        %error,
                        "ordered relay: MUC groupchat reply frame was not a stanza"
                    );
                    None
                }
            }
        })
        .collect())
}

pub(in super::super::super) fn muc_proxy_result_to_outcome(
    result: Result<Vec<RemoteStanza>, OrderedRelayNackReason>,
) -> RemoteDeliveryOutcome {
    match result {
        Ok(replies) => RemoteDeliveryOutcome {
            delivery: FullJidDeliveryOutcome::Delivered,
            client_replies: replies.into_iter().map(|reply| reply.0).collect(),
            maybe_committed: false,
            join_repair_allowed: false,
            relay_target: None,
            target_claim: None,
        },
        Err(OrderedRelayNackReason::MaybeCommitted) => {
            no_client_reply_outcome_with_commit_state(FullJidDeliveryOutcome::Dropped, true)
        }
        Err(OrderedRelayNackReason::TargetUnavailable) => {
            no_client_reply_outcome(FullJidDeliveryOutcome::Unavailable)
        }
        Err(_) => no_client_reply_outcome(FullJidDeliveryOutcome::Dropped),
    }
}

pub(in super::super::super) fn muc_proxy_result_to_ordered_outcome(
    kind: OrderedRelayMucProxyKind,
    result: Result<Vec<RemoteStanza>, OrderedRelayNackReason>,
) -> OrderedRelayMucProxyOutcome {
    match result {
        Ok(replies) => OrderedRelayMucProxyOutcome::Delivered(
            replies.into_iter().map(|reply| reply.0).collect(),
        ),
        Err(OrderedRelayNackReason::TargetUnavailable) => OrderedRelayMucProxyOutcome::Unavailable,
        Err(OrderedRelayNackReason::MaybeCommitted)
            if kind == OrderedRelayMucProxyKind::JoinPresence =>
        {
            OrderedRelayMucProxyOutcome::JoinMaybeCommitted
        }
        Err(OrderedRelayNackReason::MaybeCommitted) => OrderedRelayMucProxyOutcome::MaybeCommitted,
        Err(_) => OrderedRelayMucProxyOutcome::Dropped,
    }
}
