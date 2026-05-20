use super::*;

pub(super) async fn handle_muc_self_ping_iq(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            not_authorized_iq_error("Authentication required."),
        )];
    };
    let Some(target) = iq.to().and_then(|jid| jid.clone().try_into_full().ok()) else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            bad_request_iq_error("Malformed IQ payload."),
        )];
    };
    let room_jid = target.to_bare();
    let nick = target.resource().to_string();
    let Some(room_actor) = get_room_actor(state, &room_jid).await else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            item_not_found_iq_error("Requested item not found."),
        )];
    };
    match room_actor
        .ask(PingSelfCheck {
            nick,
            sender_jid: sender_jid.clone(),
        })
        .await
    {
        Ok(()) => vec![build_iq_result_xml(
            iq.id(),
            response_from,
            response_to,
            None,
        )],
        Err(kameo::error::SendError::HandlerError(_)) => vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            not_acceptable_iq_error("IQ rejected by server policy."),
        )],
        Err(error) => {
            warn!(room = %room_jid, error = ?error, "Failed to process MUC self-ping");
            vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                internal_server_error_iq_error("Internal server error."),
            )]
        }
    }
}

pub(super) async fn handle_isr_token_request_iq(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    authenticated_session: &Option<Session>,
    sender_jid: Option<&FullJid>,
) -> Vec<String> {
    let (Some(session), Some(sender_jid)) = (authenticated_session.as_ref(), sender_jid) else {
        return vec![iq_to_xml(build_isr_token_error(iq, "not-authorized"))];
    };
    let token: IsrToken = state
        .deps
        .protocol
        .isr_token_store
        .create_token(session.user_id.to_string(), sender_jid.to_bare());
    vec![iq_to_xml(build_isr_token_result(iq, &token))]
}

pub(super) async fn route_full_jid_iq(
    mut iq: xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    target: FullJid,
    response_from: Option<&str>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            None,
            not_authorized_iq_error("Authentication required."),
        )];
    };
    let blocking = DatabaseBlockingStorage::new(state.deps.app_state.db_pool.global().clone());
    match blocking
        .is_blocked(&target.to_bare(), &sender_jid.to_bare())
        .await
    {
        Ok(true) => {
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                Some(sender_jid.as_str()),
                service_unavailable_iq_error("Service unavailable at this address."),
            )];
        }
        Ok(false) => {}
        Err(error) => {
            warn!(error = %error, target = %target, sender = %sender_jid, "Failed to check blocklist before routing IQ");
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                Some(sender_jid.as_str()),
                internal_server_error_iq_error("Internal server error."),
            )];
        }
    }
    *iq.from_mut() = Some(Jid::from(sender_jid.clone()));
    *iq.to_mut() = Some(Jid::from(target.clone()));
    match state
        .deps
        .protocol
        .connection_registry
        .send_to(&target, Stanza::Iq(Box::new(iq.clone())))
        .await
    {
        waddle_xmpp::registry::SendResult::Sent => Vec::new(),
        waddle_xmpp::registry::SendResult::NotConnected
        | waddle_xmpp::registry::SendResult::ChannelClosed => {
            let sender = sender_jid.to_string();
            vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                Some(sender.as_str()),
                service_unavailable_iq_error("Service unavailable at this address."),
            )]
        }
    }
}
