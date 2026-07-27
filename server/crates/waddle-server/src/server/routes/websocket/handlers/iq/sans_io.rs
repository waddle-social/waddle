use super::*;

fn events_contain_iq_error(events: &[waddle_xmpp::protocol::OutboundEvent]) -> bool {
    use waddle_xmpp::protocol::OutboundEvent;
    use waddle_xmpp::Stanza;
    events.iter().any(|event| {
        let OutboundEvent::SendStanza(stanza) = event else {
            return false;
        };
        matches!(
            stanza.as_ref(),
            Stanza::Iq(iq) if matches!(iq.as_ref(), xmpp_parsers::iq::Iq::Error { .. })
        )
    })
}

/// Dispatch an IQ whose payload namespace has a registered handler in
/// the protocol dispatcher.
///
/// Returns `Some(frames)` when a registered handler owned the IQ —
/// even when `frames` is empty. An empty `Some` is a legitimate,
/// terminal outcome: a handler may forward the stanza to a peer
/// (`OutboundEvent::RouteToConnection`) and produce no synchronous
/// frame for the sender (e.g. XEP-0166 Jingle 1:1 `session-initiate`,
/// which the peer's client answers). The caller MUST treat `Some(_)`
/// as final and NOT fall through to the unhandled-IQ branch, otherwise
/// a successfully-forwarded call IQ is wrongly answered with
/// `feature-not-implemented`.
///
/// Returns `None` only when no registered handler claims the
/// namespace, so the caller can continue to the remaining (disco,
/// misc, MUC, pubsub) branches.
pub(super) async fn handle_sans_io_iq(
    ctx: IqHandlerContext<'_>,
    state: &WebSocketState,
    authenticated_session: &Option<Session>,
    phase: &ConnectionPhase,
    conn_state: &mut IqConnState<'_>,
) -> Option<Vec<String>> {
    let iq = ctx.iq;
    let id = ctx.id;
    let payload_ns = ctx.payload_ns;
    let domain = ctx.domain;
    let response_from = ctx.response_from;
    let response_to = ctx.response_to;

    // Sans-I/O dispatch: if the IQ namespace has a registered handler in
    // the protocol dispatcher, route through it and translate the emitted
    // OutboundEvents into outbound XML frames via `interpret()`.
    //
    // Handlers that still need async I/O (for example MAM, Jingle, disco,
    // and any other namespaces not yet registered with the dispatcher)
    // continue to fall through to the legacy string-matching branches
    // below until the two-phase async callback machinery lands.
    let carbons_toggle = match iq {
        xmpp_parsers::iq::Iq::Set { payload: e, .. }
            if e.ns() == CARBONS_NS && (e.name() == "enable" || e.name() == "disable") =>
        {
            Some(e.name() == "enable")
        }
        _ => None,
    };
    if state.deps.protocol.dispatcher.has_iq_handler(payload_ns) {
        if payload_ns == waddle_xmpp::xep::NS_VERSION && !is_version_query(iq) {
            return Some(vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                bad_request_iq_error("Malformed IQ payload."),
            )]);
        }
        if payload_ns == waddle_xmpp::xep::NS_VERSION
            && iq
                .to()
                .is_some_and(|target| target.to_bare().as_str() != domain)
        {
            return Some(vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                service_unavailable_iq_error("Service unavailable at this address."),
            )]);
        }
        if payload_ns == waddle_xmpp::xep::NS_TIME {
            if !is_time_query(iq) {
                return Some(vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    bad_request_iq_error("Malformed IQ payload."),
                )]);
            }
            if iq
                .to()
                .is_some_and(|target| target.to_bare().as_str() != domain)
            {
                return Some(vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    service_unavailable_iq_error("Service unavailable at this address."),
                )]);
            }
        }
        let Some(full_jid) = phase.bound_jid() else {
            return Some(vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                not_authorized_iq_error("Authentication required."),
            )]);
        };
        if let Some(enabled) = carbons_toggle {
            *conn_state.carbons_enabled = enabled;
            let _ = state
                .deps
                .protocol
                .connection_registry
                .set_carbons_enabled(full_jid, enabled);
            if let Some(owner) = conn_state.registry_owner {
                mirror_remote_carbons_update(state, full_jid, owner, enabled).await;
            }
        }
        // XMPP-native MUC-call membership gate (XEP-0272 Muji): a
        // session may only mint a LiveKit JWT for a room it has
        // actually joined through the XEP-0045 path. The pure
        // dispatcher in `waddle-xmpp` has no handle to the room
        // registry, so the check has to happen here before
        // dispatch. The gate triggers on the Jingle namespace and
        // only when the embedded `<jingle/>` carries a `<muji/>`
        // child — 1:1 Jingle traffic passes through untouched.
        let mut media_capabilities = None;
        if payload_ns == waddle_xmpp::xep::xep0166::NS_JINGLE {
            match super::jingle_muji_gate::verify_muji_jingle_request(state, full_jid, iq).await {
                super::jingle_muji_gate::GateOutcome::Allow {
                    media_capabilities: gate_capabilities,
                } => {
                    // Authorization produced the grant: the Jingle
                    // handler's Muji mint consumes exactly the
                    // capabilities the gate derived from the sender's
                    // current XEP-0045 role.
                    media_capabilities = gate_capabilities;
                }
                super::jingle_muji_gate::GateOutcome::Deny(stanza_error) => {
                    return Some(vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        *stanza_error,
                    )]);
                }
                super::jingle_muji_gate::GateOutcome::RoomNotLocal { room_jid } => {
                    // #1445: no room actor in this process. On a
                    // clustered deployment the room may be alive on the
                    // claim-owning node — relay the session-initiate
                    // there instead of denying a join for a room that
                    // exists.
                    let reply = IqReplyAddressing {
                        id,
                        response_from,
                        response_to,
                    };
                    return Some(
                        relay_muji_initiate_or_deny(
                            state, conn_state, full_jid, iq, &room_jid, reply,
                        )
                        .await,
                    );
                }
            }
        }
        let ctx = ProtocolStanzaContext {
            domain,
            full_jid,
            media_capabilities,
        };
        let muji_terminate_room = super::jingle_muji_gate::muji_session_terminate_room(iq);
        let events = state.deps.protocol.dispatcher.dispatch_iq(iq, &ctx);
        let muji_clear_after = muji_terminate_room.filter(|_| !events_contain_iq_error(&events));
        let deps = crate::server::routes::interpret::Deps {
            connection_registry: &state.deps.protocol.connection_registry,
            user_registry: Some(&state.deps.protocol.user_registry),
            sm_session_registry: Some(&state.deps.protocol.sm_session_registry),
            mam_storage: Some(&state.deps.protocol.mam_storage),
            inbox_storage: Some(&state.deps.protocol.inbox_storage),
            extension_manager: Some(&state.deps.protocol.extension_manager),
            room_registry: Some(&state.deps.protocol.room_registry),
            web_socket_state: Some(state),
            authenticated_session: authenticated_session.as_ref(),
            local_domain: state.deps.auth_state.xmpp_domain.as_str(),
            blocking_storage: Some(&state.deps.protocol.blocking_storage),
            message_dispatcher: Some(&state.deps.protocol.dispatcher),
            pending_delivery_storage: Some(&state.deps.protocol.pending_delivery_storage),
            ordered_relay_origin: conn_state.ordered_relay_origin.clone(),
        };
        let outcome = crate::server::routes::interpret::interpret(events, &deps).await;
        if let Some(room_jid) = muji_clear_after {
            crate::server::routes::muc_muji_clear::clear_muji_presence_for_departure(
                state, &room_jid, full_jid,
            )
            .await;
        }
        if outcome.close {
            warn!(
                ns = %payload_ns,
                "Sans-I/O handler requested transport close; \
                 WebSocket adapter cannot honour CloseTransport yet"
            );
        }
        // A registered handler owned this IQ. Empty frames are valid
        // and terminal (e.g. a Jingle 1:1 stanza forwarded to the peer
        // with no synchronous reply for the sender) — return `Some` so
        // the caller does NOT fall through to the unhandled-IQ error.
        return Some(outcome.frames);
    }
    None
}

/// Resolve a Muji `session-initiate` whose room has no local actor
/// (#1445): relay it to the room's claim-owning node over the ordered
/// MUC proxy — the owner runs the gate + mint where the occupancy
/// lives, and its replies (the IQ ack and the server-initiated
/// `session-accept` carrying the LiveKit token) ride back on the relay
/// ACK to be written to this socket. Outcome mapping:
/// - `Delivered` → the owner's reply frames, verbatim.
/// - `RoomUnclaimed` → no node owns the room, so it genuinely does not
///   exist: the terminal `room_not_found` denial (same wire shape and
///   telemetry as ever).
/// - everything else (claim unavailable, origin unavailable, relay
///   dropped/maybe-committed, claim-says-local race) → a `type='wait'`
///   internal-server-error so the client retries; a duplicate mint on
///   retry is harmless (set-insert registration, superseding token).
#[cfg(feature = "clustering")]
async fn relay_muji_initiate_or_deny(
    state: &WebSocketState,
    conn_state: &IqConnState<'_>,
    full_jid: &FullJid,
    iq: &xmpp_parsers::iq::Iq,
    room_jid: &jid::BareJid,
    reply: IqReplyAddressing<'_>,
) -> Vec<String> {
    use crate::clustering::ordered_relay::OrderedRelayMucProxyKind;
    use crate::clustering::route_bridge::{MucProxyRouteDecision, OrderedRelayMucProxyOutcome};

    let deny = || {
        vec![build_iq_error_xml_typed(
            reply.id,
            reply.response_from,
            reply.response_to,
            *super::jingle_muji_gate::deny_room_not_found(room_jid, &full_jid.to_bare()),
        )]
    };
    let retry_later = || {
        vec![build_iq_error_xml_typed(
            reply.id,
            reply.response_from,
            reply.response_to,
            internal_server_error_iq_error(
                "the requested room is owned by another node and could not be reached; \
                 please retry",
            ),
        )]
    };

    let bridge = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref();
    let (Some(bridge), Some(origin)) = (bridge, conn_state.ordered_relay_origin.as_ref()) else {
        // No relay substrate (clustering disabled at runtime): local
        // absence is definitive, exactly the pre-#1445 semantics.
        return deny();
    };
    // Stamp the authenticated full JID as `from` before relaying:
    // clients legitimately omit `from` (the server derives the sender
    // from the bound session), but the envelope's sender-claim
    // validation and the owner-side executor both read the stanza's
    // `from`. Unconditional overwrite — a client-supplied `from` is
    // never trusted.
    let mut relayed = iq.clone();
    let stamped_from = Some(jid::Jid::from(full_jid.clone()));
    match &mut relayed {
        xmpp_parsers::iq::Iq::Get { from, .. }
        | xmpp_parsers::iq::Iq::Set { from, .. }
        | xmpp_parsers::iq::Iq::Result { from, .. }
        | xmpp_parsers::iq::Iq::Error { from, .. } => *from = stamped_from,
    }
    let stanza = waddle_xmpp::Stanza::Iq(Box::new(relayed));
    match bridge
        .try_proxy_muc_remote_decision(
            room_jid,
            &stanza,
            OrderedRelayMucProxyKind::MujiJingleIq,
            origin,
        )
        .await
    {
        MucProxyRouteDecision::Attempted(OrderedRelayMucProxyOutcome::Delivered(replies)) => {
            replies
                .iter()
                .map(crate::server::routes::websocket::transport_xml::stanza_to_xml)
                .collect()
        }
        MucProxyRouteDecision::RoomUnclaimed => deny(),
        MucProxyRouteDecision::Attempted(
            OrderedRelayMucProxyOutcome::Unavailable
            | OrderedRelayMucProxyOutcome::Dropped
            | OrderedRelayMucProxyOutcome::MaybeCommitted
            | OrderedRelayMucProxyOutcome::JoinMaybeCommitted,
        )
        | MucProxyRouteDecision::LocalRoom
        | MucProxyRouteDecision::RoomClaimUnavailable
        | MucProxyRouteDecision::OriginUnavailable => retry_later(),
    }
}

/// Non-clustering builds have no other replica that could own the
/// room, so local absence is definitive — the terminal denial,
/// byte-identical to the pre-#1445 wire behavior.
#[cfg(not(feature = "clustering"))]
async fn relay_muji_initiate_or_deny(
    _state: &WebSocketState,
    _conn_state: &IqConnState<'_>,
    full_jid: &FullJid,
    _iq: &xmpp_parsers::iq::Iq,
    room_jid: &jid::BareJid,
    reply: IqReplyAddressing<'_>,
) -> Vec<String> {
    vec![build_iq_error_xml_typed(
        reply.id,
        reply.response_from,
        reply.response_to,
        *super::jingle_muji_gate::deny_room_not_found(room_jid, &full_jid.to_bare()),
    )]
}

/// The (id, from, to) triple every IQ error reply is stamped with —
/// bundled so the relay helper's signature stays readable.
struct IqReplyAddressing<'a> {
    id: &'a str,
    response_from: Option<&'a str>,
    response_to: Option<&'a str>,
}

#[cfg(feature = "clustering")]
async fn mirror_remote_carbons_update(
    state: &WebSocketState,
    jid: &FullJid,
    owner: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    enabled: bool,
) {
    if let Some(bridge) = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
    {
        bridge
            .update_remote_user_resource_if_owner(
                jid,
                owner,
                crate::clustering::route_bridge::RemoteResourceStateUpdate::Carbons { enabled },
            )
            .await;
    }
}

#[cfg(not(feature = "clustering"))]
async fn mirror_remote_carbons_update(
    _state: &WebSocketState,
    _jid: &FullJid,
    _owner: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    _enabled: bool,
) {
}
