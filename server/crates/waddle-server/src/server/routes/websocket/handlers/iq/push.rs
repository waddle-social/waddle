use super::*;

pub(super) async fn handle_push_iq(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    push_domain: &str,
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
    let bare_jid = sender_jid.to_bare();
    let bare_jid_s = bare_jid.to_string();

    if is_push_enable(iq) {
        let Some(enable) = parse_push_enable(iq) else {
            return vec![build_iq_error_xml_typed(
                &iq.id,
                response_from,
                response_to,
                bad_request_iq_error("Malformed IQ payload."),
            )];
        };
        let service_jid = enable.jid.to_string();
        if service_jid == push_domain {
            let Some(node) = enable.node.as_deref() else {
                return vec![build_iq_error_xml_typed(
                    &iq.id,
                    response_from,
                    response_to,
                    bad_request_iq_error("First-party Push Service enable requires a node."),
                )];
            };
            if let Err(error) = state
                .deps
                .protocol
                .push_service
                .register_first_party_node_for_owner(
                    &bare_jid,
                    &service_jid,
                    node,
                    enable.publish_options.as_ref(),
                )
                .await
            {
                return vec![build_iq_error_xml_typed(
                    &iq.id,
                    response_from,
                    response_to,
                    push_service_stanza_error(error),
                )];
            }
            return vec![iq_to_xml(build_push_enable_result(iq))];
        }
        return vec![build_iq_error_xml_typed(
            &iq.id,
            response_from,
            response_to,
            service_unavailable_iq_error(
                "Only the first-party XMPP Push Service is supported by this server.",
            ),
        )];
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
    if service_jid == push_domain {
        if let Err(error) = state
            .deps
            .protocol
            .push_service
            .remove_registered_nodes_for_owner(&bare_jid, &service_jid, disable.node.as_deref())
            .await
        {
            warn!(user = %bare_jid_s, error = %error, "Failed to atomically disable first-party push subscription");
            return vec![build_iq_error_xml_typed(
                &iq.id,
                response_from,
                response_to,
                internal_server_error_iq_error("Internal server error."),
            )];
        }
        return vec![iq_to_xml(build_push_disable_result(iq))];
    }

    if let Err(error) = state
        .deps
        .protocol
        .push_store
        .remove(&bare_jid_s, &service_jid, disable.node.as_deref())
        .await
    {
        warn!(user = %bare_jid_s, error = %error, "Failed to remove push subscription");
        return vec![build_iq_error_xml_typed(
            &iq.id,
            response_from,
            response_to,
            internal_server_error_iq_error("Internal server error."),
        )];
    }
    vec![iq_to_xml(build_push_disable_result(iq))]
}

fn push_service_stanza_error(error: XmppError) -> xmpp_parsers::stanza_error::StanzaError {
    match error {
        XmppError::Stanza {
            condition: StanzaErrorCondition::BadRequest,
            ..
        } => bad_request_iq_error("Malformed Push Service request."),
        XmppError::Stanza {
            condition: StanzaErrorCondition::ItemNotFound,
            ..
        } => item_not_found_iq_error("Requested Push Service item not found."),
        XmppError::Stanza {
            condition: StanzaErrorCondition::Forbidden,
            ..
        }
        | XmppError::PermissionDenied(_) => forbidden_iq_error("Push Service request forbidden."),
        _ => internal_server_error_iq_error("Internal server error."),
    }
}
