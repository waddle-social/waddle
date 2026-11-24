use super::errors::resource_constraint_iq_error;
use super::{
    bad_request_iq_error, build_iq_error_xml_typed, build_push_disable_result,
    build_push_enable_result, internal_server_error_iq_error, iq_to_xml, is_push_enable,
    not_authorized_iq_error, parse_push_disable, parse_push_enable, push_service_stanza_error,
    service_unavailable_iq_error, warn, FullJid, WebSocketState,
};
use crate::destination::UnservedDestination;
use crate::notification_outbox::PushServiceNodeName;
use jid::Jid;
use waddle_xmpp::push::PushSubscription;

/// How many remote Push Service registrations one account may hold: every
/// registration is a publication to another server for each notification.
const MAX_REMOTE_REGISTRATIONS: usize = 16;

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
            iq.id(),
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
                iq.id(),
                response_from,
                response_to,
                bad_request_iq_error("Malformed IQ payload."),
            )];
        };
        if enable.publish_options.is_invalid() {
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                bad_request_iq_error(
                    "XEP-0357 publish-options form requires FORM_TYPE='http://jabber.org/protocol/pubsub#publish-options'.",
                ),
            )];
        }
        let service_jid = enable.jid.to_string();
        if service_jid == push_domain {
            let Some(node) = enable.node.as_deref() else {
                return vec![build_iq_error_xml_typed(
                    iq.id(),
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
                    enable.publish_options.as_element(),
                )
                .await
            {
                return vec![build_iq_error_xml_typed(
                    iq.id(),
                    response_from,
                    response_to,
                    push_service_stanza_error(error),
                )];
            }
            return vec![iq_to_xml(build_push_enable_result(iq))];
        }
        return enable_remote_service(iq, state, &bare_jid_s, enable, response_from, response_to)
            .await;
    }

    let Some(disable) = parse_push_disable(iq) else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
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
                iq.id(),
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
            iq.id(),
            response_from,
            response_to,
            internal_server_error_iq_error("Internal server error."),
        )];
    }
    vec![iq_to_xml(build_push_disable_result(iq))]
}

/// Enable a Push Service of another server, such as the app server of a
/// third-party client (XEP-0357 §2, §4). The registration is kept like a
/// first-party one; notifications reach the service over S2S.
async fn enable_remote_service(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    user: &str,
    enable: waddle_xmpp::xep::xep0357::PushEnable,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    let refuse = |error| {
        vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            error,
        )]
    };
    let service = Jid::from(enable.jid.clone());
    let remote = state
        .deps
        .service_domains
        .served_domains(state.deps.auth_state.xmpp_domain.as_str())
        .and_then(|served| served.classify(Some(&service)).unserved());
    if !matches!(remote, Some(UnservedDestination::Remote(_)))
        || !state.deps.remote_route.supports_client_federation()
    {
        return refuse(service_unavailable_iq_error(
            "This server cannot publish to that Push Service.",
        ));
    }
    let Some(node) = enable
        .node
        .as_deref()
        .and_then(|node| PushServiceNodeName::try_new(node).ok())
        .filter(|node| node.as_str().len() <= REMOTE_NODE_MAX_BYTES)
    else {
        return refuse(bad_request_iq_error(
            "A Push Service enable requires a node of at most 1023 bytes.",
        ));
    };
    let service_jid = enable.jid.to_string();
    let existing = match state.deps.protocol.push_store.get_for_user(user).await {
        Ok(existing) => existing,
        Err(error) => {
            warn!(user = %user, error = %error, "Failed to read push registrations");
            return refuse(internal_server_error_iq_error("Internal server error."));
        }
    };
    let held = existing
        .iter()
        .filter(|held| held.service_jid != state.deps.service_domains.push.as_str())
        .filter(|held| {
            !(held.service_jid == service_jid && held.node.as_deref() == Some(node.as_str()))
        })
        .count();
    if held >= MAX_REMOTE_REGISTRATIONS {
        return refuse(resource_constraint_iq_error(
            "Too many Push Services are enabled.",
        ));
    }
    // "If the combination of JID and node has already been enabled, then
    // the server SHOULD use the last received request for any publish
    // options": the registration replaces the earlier one.
    if let Err(error) = state
        .deps
        .protocol
        .push_store
        .register(PushSubscription {
            user_jid: user.to_owned(),
            service_jid,
            node: Some(node.as_str().to_owned()),
            publish_options: enable.publish_options.as_element().cloned(),
            endpoint: None,
            p256dh: None,
            auth_key: None,
        })
        .await
    {
        warn!(user = %user, error = %error, "Failed to store push registration");
        return refuse(internal_server_error_iq_error("Internal server error."));
    }
    vec![iq_to_xml(build_push_enable_result(iq))]
}

/// The longest node a remote registration may name, in bytes.
const REMOTE_NODE_MAX_BYTES: usize = 1023;
