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
        let endpoint = enable
            .options
            .iter()
            .find(|(key, _)| key == "endpoint")
            .map(|(_, value)| value.clone());
        let p256dh = enable
            .options
            .iter()
            .find(|(key, _)| key == "p256dh")
            .map(|(_, value)| value.clone());
        let auth_key = enable
            .options
            .iter()
            .find(|(key, _)| key == "auth")
            .map(|(_, value)| value.clone());

        if !endpoint.as_deref().is_some_and(valid_push_endpoint)
            || p256dh.is_none()
            || auth_key.is_none()
        {
            return vec![build_iq_error_xml_typed(
                &iq.id,
                response_from,
                response_to,
                bad_request_iq_error("Malformed IQ payload."),
            )];
        }

        let subscription = waddle_xmpp::push::PushSubscription {
            user_jid: bare_jid.clone(),
            service_jid: enable.jid,
            node: enable.node,
            endpoint,
            p256dh,
            auth_key,
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
    if let Err(error) = state
        .deps
        .protocol
        .push_store
        .remove(&bare_jid, &disable.jid, disable.node.as_deref())
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

fn valid_push_endpoint(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    if url.scheme() != "https" {
        return false;
    }
    match url.host() {
        Some(Host::Ipv4(addr)) => {
            !(addr.is_private()
                || addr.is_loopback()
                || addr.is_link_local()
                || addr.is_multicast()
                || addr.is_broadcast()
                || addr.is_unspecified())
        }
        Some(Host::Ipv6(addr)) => {
            !(addr.is_loopback()
                || addr.is_unspecified()
                || addr.is_unique_local()
                || addr.is_unicast_link_local()
                || addr.is_multicast())
        }
        Some(Host::Domain(host)) => {
            let host = host.trim_end_matches('.').to_ascii_lowercase();
            matches!(
                host.as_str(),
                "updates.push.services.mozilla.com"
                    | "fcm.googleapis.com"
                    | "android.googleapis.com"
                    | "web.push.apple.com"
            ) || host.ends_with(".notify.windows.com")
        }
        None => false,
    }
}
