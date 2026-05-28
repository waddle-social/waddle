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

pub(super) async fn handle_sans_io_iq(
    ctx: IqHandlerContext<'_>,
    state: &WebSocketState,
    authenticated_session: &Option<Session>,
    phase: &ConnectionPhase,
    conn_state: &mut IqConnState<'_>,
) -> Vec<String> {
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
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                bad_request_iq_error("Malformed IQ payload."),
            )];
        }
        if payload_ns == waddle_xmpp::xep::NS_VERSION
            && iq
                .to()
                .is_some_and(|target| target.to_bare().as_str() != domain)
        {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                service_unavailable_iq_error("Service unavailable at this address."),
            )];
        }
        if payload_ns == waddle_xmpp::xep::NS_TIME {
            if !is_time_query(iq) {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    bad_request_iq_error("Malformed IQ payload."),
                )];
            }
            if iq
                .to()
                .is_some_and(|target| target.to_bare().as_str() != domain)
            {
                return vec![build_iq_error_xml_typed(
                    id,
                    response_from,
                    response_to,
                    service_unavailable_iq_error("Service unavailable at this address."),
                )];
            }
        }
        let Some(full_jid) = phase.bound_jid() else {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                not_authorized_iq_error("Authentication required."),
            )];
        };
        if let Some(enabled) = carbons_toggle {
            *conn_state.carbons_enabled = enabled;
            let _ = state
                .deps
                .protocol
                .connection_registry
                .set_carbons_enabled(full_jid, enabled);
        }
        // XMPP-native MUC-call membership gate (XEP-0272 Muji): a
        // session may only mint a LiveKit JWT for a room it has
        // actually joined through the XEP-0045 path. The pure
        // dispatcher in `waddle-xmpp` has no handle to the room
        // registry, so the check has to happen here before
        // dispatch. The gate triggers on the Jingle namespace and
        // only when the embedded `<jingle/>` carries a `<muji/>`
        // child — 1:1 Jingle traffic passes through untouched.
        if payload_ns == waddle_xmpp::xep::xep0166::NS_JINGLE {
            match super::jingle_muji_gate::verify_muji_jingle_request(state, full_jid, iq).await {
                super::jingle_muji_gate::GateOutcome::Allow => {}
                super::jingle_muji_gate::GateOutcome::Deny(stanza_error) => {
                    return vec![build_iq_error_xml_typed(
                        id,
                        response_from,
                        response_to,
                        *stanza_error,
                    )];
                }
            }
        }
        let ctx = ProtocolStanzaContext { domain, full_jid };
        let muji_terminate_room = super::jingle_muji_gate::muji_session_terminate_room(iq);
        let events = state.deps.protocol.dispatcher.dispatch_iq(iq, &ctx);
        let muji_clear_after = muji_terminate_room.filter(|_| !events_contain_iq_error(&events));
        let deps = crate::server::routes::interpret::Deps {
            connection_registry: &state.deps.protocol.connection_registry,
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
        return outcome.frames;
    }
    Vec::new()
}
