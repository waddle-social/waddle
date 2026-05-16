use super::*;

pub(super) async fn handle_push_iq(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_typed(
            &iq.id,
            response_from,
            response_to,
            not_authorized_iq_error("Authentication required."),
        )];
    };
    let bare_jid = sender_jid.to_bare().to_string();

    if is_push_enable(iq) {
        let Some(enable) = parse_push_enable(iq) else {
            return vec![build_iq_error_xml_typed(
                &iq.id,
                response_from,
                response_to,
                bad_request_iq_error("Malformed IQ payload."),
            )];
        };
        if enable
            .publish_options
            .as_ref()
            .is_some_and(crate::push_registrations::publish_options_contains_provider_credentials)
        {
            return vec![build_iq_error_xml_typed(
                &iq.id,
                response_from,
                response_to,
                bad_request_iq_error(
                    "Provider push credentials must be registered with the XMPP Push Service.",
                ),
            )];
        }
        let subscription = waddle_xmpp::push::PushSubscription {
            user_jid: bare_jid.clone(),
            service_jid: enable.jid.to_string(),
            node: enable.node,
            publish_options: enable.publish_options,
            endpoint: None,
            p256dh: None,
            auth_key: None,
        };
        if let Err(error) = state.deps.protocol.push_store.register(subscription).await {
            warn!(user = %bare_jid, error = %error, "Failed to register push subscription");
            return vec![build_iq_error_xml_typed(
                &iq.id,
                response_from,
                response_to,
                internal_server_error_iq_error("Internal server error."),
            )];
        }
        return vec![iq_to_xml(build_push_enable_result(iq))];
    }

    let Some(disable) = parse_push_disable(iq) else {
        return vec![build_iq_error_xml_typed(
            &iq.id,
            response_from,
            response_to,
            bad_request_iq_error("Malformed IQ payload."),
        )];
    };
    let service_jid = disable.jid.to_string();
    if let Err(error) = state
        .deps
        .protocol
        .push_store
        .remove(&bare_jid, &service_jid, disable.node.as_deref())
        .await
    {
        warn!(user = %bare_jid, error = %error, "Failed to remove push subscription");
        return vec![build_iq_error_xml_typed(
            &iq.id,
            response_from,
            response_to,
            internal_server_error_iq_error("Internal server error."),
        )];
    }
    vec![iq_to_xml(build_push_disable_result(iq))]
}
