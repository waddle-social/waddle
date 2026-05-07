use super::*;

pub(super) async fn handle_sans_io_iq(
    iq: &xmpp_parsers::iq::Iq,
    id: &str,
    payload_ns: &str,
    domain: &str,
    state: &WebSocketState,
    authenticated_session: &Option<Session>,
    phase: &ConnectionPhase,
    conn_state: &mut IqConnState<'_>,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    // Sans-I/O dispatch: if the IQ namespace has a registered handler in
    // the protocol dispatcher, route through it and translate the emitted
    // OutboundEvents into outbound XML frames via `interpret()`.
    //
    // Handlers that still need async I/O (for example MAM, Jingle, disco,
    // and any other namespaces not yet registered with the dispatcher)
    // continue to fall through to the legacy string-matching branches
    // below until the two-phase async callback machinery lands.
    let carbons_toggle = match &iq.payload {
        xmpp_parsers::iq::IqType::Set(e)
            if e.ns() == CARBONS_NS && (e.name() == "enable" || e.name() == "disable") =>
        {
            Some(e.name() == "enable")
        }
        _ => None,
    };
    if state.deps.protocol.dispatcher.has_iq_handler(payload_ns) {
        if payload_ns == waddle_xmpp::xep::NS_VERSION && !is_version_query(&iq) {
            return vec![build_iq_error_xml_typed(
                &id,
                response_from,
                response_to,
                bad_request_iq_error("Malformed IQ payload."),
            )];
        }
        if payload_ns == waddle_xmpp::xep::NS_VERSION
            && iq
                .to
                .as_ref()
                .is_some_and(|target| target.to_bare().as_str() != domain)
        {
            return vec![build_iq_error_xml_typed(
                &id,
                response_from,
                response_to,
                service_unavailable_iq_error("Service unavailable at this address."),
            )];
        }
        if payload_ns == waddle_xmpp::xep::NS_TIME {
            if !is_time_query(&iq) {
                return vec![build_iq_error_xml_typed(
                    &id,
                    response_from,
                    response_to,
                    bad_request_iq_error("Malformed IQ payload."),
                )];
            }
            if iq
                .to
                .as_ref()
                .is_some_and(|target| target.to_bare().as_str() != domain)
            {
                return vec![build_iq_error_xml_typed(
                    &id,
                    response_from,
                    response_to,
                    service_unavailable_iq_error("Service unavailable at this address."),
                )];
            }
        }
        let Some(full_jid) = phase.bound_jid() else {
            return vec![build_iq_error_xml_typed(
                &id,
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
        let ctx = ProtocolStanzaContext { domain, full_jid };
        let events = state.deps.protocol.dispatcher.dispatch_iq(&iq, &ctx);
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
