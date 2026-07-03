use super::*;

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
        .is_blocked_jid(&target.to_bare(), &Jid::from(sender_jid.clone()))
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
