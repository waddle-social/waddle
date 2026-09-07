use super::*;

/// Build the [`crate::server::routes::interpret::Deps`] view a
/// per-connection main loop needs to resolve recipient-pass outbound
/// events. Centralized so the deps shape stays in sync with the IQ
/// flow's callsite — both go through the same `interpret()`.
///
/// `authenticated_principal` is threaded through so the
/// [`OutboundEvent::DispatchToRoom`] bridge arm can preserve the
/// legacy managed-room owner check (announcements room admits server
/// owners only). Without it the dispatcher path's owner override
/// would always fail. The recipient-pass path the main loop drives
/// passes the connection's own session here.
pub(crate) fn build_interpret_deps<'a>(
    state: &'a WebSocketState,
    authenticated_principal: Option<super::ResolvedPrincipal<'a>>,
) -> crate::server::routes::interpret::Deps<'a> {
    crate::server::routes::interpret::Deps {
        connection_registry: &state.deps.protocol.connection_registry,
        user_registry: Some(&state.deps.protocol.user_registry),
        sm_session_registry: Some(&state.deps.protocol.sm_session_registry),
        mam_storage: Some(&state.deps.protocol.mam_storage),
        inbox_storage: Some(&state.deps.protocol.inbox_storage),
        extension_manager: Some(&state.deps.protocol.extension_manager),
        room_registry: Some(&state.deps.protocol.room_registry),
        web_socket_state: Some(state),
        authenticated_principal,
        local_domain: state.deps.auth_state.xmpp_domain.as_str(),
        blocking_storage: Some(&state.deps.protocol.blocking_storage),
        message_dispatcher: Some(&state.deps.protocol.dispatcher),
        pending_delivery_storage: Some(&state.deps.protocol.pending_delivery_storage),
        ordered_relay_origin: None,
        sfu: state.deps.protocol.sfu.as_deref(),
        ingress_effect_capture: None,
        effects: &crate::server::routes::interpret::effects::ImmediateSink,
    }
}
