use super::*;

/// Owner-side receipts retained until the relay reply is accepted by its sender.
pub(crate) struct RelayFrameCompletion {
    pub authority: Arc<crate::ingress::IngressAuthority>,
    pub report: crate::ingress::execute::ExecutionReport,
}

pub(in super::super::super) async fn deliver_reserved_muc_proxy(
    services: &OrderedRelayDeliveryServices,
    room_jid: &jid::BareJid,
    kind: OrderedRelayMucProxyKind,
    origin: MucProxyOrigin,
    stanza: &Stanza,
    admission: Option<&crate::ingress::identity::IngressRelayAdmission>,
    completion: &mut Option<RelayFrameCompletion>,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    let Some(state) = services.web_socket_state.upgrade() else {
        tracing::warn!(
            room = %room_jid,
            "ordered relay: WebSocket state is gone; cannot deliver MUC relay payload"
        );
        return Err(OrderedRelayNackReason::Unreachable);
    };
    match (kind, origin, stanza) {
        (
            OrderedRelayMucProxyKind::JoinPresence,
            MucProxyOrigin::Connection(generation),
            Stanza::Presence(presence),
        ) => deliver_reserved_muc_join(state.as_ref(), room_jid, generation, presence).await,
        (
            OrderedRelayMucProxyKind::GroupchatMessage,
            MucProxyOrigin::Server,
            Stanza::Message(message),
        ) => {
            deliver_reserved_muc_groupchat(
                services,
                state.as_ref(),
                room_jid,
                message,
                admission,
                completion,
            )
            .await
        }
        (
            OrderedRelayMucProxyKind::OccupantPresence,
            MucProxyOrigin::Connection(generation),
            Stanza::Presence(presence),
        ) => {
            deliver_reserved_muc_occupant_presence(state.as_ref(), room_jid, generation, presence)
                .await
        }
        (
            OrderedRelayMucProxyKind::MujiJingleIq,
            MucProxyOrigin::Connection(generation),
            Stanza::Iq(iq),
        ) => deliver_reserved_muji_iq(state.as_ref(), room_jid, generation, iq).await,
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
    occupancy_session: waddle_xmpp_core::OccupancySessionGeneration,
    iq: &xmpp_parsers::iq::Iq,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    let frames = tokio::time::timeout(
        ORDERED_RECEIVER_DELIVERY_TIMEOUT,
        crate::server::routes::websocket::handlers::iq::jingle_muji_relay::handle_relayed_muji_initiate(
            state, iq, occupancy_session,
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
    occupancy_session: waddle_xmpp_core::OccupancySessionGeneration,
    presence: &xmpp_parsers::presence::Presence,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    if presence.type_ == xmpp_parsers::presence::Type::Unavailable {
        return deliver_reserved_muc_leave(state, room_jid, occupancy_session, presence).await;
    }
    deliver_reserved_muc_update(state, room_jid, occupancy_session, presence).await
}

pub(in super::super::super) async fn deliver_reserved_muc_join(
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    occupancy_session: waddle_xmpp_core::OccupancySessionGeneration,
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
            crate::server::routes::websocket::handlers::presence::MucJoinConnectionContext {
                occupancy_session,
                authenticated_session: &Some(synthetic_session),
                registry_owner: None,
            },
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
    occupancy_session: waddle_xmpp_core::OccupancySessionGeneration,
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
            occupancy_session,
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
    occupancy_session: waddle_xmpp_core::OccupancySessionGeneration,
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
            occupancy_session,
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
    services: &OrderedRelayDeliveryServices,
    state: &WebSocketState,
    room_jid: &jid::BareJid,
    message: &xmpp_parsers::message::Message,
    admission: Option<&crate::ingress::identity::IngressRelayAdmission>,
    completion: &mut Option<RelayFrameCompletion>,
) -> Result<Vec<RemoteStanza>, OrderedRelayNackReason> {
    use crate::ingress::{ImmediateSink, IngressStreamIdentity, IngressSubmission};
    use waddle_xmpp::ingress::{ConnectionGeneration, DigestContext, NormalizedTarget};
    let admission = admission.ok_or(OrderedRelayNackReason::ParseFailure)?;
    let sender = message
        .from
        .as_ref()
        .and_then(|jid| jid.try_as_full().ok())
        .ok_or(OrderedRelayNackReason::ParseFailure)?;
    if sender.to_bare() != admission.canonical.sender_bare
        || sender.to_bare() != *admission.principal.bare_jid()
    {
        return Err(OrderedRelayNackReason::ParseFailure);
    }
    let synthetic_session = synthetic_session_for_full_jid(sender);
    let sender_entity = room_entity(room_jid);
    let deps = build_interpret_deps(
        state,
        Some(
            crate::server::routes::websocket::ResolvedPrincipal::from_authenticated_session(
                &synthetic_session,
            ),
        ),
    )
    .with_ordered_relay_origin(Some(OrderedRelayRouteOrigin {
        kind: OrderedRelayRouteOriginKind::Entity(sender_entity.clone()),
        sender_entity: sender_entity.clone(),
        inbound_sequence: 0,
        handoff: None,
    }));
    let authority = &state.deps.protocol.ingress;
    let decision = tokio::time::timeout(ORDERED_RECEIVER_DELIVERY_TIMEOUT, async {
        let claim = current_claim(services, &sender_entity)
            .await
            .ok_or(OrderedRelayNackReason::TargetUnavailable)?;
        if !claim.owner_lease_fresh || claim.owner != services.node_identity.current() {
            return Err(OrderedRelayNackReason::TargetUnavailable);
        }
        let room_fence = waddle_xmpp::muc::RoomClaimFenceContext::new(
            sender_entity.clone(),
            claim.owner,
            claim.claim_epoch,
        );
        let target = NormalizedTarget::Bare(room_jid.clone());
        let digest_input = crate::ingress::submission::digest_input(
            message,
            &DigestContext {
                target: target.clone(),
                server_authorities: vec![sender.to_bare(), room_jid.clone()],
                stanza_lang: admission.stanza_lang.clone(),
            },
        )
        .map_err(|_| OrderedRelayNackReason::ParseFailure)?;
        let plan = crate::server::routes::interpret::plan_muc_for_relay(
            &deps,
            room_jid.clone(),
            message.clone(),
        )
        .await;
        let submission = IngressSubmission {
            identity: IngressStreamIdentity::Relayed {
                canonical: admission.canonical.clone(),
                room: room_jid.clone(),
                room_fence,
            },
            principal: admission.principal.clone(),
            sender: sender.clone(),
            target,
            plan,
            digest_input,
            connection_generation: ConnectionGeneration::INITIAL,
        };
        Ok(authority.commit(&submission).await)
    })
    .await
    .map_err(|_| OrderedRelayNackReason::MaybeCommitted)??;
    if !decision.class.advances() {
        return Err(OrderedRelayNackReason::MaybeCommitted);
    }
    // This budget is independent from admission: a timeout cannot undo commit.
    let report = authority.execute(&decision, &ImmediateSink, &deps).await;
    let replies = report
        .frame_obligations
        .iter()
        .flat_map(|obligation| obligation.frames.iter().cloned())
        .map(RemoteStanza)
        .collect();
    *completion = Some(RelayFrameCompletion {
        authority: Arc::clone(authority),
        report,
    });
    Ok(replies)
}

pub(in super::super::super) fn muc_proxy_result_to_outcome(
    result: Result<Vec<RemoteStanza>, OrderedRelayNackReason>,
) -> RemoteDeliveryOutcome {
    match result {
        Ok(replies) => RemoteDeliveryOutcome {
            frame_completion: None,
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
